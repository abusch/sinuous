use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use futures::TryStreamExt;
use sonor::{Speaker, SpeakerInfo, Track, TrackInfo, URN};
use std::net::Ipv4Addr;
use tokio::{
    select,
    sync::mpsc::{Receiver, Sender},
};
use tracing::{debug, error, info, warn};

use crate::{Action, Direction, Update, ViewMode};

#[derive(Debug, Clone)]
pub struct FavoritePlaylist {
    pub title: String,
    pub description: String,
    pub uri: String,
    pub metadata: String,
}

#[derive(Debug)]
pub struct SpeakerState {
    pub is_playing: bool,
    pub current_volume: u16,
    pub group_names: Vec<String>,
    pub selected_group: usize,
    pub now_playing: Option<Arc<TrackInfo>>,
    pub queue: Arc<Vec<Track>>,
    pub current_view: ViewMode,
    pub favorites: Vec<FavoritePlaylist>,
    pub selected_favorite: usize,
}

impl SpeakerState {
    pub fn group_name(&self) -> &str {
        &self.group_names[self.selected_group]
    }
}

pub struct SonosService {
    update_tx: Sender<Update>,
    cmd_rx: Receiver<Action>,
    speakers_by_uuid: BTreeMap<String, Speaker>,
    groups: Vec<SpeakerGroup>,
    selected_group: usize,
    current_view: ViewMode,
    favorites: Vec<FavoritePlaylist>,
    selected_favorite: usize,
    // Cached state
    cached_is_playing: bool,
    cached_volume: u16,
    cached_now_playing: Option<Arc<TrackInfo>>,
    cached_queue: Arc<Vec<Track>>,
}

impl SonosService {
    pub fn new(update_tx: Sender<Update>, cmd_rx: Receiver<Action>) -> Self {
        Self {
            update_tx,
            cmd_rx,
            speakers_by_uuid: BTreeMap::new(),
            groups: vec![],
            selected_group: 0,
            current_view: ViewMode::Queue,
            favorites: vec![],
            selected_favorite: 0,
            cached_is_playing: false,
            cached_volume: 0,
            cached_now_playing: None,
            cached_queue: Arc::new(vec![]),
        }
    }

    pub fn start(self, provided_devices: (Vec<Ipv4Addr>, Vec<String>)) {
        tokio::spawn(async move {
            if let Err(err) = self.inner_loop(provided_devices).await {
                error!(%err, "Sonos error");
            }
        });
    }

    async fn inner_loop(mut self, provided_devices: (Vec<Ipv4Addr>, Vec<String>)) -> Result<()> {
        let speakers = get_speakers(provided_devices).await?;

        let mut speakers_by_uuid = BTreeMap::new();
        // TODO do in parallel?
        for s in speakers {
            let uuid = s.uuid().await?;
            speakers_by_uuid.insert(uuid, s);
        }

        // Use the first speaker discovered
        let (_uuid, speaker) = speakers_by_uuid
            .iter()
            .next()
            .context("No speaker discovered!")?;
        let groups = speaker.zone_group_state().await?;
        debug!("Found {} groups", groups.len());

        // Fetch favorites from the speaker (before moving speakers_by_uuid)
        debug!("Fetching favorites...");
        match fetch_favorite_playlists(speaker).await {
            Ok(favs) => {
                info!("Found {} favorite playlists", favs.len());
                self.favorites = favs;
            }
            Err(e) => {
                warn!("Failed to fetch favorites: {}", e);
            }
        }

        let group_list = groups
            .into_iter()
            .map(|(uuid, speaker_list)| SpeakerGroup::new(uuid, speaker_list))
            .collect::<Vec<_>>();
        self.groups = group_list;
        self.speakers_by_uuid = speakers_by_uuid;

        // Initial state fetch
        if let Err(e) = self.refresh_state().await {
            warn!("Failed to fetch initial state: {}", e);
        }

        let mut ticker = tokio::time::interval(Duration::from_secs(1));
        debug!("Starting sonos loop");

        loop {
            select! {
                _tick = ticker.tick() => {
                    // time to refresh our state
                    if let Err(e) = self.refresh_state().await {
                        warn!("Failed to refresh state: {}", e);
                    }
                    self.send_update().await;
                }
                cmd = self.cmd_rx.recv() => {
                    if let Some(c) = cmd {
                        let mut needs_refresh = false;

                        // Process the first command
                        match self.handle_command(c).await {
                            Ok(r) => if r { needs_refresh = true; },
                            Err(e) => warn!("Error handling command: {}", e),
                        }

                        // Drain pending commands
                        while let Ok(c) = self.cmd_rx.try_recv() {
                            match self.handle_command(c).await {
                                Ok(r) => if r { needs_refresh = true; },
                                Err(e) => warn!("Error handling batched command: {}", e),
                            }
                        }

                        if needs_refresh && let Err(e) = self.refresh_state().await {
                            warn!("Failed to refresh state after commands: {}", e);
                        }
                    } else {
                        warn!("Command channel was closed: exiting...");
                        break;
                    }
                    self.send_update().await;
                }
            }
        }
        Ok(())
    }

    async fn handle_command(&mut self, cmd: Action) -> Result<bool> {
        debug!(?cmd, "Handling command");
        match cmd {
            // Playback controls
            Action::Play => {
                let speaker = self.current_speaker().context("No selected group")?;
                speaker.play().await?;
                Ok(true)
            }
            Action::Pause => {
                let speaker = self.current_speaker().context("No selected group")?;
                speaker.pause().await?;
                Ok(true)
            }
            Action::Next => {
                let speaker = self.current_speaker().context("No selected group")?;
                speaker.next().await?;
                Ok(true)
            }
            Action::Prev => {
                let speaker = self.current_speaker().context("No selected group")?;
                speaker.previous().await?;
                Ok(true)
            }
            Action::VolAdjust(v) => {
                let speaker = self.current_speaker().context("No selected group")?;
                speaker.set_volume_relative(v).await.map(drop)?;
                Ok(true)
            }

            // Group switching
            Action::NextSpeaker => {
                self.select_next_group();
                Ok::<bool, anyhow::Error>(true)
            }
            Action::PrevSpeaker => {
                self.select_prev_group();
                Ok::<bool, anyhow::Error>(true)
            }

            // View switching
            Action::SwitchView(view_mode) => {
                self.current_view = view_mode;
                Ok(false)
            }

            // Favorites navigation
            Action::NavigateFavorites(direction) => {
                match direction {
                    Direction::Up => {
                        if self.selected_favorite > 0 {
                            self.selected_favorite -= 1;
                        }
                    }
                    Direction::Down => {
                        if self.selected_favorite < self.favorites.len().saturating_sub(1) {
                            self.selected_favorite += 1;
                        }
                    }
                }
                Ok(false)
            }

            // Play favorite
            Action::PlayFavorite(index) => {
                if let Some(favorite) = self.favorites.get(index) {
                    info!("Attempting to play favorite: {}", favorite.title);
                    debug!("Favorite URI: {}", favorite.uri);

                    let speaker = self.current_speaker().context("No selected group")?;

                    // Clear the queue first
                    debug!("Clearing queue...");
                    if let Err(e) = speaker.clear_queue().await {
                        warn!("Failed to clear queue: {}", e);
                    }

                    // Try different approaches based on URI type
                    let unescaped_uri = html_unescape(&favorite.uri);
                    let unescaped_metadata = html_unescape(&favorite.metadata);

                    debug!("Unescaped URI: {}", unescaped_uri);

                    // For containers (playlists), use AddURIToQueue
                    if unescaped_uri.starts_with("x-rincon-cpcontainer:") {
                        debug!("Using AddURIToQueue for container...");
                        let service = URN::service("schemas-upnp-org", "AVTransport", 1);
                        let payload = format!(
                            r#"<InstanceID>0</InstanceID>
<EnqueuedURI>{}</EnqueuedURI>
<EnqueuedURIMetaData>{}</EnqueuedURIMetaData>
<DesiredFirstTrackNumberEnqueued>0</DesiredFirstTrackNumberEnqueued>
<EnqueueAsNext>1</EnqueueAsNext>"#,
                            favorite.uri, favorite.metadata
                        );

                        match speaker.action(&service, "AddURIToQueue", &payload).await {
                            Ok(_) => {
                                debug!("AddURIToQueue succeeded");
                                // Start playback
                                debug!("Starting playback...");
                                speaker.play().await?;
                                info!("Successfully started playing: {}", favorite.title);
                            }
                            Err(e) => {
                                error!("AddURIToQueue failed: {:?}", e);
                                return Err(e).context("Failed to add playlist to queue");
                            }
                        }
                    } else {
                        // For individual tracks, use queue_next
                        debug!("Using queue_next for track...");
                        speaker
                            .queue_next(&unescaped_uri, &unescaped_metadata)
                            .await?;
                        speaker.next().await?;
                        info!("Successfully started playing: {}", favorite.title);
                    }

                    Ok(true)
                } else {
                    warn!("Invalid favorite index: {}", index);
                    Ok(false)
                }
            }

            Action::Nop => Ok(false),
        }
        .context("Error while handling command")
    }

    async fn refresh_state(&mut self) -> Result<()> {
        let uuid = self
            .groups
            .get(self.selected_group)
            .map(|g| g.coordinator.clone())
            .context("No selected group")?;

        let speaker = self
            .speakers_by_uuid
            .get(&uuid)
            .context("Speaker not found")?
            .clone();

        self.cached_is_playing = speaker.is_playing().await?;
        self.cached_volume = speaker.volume().await?;
        self.cached_now_playing = speaker.track().await?.map(Arc::new);
        self.cached_queue = Arc::new(speaker.queue().await?);
        Ok(())
    }

    async fn send_update(&self) {
        match self.build_state() {
            Ok(speaker_state) => {
                if let Err(err) = self
                    .update_tx
                    .send(Update::NewState(Box::new(speaker_state)))
                    .await
                {
                    warn!(%err, "Updates channel was closed: exiting");
                }
            }
            Err(err) => warn!(%err, "Failed to build state"),
        }
    }

    fn select_prev_group(&mut self) {
        if self.selected_group == 0 {
            self.selected_group = self.groups.len();
        } else {
            self.selected_group -= 1;
        }
    }

    fn select_next_group(&mut self) {
        self.selected_group += 1;
        if self.selected_group >= self.groups.len() {
            self.selected_group = 0;
        }
    }

    fn current_speaker(&self) -> Option<&Speaker> {
        // &self.speakers[self.selected_speaker]
        self.groups
            .get(self.selected_group)
            .and_then(|group| self.speakers_by_uuid.get(&group.coordinator))
    }

    fn build_state(&self) -> Result<SpeakerState> {
        let mut names = vec![];
        for group in &self.groups {
            names.push(group.name());
        }

        Ok(SpeakerState {
            is_playing: self.cached_is_playing,
            current_volume: self.cached_volume,
            group_names: names,
            selected_group: self.selected_group,
            now_playing: self.cached_now_playing.clone(),
            queue: self.cached_queue.clone(),
            current_view: self.current_view,
            favorites: self.favorites.clone(),
            selected_favorite: self.selected_favorite,
        })
    }
}

async fn get_speakers(provided_devices: (Vec<Ipv4Addr>, Vec<String>)) -> Result<Vec<Speaker>> {
    let mut speakers: Vec<Speaker> = vec![];
    debug!("Connecting to provided speakers...");
    for e in &provided_devices.0 {
        if let Some(spk) = sonor::Speaker::from_ip(*e).await.unwrap_or(None) {
            speakers.push(spk);
        } else {
            debug!("Not connecting to {e} due to errors");
        }
    }
    for e in &provided_devices.1 {
        if let Some(device) = sonor::find(e, Duration::from_secs(2)).await? {
            speakers.push(device);
        } else {
            debug!("Not connecting to {e} due to errors");
        }
    }
    if provided_devices.0.is_empty() && provided_devices.1.is_empty() {
        debug!("Discovering speakers...");
        let mut devices = sonor::discover(Duration::from_secs(2)).await?;
        while let Some(device) = devices.try_next().await? {
            speakers.push(device);
        }
    }

    info!("Found {} speakers", speakers.len());
    Ok(speakers)
}

struct SpeakerGroup {
    coordinator: String,
    speakers: Vec<SpeakerInfo>,
}

impl SpeakerGroup {
    #[must_use]
    fn new(coordinator: String, mut speakers: Vec<SpeakerInfo>) -> Self {
        let mut speaker_list = vec![];
        let coordinator_speaker = speakers
            .iter()
            .position(|s| s.uuid() == coordinator)
            .expect("Coordinator of the group was not found in the members of the group??");

        // Make sure the coordinator is first in the list of speakers, so its name gets displayed
        // first.
        speaker_list.push(speakers.remove(coordinator_speaker));
        // Now add the rest of the speakers
        speaker_list.append(&mut speakers);
        Self {
            coordinator,
            speakers: speaker_list,
        }
    }

    fn name(&self) -> String {
        let names: Vec<_> = self.speakers.iter().map(SpeakerInfo::name).collect();
        names.join(" + ")
    }
}

async fn fetch_favorite_playlists(speaker: &Speaker) -> Result<Vec<FavoritePlaylist>> {
    let service = URN::service("schemas-upnp-org", "ContentDirectory", 1);

    let payload = r#"<ObjectID>FV:2</ObjectID>
<BrowseFlag>BrowseDirectChildren</BrowseFlag>
<Filter>*</Filter>
<StartingIndex>0</StartingIndex>
<RequestedCount>100</RequestedCount>
<SortCriteria></SortCriteria>"#;

    let response = speaker
        .action(&service, "Browse", payload)
        .await
        .context("Failed to browse favorites")?;

    let xml = response
        .get("Result")
        .context("No Result in browse response")?;

    Ok(parse_favorite_playlists(xml))
}

fn parse_favorite_playlists(xml: &str) -> Vec<FavoritePlaylist> {
    let mut playlists = Vec::new();

    // Split XML into individual items
    let items: Vec<&str> = xml.split("<item ").skip(1).collect();

    for item in items {
        // Extract URI from <res> tag
        let uri = extract_tag_content(item, "<res", "</res>")
            .and_then(|res_block| res_block.find('>').map(|start| &res_block[start + 1..]))
            .unwrap_or("");

        // Filter for playlists only (check URI patterns and upnp:class)
        let is_playlist = uri.contains("playlist")
            || uri.starts_with("x-rincon-cpcontainer:")
            || item.contains("playlistContainer");

        if !is_playlist {
            continue;
        }

        let title = extract_tag_content(item, "<dc:title>", "</dc:title>")
            .unwrap_or("Unknown")
            .to_string();

        let description = extract_tag_content(item, "<r:description>", "</r:description>")
            .unwrap_or("")
            .to_string();

        let metadata = extract_tag_content(item, "<r:resMD>", "</r:resMD>")
            .unwrap_or("")
            .to_string();

        playlists.push(FavoritePlaylist {
            title,
            description,
            uri: uri.to_string(),
            metadata,
        });
    }

    playlists
}

fn extract_tag_content<'a>(text: &'a str, start_tag: &str, end_tag: &str) -> Option<&'a str> {
    let start = text.find(start_tag)?;
    let content_start = start + start_tag.len();
    let end = text[content_start..].find(end_tag)?;
    Some(&text[content_start..content_start + end])
}

fn html_unescape(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
}
