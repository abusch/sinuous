use std::collections::HashSet;
use std::net::Ipv4Addr;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use sinuous_client::{
    GroupHandle, GroupId, Household, Topology,
    favorites::Favorite,
    playback::{LoadAction, LoadOptions, PlaybackState},
    playback_metadata::{QueueItem, Track},
};
use tokio::{
    select,
    sync::mpsc::{Receiver, Sender},
};
use tracing::{debug, error, info, warn};

use crate::{Action, Direction, Update, ViewMode};

const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone)]
pub struct TrackInfo {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub duration_secs: Option<u32>,
}

impl From<&Track> for TrackInfo {
    fn from(track: &Track) -> Self {
        let artist = track
            .artist
            .as_ref()
            .or_else(|| track.album.as_ref()?.artist.as_ref());
        Self {
            title: track.name.clone(),
            artist: artist.map(|a| a.name.clone()),
            album: track.album.as_ref().map(|a| a.name.clone()),
            duration_secs: track.duration_millis.map(millis_to_secs),
        }
    }
}

#[derive(Debug)]
pub struct SpeakerState {
    pub is_playing: bool,
    pub current_volume: u8,
    pub group_names: Vec<String>,
    pub selected_group: usize,
    pub now_playing: Option<TrackInfo>,
    pub elapsed_secs: u32,
    pub next_track: Option<TrackInfo>,
    pub current_view: ViewMode,
    pub favorites: Vec<Favorite>,
    pub selected_favorite: usize,
}

impl SpeakerState {
    pub fn group_name(&self) -> &str {
        self.group_names
            .get(self.selected_group)
            .map_or("", String::as_str)
    }
}

pub struct SonosService {
    update_tx: Sender<Update>,
    cmd_rx: Receiver<Action>,
}

impl SonosService {
    pub fn new(update_tx: Sender<Update>, cmd_rx: Receiver<Action>) -> Self {
        Self { update_tx, cmd_rx }
    }

    pub fn start(self, provided_devices: (Vec<Ipv4Addr>, Vec<String>)) {
        tokio::spawn(async move {
            if let Err(err) = self.inner_loop(provided_devices).await {
                error!(%err, "Sonos error");
            }
        });
    }

    async fn inner_loop(mut self, provided_devices: (Vec<Ipv4Addr>, Vec<String>)) -> Result<()> {
        let household = connect_household(provided_devices).await?;
        let mut session = Session::new(household).await;

        // Initial state fetch
        if let Err(e) = session.refresh_state().await {
            warn!("Failed to fetch initial state: {e:#}");
        }

        let mut ticker = tokio::time::interval(Duration::from_secs(1));
        debug!("Starting sonos loop");

        loop {
            select! {
                _tick = ticker.tick() => {
                    // time to refresh our state
                    if let Err(e) = session.refresh_state().await {
                        warn!("Failed to refresh state: {e:#}");
                    }
                    self.send_update(&session).await;
                }
                cmd = self.cmd_rx.recv() => {
                    if let Some(c) = cmd {
                        let mut needs_refresh = false;

                        // Process the first command
                        match session.handle_command(c).await {
                            Ok(r) => if r { needs_refresh = true; },
                            Err(e) => warn!("Error handling command: {e:#}"),
                        }

                        // Drain pending commands
                        while let Ok(c) = self.cmd_rx.try_recv() {
                            match session.handle_command(c).await {
                                Ok(r) => if r { needs_refresh = true; },
                                Err(e) => warn!("Error handling batched command: {e:#}"),
                            }
                        }

                        if needs_refresh && let Err(e) = session.refresh_state().await {
                            warn!("Failed to refresh state after commands: {e:#}");
                        }
                    } else {
                        warn!("Command channel was closed: exiting...");
                        break;
                    }
                    self.send_update(&session).await;
                }
            }
        }
        session.household.close().await;
        Ok(())
    }

    async fn send_update(&self, session: &Session) {
        let speaker_state = session.build_state();
        if let Err(err) = self
            .update_tx
            .send(Update::NewState(Box::new(speaker_state)))
            .await
        {
            warn!(%err, "Updates channel was closed: exiting");
        }
    }
}

/// A group, as displayed in the group tabs.
struct GroupEntry {
    id: GroupId,
    name: String,
}

/// The state of the connection to a household.
struct Session {
    household: Household,
    groups: Vec<GroupEntry>,
    selected_group: Option<GroupId>,
    current_view: ViewMode,
    favorites: Vec<Favorite>,
    selected_favorite: usize,
    // Cached state
    cached_is_playing: bool,
    cached_volume: u8,
    cached_now_playing: Option<TrackInfo>,
    cached_elapsed_secs: u32,
    cached_next_track: Option<TrackInfo>,
}

impl Session {
    async fn new(household: Household) -> Self {
        let groups = group_entries(&household.topology());
        debug!("Found {} groups", groups.len());

        debug!("Fetching favorites...");
        let favorites = match household.connection().get_favorites().await {
            Ok(favs) => {
                info!("Found {} favorites", favs.items.len());
                favs.items
            }
            Err(e) => {
                warn!("Failed to fetch favorites: {e}");
                vec![]
            }
        };

        Self {
            household,
            selected_group: groups.first().map(|g| g.id.clone()),
            groups,
            current_view: ViewMode::Queue,
            favorites,
            selected_favorite: 0,
            cached_is_playing: false,
            cached_volume: 0,
            cached_now_playing: None,
            cached_elapsed_secs: 0,
            cached_next_track: None,
        }
    }

    async fn handle_command(&mut self, cmd: Action) -> Result<bool> {
        debug!(?cmd, "Handling command");
        let result: Result<bool> = match cmd {
            // Playback controls
            Action::Play => {
                self.current_group().await?.play().await?;
                Ok(true)
            }
            Action::Pause => {
                self.current_group().await?.pause().await?;
                Ok(true)
            }
            Action::Next => {
                self.current_group().await?.skip_to_next_track().await?;
                Ok(true)
            }
            Action::Prev => {
                self.current_group().await?.skip_to_previous_track().await?;
                Ok(true)
            }
            Action::VolAdjust(v) => {
                self.current_group().await?.set_relative_volume(v).await?;
                Ok(true)
            }

            // Group switching
            Action::NextSpeaker => {
                self.select_next_group();
                Ok(true)
            }
            Action::PrevSpeaker => {
                self.select_prev_group();
                Ok(true)
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
                let Some(favorite) = self.favorites.get(index) else {
                    warn!("Invalid favorite index: {}", index);
                    return Ok(false);
                };
                info!("Playing favorite: {}", favorite.name);
                let options = LoadOptions {
                    action: Some(LoadAction::Replace),
                    play_on_completion: Some(true),
                    ..Default::default()
                };
                self.current_group()
                    .await?
                    .load_favorite(&favorite.id, &options)
                    .await
                    .context("Failed to load favorite")?;
                Ok(true)
            }

            Action::Nop => Ok(false),
        };
        result.context("Error while handling command")
    }

    async fn refresh_state(&mut self) -> Result<()> {
        // Groups can be created and modified at any time, e.g. from the Sonos app.
        self.groups = group_entries(&self.household.topology());

        let group = self.current_group().await?;
        let (status, volume, metadata) = tokio::try_join!(
            group.get_playback_status(),
            group.get_volume(),
            group.get_metadata_status(),
        )?;

        self.cached_is_playing = matches!(
            status.playback_state,
            PlaybackState::Playing | PlaybackState::Buffering
        );
        self.cached_volume = volume.volume;
        self.cached_elapsed_secs = status.position_millis.map_or(0, millis_to_secs);
        self.cached_now_playing = metadata.current_item.and_then(|item| track_info(&item));
        self.cached_next_track = metadata.next_item.and_then(|item| track_info(&item));
        Ok(())
    }

    /// Index of the selected group in `groups`, falling back to the first group if the selected
    /// one is gone.
    fn selected_index(&self) -> usize {
        self.selected_group
            .as_ref()
            .and_then(|id| self.groups.iter().position(|g| &g.id == id))
            .unwrap_or(0)
    }

    fn select_prev_group(&mut self) {
        let index = match self.selected_index() {
            0 => self.groups.len().saturating_sub(1),
            i => i - 1,
        };
        self.selected_group = self.groups.get(index).map(|g| g.id.clone());
    }

    fn select_next_group(&mut self) {
        let mut index = self.selected_index() + 1;
        if index >= self.groups.len() {
            index = 0;
        }
        self.selected_group = self.groups.get(index).map(|g| g.id.clone());
    }

    async fn current_group(&self) -> Result<GroupHandle> {
        let group = self
            .groups
            .get(self.selected_index())
            .context("No selected group")?;
        Ok(self.household.group(&group.id).await?)
    }

    fn build_state(&self) -> SpeakerState {
        SpeakerState {
            is_playing: self.cached_is_playing,
            current_volume: self.cached_volume,
            group_names: self.groups.iter().map(|g| g.name.clone()).collect(),
            selected_group: self.selected_index(),
            now_playing: self.cached_now_playing.clone(),
            elapsed_secs: self.cached_elapsed_secs,
            next_track: self.cached_next_track.clone(),
            current_view: self.current_view,
            favorites: self.favorites.clone(),
            selected_favorite: self.selected_favorite,
        }
    }
}

/// Connect to the household of one of the provided speakers, or of the first speaker discovered
/// if none were provided.
async fn connect_household(provided_devices: (Vec<Ipv4Addr>, Vec<String>)) -> Result<Household> {
    let (ips, names) = provided_devices;

    debug!("Connecting to provided speakers...");
    for ip in &ips {
        match Household::connect(&ip.to_string()).await {
            Ok(household) => return Ok(household),
            Err(e) => debug!("Not connecting to {ip} due to errors: {e:#}"),
        }
    }
    if !ips.is_empty() && names.is_empty() {
        bail!("Could not connect to any of the provided speakers");
    }

    debug!("Discovering speakers...");
    let players = sinuous_client::discover(DISCOVERY_TIMEOUT).await?;
    info!("Found {} speakers", players.len());

    // Players from several households may answer: try one player from each.
    let mut tried_households = HashSet::new();
    for player in &players {
        if !tried_households.insert(&player.household_id) {
            continue;
        }
        let household = match player.connect().await {
            Ok(conn) => Household::new(conn).await,
            Err(e) => Err(e),
        };
        let household = match household {
            Ok(household) => household,
            Err(e) => {
                debug!("Not connecting to {} due to errors: {e:#}", player.address);
                // Give another player of this household a chance.
                tried_households.remove(&player.household_id);
                continue;
            }
        };
        let topology = household.topology();
        if names.is_empty()
            || names
                .iter()
                .any(|name| topology.players.iter().any(|p| &p.name == name))
        {
            return Ok(household);
        }
        debug!("None of {names:?} found in household {}", household.id());
        household.close().await;
    }

    if names.is_empty() {
        bail!("No speaker discovered!");
    }
    bail!("Could not find any of the speakers {names:?}");
}

/// The groups of the household, sorted by name.
fn group_entries(topology: &Topology) -> Vec<GroupEntry> {
    let mut groups: Vec<_> = topology
        .groups
        .iter()
        .map(|group| {
            // Make sure the coordinator comes first, so its name gets displayed first.
            let coordinator = topology.players.get(&group.coordinator_id);
            let others = group
                .player_ids
                .iter()
                .filter(|id| **id != group.coordinator_id)
                .filter_map(|id| topology.players.get(id));
            let names: Vec<_> = coordinator
                .into_iter()
                .chain(others)
                .map(|p| p.name.as_str())
                .collect();
            let name = if names.is_empty() {
                group.name.clone()
            } else {
                names.join(" + ")
            };
            GroupEntry {
                id: group.id.clone(),
                name,
            }
        })
        .collect();
    groups.sort_by(|a, b| a.name.cmp(&b.name));
    groups
}

/// Players report items with an empty track when there is nothing to show, e.g. for a suspended
/// Spotify Connect session.
fn track_info(item: &QueueItem) -> Option<TrackInfo> {
    item.track.name.as_ref()?;
    Some((&item.track).into())
}

fn millis_to_secs(millis: u64) -> u32 {
    u32::try_from(millis / 1000).unwrap_or(u32::MAX)
}
