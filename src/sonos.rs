//! The background service talking to the Sonos speakers.
//!
//! The service publishes a [`HouseholdState`] snapshot that the UI renders, and runs the
//! [`Command`]s the UI sends. The state is kept up to date from the events the speakers send,
//! rather than by polling them. Commands run in their own task, so that a slow speaker never holds
//! up state updates.

use std::collections::HashSet;
use std::net::Ipv4Addr;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use futures::StreamExt;
use sinuous_client::{
    Event, EventPayload, FavoriteId, GroupId, Household, HouseholdId, PlayerId, PlaylistId,
    Subscription, Topology,
    favorites::Favorite,
    hardware_status::{BatteryStatus, WirelessNetworkStatus},
    history::{ContentType, HistoryItem},
    playback::{
        LoadAction, LoadOptions, PlayModes, PlayModesUpdate, PlaybackActions, PlaybackState,
        PlaybackStatus,
    },
    playback_metadata::{ImageUrl, MetadataStatus, QueueItem, Tag, Track, UniversalMusicObjectId},
    playlists::PlaylistSummary,
    queue::Queue,
};
use tokio::{
    select,
    sync::{broadcast, mpsc, watch},
    task::{JoinError, JoinSet},
    time::{self, MissedTickBehavior},
};
use tracing::{debug, error, info, warn};
use url::Url;

const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(2);
/// How often to retry subscriptions that failed, e.g. because a speaker couldn't be reached. Once
/// made, subscriptions are restored by the household if a connection drops.
const RETRY_INTERVAL: Duration = Duration::from_secs(30);
/// Commands sent while this many are still waiting to run are dropped.
const COMMAND_QUEUE_SIZE: usize = 16;
/// The lists of the household the service keeps up to date.
const HOUSEHOLD_SUBSCRIPTIONS: [Subscription; 3] = [
    Subscription::Favorites,
    Subscription::Playlists,
    Subscription::History,
];

/// The speakers to connect to, as given on the command line or in the configuration file.
#[derive(Debug, Default)]
pub struct ProvidedDevices {
    pub ips: Vec<Ipv4Addr>,
    pub names: Vec<String>,
}

impl ProvidedDevices {
    /// Sort out the speakers given as IPs from the ones given as names.
    pub fn new(devices: impl IntoIterator<Item = String>) -> Self {
        let mut provided = Self::default();
        for device in devices {
            if let Ok(ip) = Ipv4Addr::from_str(&device) {
                provided.ips.push(ip);
            } else {
                provided.names.push(device);
            }
        }
        provided
    }
}

/// What the service knows about the household.
#[derive(Debug, Clone, Default)]
pub struct HouseholdState {
    pub household_id: Option<HouseholdId>,
    /// The groups and players as the household last reported them, with all their details.
    pub topology: Arc<Topology>,
    /// Sorted by name.
    pub groups: Vec<GroupState>,
    /// Sorted by name.
    pub players: Vec<PlayerInfo>,
    pub favorites: Arc<[Favorite]>,
    /// The Sonos playlists saved in the household.
    pub playlists: Arc<[PlaylistSummary]>,
    /// What was played recently, most recent first.
    pub history: Arc<[HistoryItem]>,
    /// The queue of the group the UI watches (see [`SonosHandle::watch_queue`]), or of the last
    /// one it watched.
    pub queue: Option<GroupQueue>,
    pub last_error: Option<ErrorMessage>,
}

impl HouseholdState {
    pub fn group(&self, id: &GroupId) -> Option<&GroupState> {
        self.groups.iter().find(|g| &g.id == id)
    }

    /// The players in `group`, sorted by name.
    pub fn members(&self, group: &GroupState) -> impl Iterator<Item = &PlayerInfo> {
        self.players
            .iter()
            .filter(|p| group.player_ids.contains(&p.id))
    }
}

/// The queue of a group.
#[derive(Debug, Clone)]
pub struct GroupQueue {
    pub group: GroupId,
    pub contents: QueueContents,
}

#[derive(Debug, Clone)]
pub enum QueueContents {
    Loading,
    Loaded(Arc<Queue>),
    Failed,
}

impl GroupQueue {
    /// The queue of `group`, as far as it is known.
    pub fn of<'a>(queue: Option<&'a Self>, group: &GroupId) -> &'a QueueContents {
        match queue {
            Some(queue) if queue.group == *group => &queue.contents,
            _ => &QueueContents::Loading,
        }
    }

    fn loaded(&self) -> Option<&Queue> {
        match &self.contents {
            QueueContents::Loaded(queue) => Some(queue),
            QueueContents::Loading | QueueContents::Failed => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ErrorMessage {
    pub message: String,
    pub at: Instant,
}

impl ErrorMessage {
    pub fn new(message: String) -> Self {
        Self {
            message,
            at: Instant::now(),
        }
    }
}

/// A group, and what it has told us about itself. Fields are `None` until the group has reported
/// them.
#[derive(Debug, Clone)]
pub struct GroupState {
    pub id: GroupId,
    pub name: String,
    pub coordinator_id: PlayerId,
    pub player_ids: Vec<PlayerId>,
    pub playback: Option<Playback>,
    pub volume: Option<u8>,
    pub now_playing: Option<TrackInfo>,
    pub next_track: Option<TrackInfo>,
    pub source: Option<Source>,
    /// When the sleep timer runs out, and playback fades out and pauses, if it is set.
    pub sleep_timer: Option<Instant>,
    /// The version of the queue it plays from, if it plays from its queue (see
    /// [`Queue::version`]).
    pub queue_version: Option<String>,
    /// The position in its queue of the current track, from 1, if it plays from its queue.
    pub queue_position: Option<u32>,
}

impl GroupState {
    pub fn new(
        id: GroupId,
        name: String,
        coordinator_id: PlayerId,
        player_ids: Vec<PlayerId>,
    ) -> Self {
        Self {
            id,
            name,
            coordinator_id,
            player_ids,
            playback: None,
            volume: None,
            now_playing: None,
            next_track: None,
            source: None,
            sleep_timer: None,
            queue_version: None,
            queue_position: None,
        }
    }

    /// Whether it plays from `queue`, which is then up to date.
    pub fn plays_from(&self, queue: &Queue) -> bool {
        self.queue_version.as_deref() == Some(queue.version.as_str())
    }
}

/// A player, and its volume once it has reported it.
#[derive(Debug, Clone)]
pub struct PlayerInfo {
    pub id: PlayerId,
    pub name: String,
    /// The model of its main device, e.g. `Arc Ultra` for a home theatre, if it is known.
    pub model: Option<String>,
    pub volume: Option<u8>,
    /// The battery of a portable player, once it has reported it.
    pub battery: Option<BatteryStatus>,
    /// Its Wi-Fi connection, once it has reported it. Players connected by Ethernet don't.
    pub wifi: Option<WirelessNetworkStatus>,
}

#[derive(Debug, Clone, Copy)]
pub struct Playback {
    pub state: PlaybackState,
    pub is_playing: bool,
    pub play_modes: PlayModes,
    /// What the current source supports.
    pub actions: PlaybackActions,
    /// The position in the current track when it was reported.
    position: Duration,
    /// When `position` was reported, if it is moving forward.
    advancing_since: Option<Instant>,
}

impl Playback {
    /// The playback as reported at `now`.
    fn new(status: &PlaybackStatus, now: Instant) -> Self {
        Self {
            play_modes: status.play_modes,
            actions: status.available_playback_actions,
            ..Self::from_parts(
                status.playback_state,
                Duration::from_millis(status.position_millis.unwrap_or(0)),
                now,
            )
        }
    }

    fn from_parts(state: PlaybackState, position: Duration, now: Instant) -> Self {
        Self {
            state,
            is_playing: matches!(state, PlaybackState::Playing | PlaybackState::Buffering),
            play_modes: PlayModes::default(),
            actions: PlaybackActions::default(),
            position,
            advancing_since: (state == PlaybackState::Playing).then_some(now),
        }
    }

    /// The position in the current track at `now`.
    ///
    /// Speakers only report the position when something changes (e.g. the track or the playback
    /// state), so it is worked out from the last one they reported.
    pub fn position(&self, now: Instant) -> Duration {
        let advanced = self
            .advancing_since
            .map_or(Duration::ZERO, |since| now.saturating_duration_since(since));
        self.position + advanced
    }

    /// Whether [`Playback::position`] changes over time.
    pub fn is_advancing(&self) -> bool {
        self.advancing_since.is_some()
    }

    /// Paused playback, in these play modes, of a source supporting `actions`.
    #[cfg(test)]
    pub fn paused(play_modes: PlayModes, actions: PlaybackActions) -> Self {
        Self {
            play_modes,
            actions,
            ..Self::from_parts(PlaybackState::Paused, Duration::ZERO, Instant::now())
        }
    }

    /// Idle playback, e.g. of a stopped live radio stream, of a source supporting `actions`.
    #[cfg(test)]
    pub fn idle(actions: PlaybackActions) -> Self {
        Self {
            actions,
            ..Self::from_parts(PlaybackState::Idle, Duration::ZERO, Instant::now())
        }
    }

    /// Playback, in the default play modes, of a source supporting `actions`.
    #[cfg(test)]
    pub fn playing(actions: PlaybackActions) -> Self {
        Self {
            actions,
            ..Self::from_parts(PlaybackState::Playing, Duration::ZERO, Instant::now())
        }
    }
}

#[derive(Debug, Clone)]
pub struct TrackInfo {
    pub title: Option<String>,
    pub artist: Option<String>,
    /// Whether its album is tagged explicit. Tracks have no tags of their own.
    pub album_explicit: bool,
    pub duration_secs: Option<u32>,
    /// The album art, or another picture of what is playing.
    pub image_url: Option<Url>,
    /// Whether it is a radio station rather than a track, so it has no artist.
    pub is_station: bool,
}

impl TrackInfo {
    /// The track of `item`, with its image found with `images`, if given.
    ///
    /// Players report items with an empty track when there is nothing to show, e.g. for a suspended
    /// Spotify Connect session.
    fn new(item: &QueueItem, images: Option<&Images>) -> Option<Self> {
        let track = &item.track;
        track.name.as_ref()?;
        let artist = track
            .artist
            .as_ref()
            .or_else(|| track.album.as_ref()?.artist.as_ref());
        Some(Self {
            title: track.name.clone(),
            artist: artist.map(|a| a.name.clone()),
            album_explicit: track
                .album
                .as_ref()
                .is_some_and(|a| a.tags.contains(&Tag::TagExplicit)),
            duration_secs: track.duration_millis.map(millis_to_secs),
            image_url: images.and_then(|images| images.resolve(track)),
            is_station: false,
        })
    }

    /// What the radio station of `metadata` plays, for streams with no current track, or once
    /// they are stopped: what the stream says it plays, if anything, or the name of the station.
    fn station(metadata: &MetadataStatus, images: &Images) -> Option<Self> {
        let container = metadata.container.as_ref()?;
        if !matches!(container.r#type.as_deref(), Some("station" | "program")) {
            return None;
        }
        let title = metadata.stream_info.as_ref().or(container.name.as_ref())?;
        Some(Self {
            title: Some(title.clone()),
            artist: None,
            album_explicit: false,
            duration_secs: None,
            image_url: images.fallback.and_then(|image| images.url(image)),
            is_station: true,
        })
    }
}

/// Where what a group plays comes from, e.g. an album on Spotify.
#[derive(Debug, Clone)]
pub struct Source {
    /// The music service, e.g. `Spotify`.
    pub service: Option<String>,
    /// The kind of source, e.g. `album` or `playlist`.
    pub kind: Option<String>,
    pub name: Option<String>,
}

impl Source {
    fn new(metadata: &MetadataStatus) -> Option<Self> {
        let container = metadata.container.as_ref();
        let service = container
            .and_then(|c| c.service.as_ref())
            .or_else(|| metadata.current_item.as_ref()?.track.service.as_ref())
            .map(|s| s.name.clone());
        let name = container.and_then(|c| c.name.clone());
        // Idle groups report an empty container.
        if service.is_none() && name.is_none() {
            return None;
        }
        Some(Self {
            service,
            kind: container.and_then(|c| c.r#type.clone()),
            name,
        })
    }
}

/// Something to do with a group or a player.
#[derive(Debug)]
pub enum Command {
    Group {
        group: GroupId,
        action: GroupAction,
    },
    Player {
        player: PlayerId,
        action: PlayerAction,
    },
}

#[derive(Debug)]
pub enum GroupAction {
    Play,
    /// Pause, or stop what can't be paused, e.g. a live radio stream.
    Pause,
    Next,
    Previous,
    AdjustVolume(i8),
    LoadFavorite(FavoriteId),
    LoadPlaylist(PlaylistId),
    /// Play content from a music service, e.g. something in the history.
    LoadContent(UniversalMusicObjectId, ContentType),
    /// Play the track at this position in the queue, from 1.
    SkipToTrack(u32),
    /// Make these players the members of the group. The coordinator must come first.
    SetMembers(Vec<PlayerId>),
    SetPlayModes(PlayModesUpdate),
    /// Pause after this long, or never.
    SetSleepTimer(Option<Duration>),
}

#[derive(Debug)]
pub enum PlayerAction {
    AdjustVolume(i8),
}

impl Command {
    /// Add `other` to this command if running both amounts to running one: changes of the volume
    /// of the same group or player. Gives `other` back otherwise.
    fn merge(&mut self, other: Command) -> Result<(), Command> {
        match (self, other) {
            (
                Command::Group {
                    group,
                    action: GroupAction::AdjustVolume(delta),
                },
                Command::Group {
                    group: other_group,
                    action: GroupAction::AdjustVolume(other_delta),
                },
            ) if *group == other_group => {
                *delta = delta.saturating_add(other_delta);
                Ok(())
            }
            (
                Command::Player {
                    player,
                    action: PlayerAction::AdjustVolume(delta),
                },
                Command::Player {
                    player: other_player,
                    action: PlayerAction::AdjustVolume(other_delta),
                },
            ) if *player == other_player => {
                *delta = delta.saturating_add(other_delta);
                Ok(())
            }
            (_, other) => Err(other),
        }
    }
}

/// The UI's side of the service.
pub struct SonosHandle {
    state_rx: watch::Receiver<HouseholdState>,
    cmd_tx: mpsc::Sender<Command>,
    watched_queue_tx: watch::Sender<Option<GroupId>>,
}

impl SonosHandle {
    /// Watch the state of the household.
    ///
    /// The last state stays available once the service has stopped.
    pub fn state(&self) -> watch::Receiver<HouseholdState> {
        self.state_rx.clone()
    }

    /// Run a command, without waiting for it to complete. Failures are reported in
    /// [`HouseholdState::last_error`].
    pub fn send(&self, command: Command) {
        if let Err(err) = self.cmd_tx.try_send(command) {
            warn!(%err, "Dropping command");
        }
    }

    /// Keep the queue of `group` up to date in [`HouseholdState::queue`], or stop if `None`.
    ///
    /// Queues can be long, so they are only read while they are looked at. A queue starts being
    /// read again whenever it is watched again, as changes to it are only reported while its group
    /// plays from it.
    pub fn watch_queue(&self, group: Option<GroupId>) {
        self.watched_queue_tx.send_if_modified(|watched| {
            let changed = *watched != group;
            *watched = group;
            changed
        });
    }
}

/// Start the service in the background.
///
/// The groups and speakers of the household are in the state right away.
pub fn spawn(household: Household) -> SonosHandle {
    let (state_tx, state_rx) = watch::channel(HouseholdState {
        household_id: Some(household.id().clone()),
        ..HouseholdState::default()
    });
    let (cmd_tx, cmd_rx) = mpsc::channel(COMMAND_QUEUE_SIZE);
    let (watched_queue_tx, watched_queue_rx) = watch::channel(None);

    tokio::spawn(run_commands(household.clone(), cmd_rx, state_tx.clone()));
    let mut service = Service::new(household, state_tx, watched_queue_rx);
    service.update_groups();
    tokio::spawn(service.run());

    SonosHandle {
        state_rx,
        cmd_tx,
        watched_queue_tx,
    }
}

/// Keeps the [`HouseholdState`] up to date.
struct Service {
    household: Household,
    state_tx: watch::Sender<HouseholdState>,
    events: broadcast::Receiver<Event>,
    topology_rx: watch::Receiver<Topology>,
    /// The group and player subscriptions made, or in progress.
    subscribed: HashSet<Subscription>,
    /// Subscriptions in progress.
    subscriptions: JoinSet<(Subscription, Result<(), sinuous_client::Error>)>,
    /// The subscriptions that failed last time, and have been reported: they are retried until they
    /// succeed, without reporting them again.
    failing: HashSet<Subscription>,
    /// The subscriptions to the lists of the household made, among
    /// [`HOUSEHOLD_SUBSCRIPTIONS`].
    household_subscribed: HashSet<Subscription>,
    /// The version of the favorites in the state.
    favorites_version: Option<String>,
    /// The version of the playlists in the state.
    playlists_version: Option<String>,
    /// The version of the history in the state.
    history_version: Option<String>,
    /// The group whose queue to keep up to date, if any.
    watched_queue_rx: watch::Receiver<Option<GroupId>>,
    /// The queue being read, if any.
    queue_reads: JoinSet<(GroupId, Result<Queue, sinuous_client::Error>)>,
}

impl Service {
    fn new(
        household: Household,
        state_tx: watch::Sender<HouseholdState>,
        watched_queue_rx: watch::Receiver<Option<GroupId>>,
    ) -> Self {
        Self {
            // Before subscribing to anything, so that we get the initial events.
            events: household.events(),
            topology_rx: household.topology_updates(),
            household,
            state_tx,
            subscribed: HashSet::new(),
            subscriptions: JoinSet::new(),
            failing: HashSet::new(),
            household_subscribed: HashSet::new(),
            favorites_version: None,
            playlists_version: None,
            history_version: None,
            watched_queue_rx,
            queue_reads: JoinSet::new(),
        }
    }

    async fn run(mut self) {
        // The favorites, the playlists and the history get loaded when the initial events arrive.
        self.subscribe_to_household().await;

        let mut retry = time::interval_at(time::Instant::now() + RETRY_INTERVAL, RETRY_INTERVAL);
        retry.set_missed_tick_behavior(MissedTickBehavior::Delay);
        debug!("Starting sonos loop");

        loop {
            select! {
                event = self.events.recv() => match event {
                    Ok(event) => self.handle_event(event).await,
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!("Missed {n} events");
                        self.resync().await;
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                },
                changed = self.topology_rx.changed() => {
                    if changed.is_err() {
                        break;
                    }
                    // Groups can be created and modified at any time, e.g. from the Sonos app.
                    self.update_groups();
                }
                Some(result) = self.subscriptions.join_next() => self.subscription_done(result),
                changed = self.watched_queue_rx.changed() => {
                    if changed.is_err() {
                        break;
                    }
                    self.watch_queue();
                }
                Some(result) = self.queue_reads.join_next() => self.queue_read(result),
                _ = retry.tick() => self.retry_subscriptions().await,
                () = self.state_tx.closed() => {
                    debug!("UI is gone: exiting...");
                    break;
                }
            }
        }
        self.household.close().await;
    }

    async fn handle_event(&mut self, event: Event) {
        match event.payload {
            EventPayload::VersionChanged(changed) if event.namespace == "favorites" => {
                self.load_favorites(changed.version).await;
            }
            EventPayload::VersionChanged(changed) if event.namespace == "playlists" => {
                self.load_playlists(changed.version).await;
            }
            EventPayload::VersionChanged(changed) if event.namespace == "history" => {
                self.load_history(changed.version).await;
            }
            EventPayload::PlaybackError(err) => self.report_error(format!(
                "Playback error: {}",
                err.reason.unwrap_or(err.error_code)
            )),
            payload @ (EventPayload::PlayerVolume(_)
            | EventPayload::BatteryStatus(_)
            | EventPayload::WirelessNetworkStatus(_)) => {
                let Some(id) = event.player_id else { return };
                self.state_tx.send_if_modified(|state| {
                    let Some(player) = state.players.iter_mut().find(|p| p.id == id) else {
                        return false;
                    };
                    match payload {
                        EventPayload::PlayerVolume(volume) => player.volume = Some(volume.volume),
                        EventPayload::BatteryStatus(battery) => player.battery = Some(battery),
                        EventPayload::WirelessNetworkStatus(wifi) => player.wifi = Some(wifi),
                        _ => return false,
                    }
                    true
                });
            }
            payload => {
                let Some(id) = event.group_id else { return };
                let mut queue_changed = false;
                self.state_tx.send_if_modified(|state| {
                    let Some(group) = state.groups.iter_mut().find(|g| g.id == id) else {
                        return false;
                    };
                    match payload {
                        EventPayload::PlaybackStatus(status) => {
                            group.playback = Some(Playback::new(&status, Instant::now()));
                            group.queue_position = status
                                .queue_version
                                .as_ref()
                                .and(status.item_id.as_deref())
                                .and_then(|item| item.parse().ok());
                            group.queue_version = status.queue_version;
                            queue_changed = group.queue_version.as_ref().is_some_and(|version| {
                                state
                                    .queue
                                    .as_ref()
                                    .filter(|queue| queue.group == id)
                                    .and_then(GroupQueue::loaded)
                                    .is_some_and(|queue| queue.version != *version)
                            });
                        }
                        EventPayload::MetadataStatus(metadata) => {
                            // Images served by the coordinator are given relative to it.
                            let coordinator_url = self
                                .topology_rx
                                .borrow()
                                .coordinator(&id)
                                .map(|p| p.websocket_url.clone());
                            let images = Images {
                                coordinator_url: coordinator_url.as_ref(),
                                fallback: metadata
                                    .container
                                    .as_ref()
                                    .and_then(|c| c.image_url.as_ref()),
                            };
                            group.now_playing = metadata
                                .current_item
                                .as_ref()
                                .and_then(|item| TrackInfo::new(item, Some(&images)))
                                .or_else(|| TrackInfo::station(&metadata, &images));
                            // Only the image of the current track is shown.
                            group.next_track = metadata
                                .next_item
                                .as_ref()
                                .and_then(|item| TrackInfo::new(item, None));
                            group.source = Source::new(&metadata);
                        }
                        EventPayload::GroupVolume(volume) => group.volume = Some(volume.volume),
                        EventPayload::SleepTimerStatus(timer) => {
                            group.sleep_timer = timer.active.then(|| {
                                Instant::now() + timer.remaining.unwrap_or(Duration::ZERO)
                            });
                        }
                        _ => return false,
                    }
                    true
                });
                if queue_changed && self.watched_queue_rx.borrow().as_ref() == Some(&id) {
                    self.read_queue(id);
                }
            }
        }
    }

    /// Start reading the queue of the group now watched, if any.
    fn watch_queue(&mut self) {
        self.queue_reads.abort_all();
        let watched = self.watched_queue_rx.borrow_and_update().clone();
        if let Some(group) = watched {
            self.read_queue(group);
        }
    }

    /// Read the queue of `group` in the background, keeping what is known of it until then.
    fn read_queue(&mut self, group: GroupId) {
        debug!(%group, "Reading queue");
        self.queue_reads.abort_all();
        self.state_tx.send_if_modified(|state| {
            if state
                .queue
                .as_ref()
                .is_some_and(|queue| queue.group == group)
            {
                return false;
            }
            state.queue = Some(GroupQueue {
                group: group.clone(),
                contents: QueueContents::Loading,
            });
            true
        });
        let household = self.household.clone();
        self.queue_reads.spawn(async move {
            let queue = match household.group(&group).await {
                Ok(handle) => handle.get_queue().await,
                Err(err) => Err(err),
            };
            (group, queue)
        });
    }

    fn queue_read(
        &mut self,
        result: Result<(GroupId, Result<Queue, sinuous_client::Error>), JoinError>,
    ) {
        let (group, queue) = match result {
            Ok(read) => read,
            Err(err) if err.is_cancelled() => return,
            Err(err) => return error!("Queue reading task failed: {err}"),
        };
        let contents = match queue {
            Ok(queue) => {
                debug!(%group, tracks = queue.items.len(), version = queue.version, "Read queue");
                QueueContents::Loaded(Arc::new(queue))
            }
            Err(err) => {
                self.report_error(format!("Failed to read the queue: {err}"));
                QueueContents::Failed
            }
        };
        self.state_tx.send_modify(|state| match &mut state.queue {
            // Keep showing the queue as it was when it can't be read again.
            Some(queue) if queue.group == group && matches!(contents, QueueContents::Failed) => {
                if matches!(queue.contents, QueueContents::Loading) {
                    queue.contents = contents;
                }
            }
            queue => *queue = Some(GroupQueue { group, contents }),
        });
    }

    async fn load_favorites(&mut self, version: String) {
        if self.favorites_version.as_ref() == Some(&version) {
            return;
        }
        debug!("Fetching favorites...");
        let favorites = match self.household.connection().await {
            Ok(conn) => conn.get_favorites().await,
            Err(err) => Err(err),
        };
        match favorites {
            Ok(favorites) => {
                info!("Found {} favorites", favorites.items.len());
                self.favorites_version = Some(version);
                self.state_tx.send_modify(|state| {
                    state.favorites = favorites.items.into();
                });
            }
            Err(err) => self.report_error(format!("Failed to fetch favorites: {err}")),
        }
    }

    async fn load_playlists(&mut self, version: String) {
        if self.playlists_version.as_ref() == Some(&version) {
            return;
        }
        debug!("Fetching playlists...");
        let playlists = match self.household.connection().await {
            Ok(conn) => conn.get_playlists().await,
            Err(err) => Err(err),
        };
        match playlists {
            Ok(playlists) => {
                info!("Found {} playlists", playlists.playlists.len());
                self.playlists_version = Some(version);
                self.state_tx.send_modify(|state| {
                    state.playlists = playlists.playlists.into();
                });
            }
            Err(err) => self.report_error(format!("Failed to fetch playlists: {err}")),
        }
    }

    async fn load_history(&mut self, version: String) {
        if self.history_version.as_ref() == Some(&version) {
            return;
        }
        debug!("Fetching history...");
        let history = match self.household.connection().await {
            Ok(conn) => conn.get_history().await,
            Err(err) => Err(err),
        };
        match history {
            Ok(history) => {
                info!("Found {} items in the history", history.items.len());
                self.history_version = Some(version);
                self.state_tx.send_modify(|state| {
                    state.history = history.items.into();
                });
            }
            Err(err) => self.report_error(format!("Failed to fetch the history: {err}")),
        }
    }

    /// Update the groups and players from the household's topology, keeping what we know about
    /// them.
    fn update_groups(&mut self) {
        let (topology, groups, players) = {
            let topology = self.topology_rx.borrow_and_update();
            let (groups, players) = (group_states(&topology), player_infos(&topology));
            (Arc::new(topology.clone()), groups, players)
        };
        self.state_tx.send_modify(|state| {
            state.topology = topology;
            let old_groups = std::mem::replace(&mut state.groups, groups);
            keep_group_states(&mut state.groups, &old_groups);
            let old_players = std::mem::replace(&mut state.players, players);
            keep_player_states(&mut state.players, &old_players);
        });
        self.subscribe_to_members();
    }

    /// Subscribe to the events of the groups and players we haven't subscribed to yet, and forget
    /// the subscriptions of the ones that are gone.
    ///
    /// The subscriptions run in the background: they may need to connect to other speakers.
    fn subscribe_to_members(&mut self) {
        let wanted: HashSet<_> = {
            let state = self.state_tx.borrow();
            let groups = state.groups.iter().flat_map(|group| {
                [
                    Subscription::Playback(group.id.clone()),
                    Subscription::PlaybackMetadata(group.id.clone()),
                    Subscription::GroupVolume(group.id.clone()),
                    Subscription::SleepTimer(group.id.clone()),
                ]
            });
            let players = state.players.iter().flat_map(|player| {
                [
                    Subscription::PlayerVolume(player.id.clone()),
                    Subscription::HardwareStatus(player.id.clone()),
                ]
            });
            groups.chain(players).collect()
        };
        self.subscribed.retain(|s| wanted.contains(s));
        self.failing
            .retain(|s| HOUSEHOLD_SUBSCRIPTIONS.contains(s) || wanted.contains(s));
        for subscription in wanted {
            if !self.subscribed.insert(subscription.clone()) {
                continue;
            }
            debug!("Subscribing to {subscription:?}");
            let household = self.household.clone();
            self.subscriptions.spawn(async move {
                let result = household.subscribe(&subscription).await;
                (subscription, result)
            });
        }
    }

    fn subscription_done(
        &mut self,
        result: Result<(Subscription, Result<(), sinuous_client::Error>), JoinError>,
    ) {
        match result {
            Ok((subscription, Ok(()))) => {
                self.failing.remove(&subscription);
            }
            Ok((subscription, Err(err))) => {
                // Try again later.
                self.subscribed.remove(&subscription);
                let message = format!(
                    "Failed to get updates from {}: {err}",
                    self.subject(&subscription)
                );
                self.subscription_failed(subscription, message);
            }
            Err(err) => error!("Subscription task failed: {err}"),
        }
    }

    /// Subscribe to the lists of the household we haven't subscribed to yet.
    async fn subscribe_to_household(&mut self) {
        for subscription in HOUSEHOLD_SUBSCRIPTIONS {
            if self.household_subscribed.contains(&subscription) {
                continue;
            }
            match self.household.subscribe(&subscription).await {
                Ok(()) => {
                    self.failing.remove(&subscription);
                    self.household_subscribed.insert(subscription);
                }
                Err(err) => {
                    let list = match subscription {
                        Subscription::History => "the history",
                        Subscription::Playlists => "playlists",
                        _ => "favorites",
                    };
                    self.subscription_failed(
                        subscription,
                        format!("Failed to get updates to {list}: {err}"),
                    );
                }
            }
        }
    }

    /// Report that `subscription` failed with `message`, unless it already failed last time.
    fn subscription_failed(&mut self, subscription: Subscription, message: String) {
        if self.failing.insert(subscription) {
            self.report_error(message);
        } else {
            warn!("{message}");
        }
    }

    /// The group or the speaker `subscription` is for, to tell the user.
    fn subject(&self, subscription: &Subscription) -> String {
        let state = self.state_tx.borrow();
        let name = match subscription {
            Subscription::Playback(id)
            | Subscription::PlaybackMetadata(id)
            | Subscription::GroupVolume(id)
            | Subscription::SleepTimer(id) => state.group(id).map(|g| g.name.clone()),
            Subscription::PlayerVolume(id) | Subscription::HardwareStatus(id) => state
                .players
                .iter()
                .find(|p| &p.id == id)
                .map(|p| p.name.clone()),
            _ => None,
        };
        name.unwrap_or_else(|| "a speaker".to_owned())
    }

    async fn retry_subscriptions(&mut self) {
        self.subscribe_to_members();
        self.subscribe_to_household().await;
    }

    /// Subscribe to everything again, to catch up with missed events: subscribing makes the
    /// speakers send their whole state.
    async fn resync(&mut self) {
        debug!("Resyncing");
        self.subscribed.clear();
        self.household_subscribed.clear();
        self.subscribe_to_members();
        self.subscribe_to_household().await;
    }

    fn report_error(&self, message: String) {
        report_error(&self.state_tx, message);
    }
}

fn report_error(state_tx: &watch::Sender<HouseholdState>, message: String) {
    warn!("{message}");
    state_tx.send_modify(|state| {
        state.last_error = Some(ErrorMessage::new(message));
    });
}

/// Run commands one at a time, in the order they were sent, and report failures.
///
/// There is no need to report what the commands change: the speakers send events about it.
async fn run_commands(
    household: Household,
    mut cmd_rx: mpsc::Receiver<Command>,
    state_tx: watch::Sender<HouseholdState>,
) {
    // The command received while merging the previous ones, which couldn't be merged.
    let mut next = None;
    loop {
        let mut command = match next.take() {
            Some(command) => command,
            None => match cmd_rx.recv().await {
                Some(command) => command,
                None => break,
            },
        };
        // Catch up with the volume changes sent while the last command ran, e.g. while a key is
        // held down, in one go.
        while let Ok(queued) = cmd_rx.try_recv() {
            if let Err(queued) = command.merge(queued) {
                next = Some(queued);
                break;
            }
        }
        debug!(?command, "Running command");
        if let Err(err) = run_command(&household, &command).await {
            report_error(&state_tx, format!("{err:#}"));
        }
    }
}

async fn run_command(household: &Household, command: &Command) -> Result<()> {
    match command {
        Command::Group { group, action } => run_group_command(household, group, action).await,
        Command::Player { player, action } => {
            let player = household.player(player).await?;
            match action {
                PlayerAction::AdjustVolume(delta) => player
                    .set_relative_volume(*delta)
                    .await
                    .context("Failed to change the volume")?,
            }
            Ok(())
        }
    }
}

async fn run_group_command(
    household: &Household,
    id: &GroupId,
    action: &GroupAction,
) -> Result<()> {
    let group = household.group(id).await?;
    match action {
        GroupAction::Play => group.play().await.context("Failed to play")?,
        GroupAction::Pause => group.pause().await.context("Failed to pause")?,
        GroupAction::Next => group
            .skip_to_next_track()
            .await
            .context("Failed to skip to the next track")?,
        GroupAction::Previous => group
            .skip_to_previous_track()
            .await
            .context("Failed to skip to the previous track")?,
        GroupAction::AdjustVolume(delta) => group
            .set_relative_volume(*delta)
            .await
            .context("Failed to change the volume")?,
        GroupAction::LoadFavorite(favorite) => {
            group
                .load_favorite(favorite, &play_now())
                .await
                .context("Failed to play favorite")?;
        }
        GroupAction::LoadPlaylist(playlist) => group
            .load_playlist(playlist, &play_now())
            .await
            .context("Failed to play the playlist")?,
        GroupAction::LoadContent(id, content_type) => group
            .load_content(id, *content_type, &play_now())
            .await
            .context("Failed to play it again")?,
        GroupAction::SkipToTrack(position) => {
            group
                .skip_to_track(*position)
                .await
                .context("Failed to skip to the track")?;
            // Skipping doesn't start playback.
            group.play().await.context("Failed to play")?;
        }
        GroupAction::SetMembers(players) => {
            group
                .set_members(players)
                .await
                .context("Failed to group speakers")?;
        }
        GroupAction::SetPlayModes(modes) => group
            .set_play_modes(modes)
            .await
            .context("Failed to change the play modes")?,
        GroupAction::SetSleepTimer(Some(duration)) => group
            .set_sleep_timer(*duration)
            .await
            .context("Failed to set the sleep timer")?,
        GroupAction::SetSleepTimer(None) => group
            .cancel_sleep_timer()
            .await
            .context("Failed to cancel the sleep timer")?,
    }
    Ok(())
}

/// Options to replace the queue of a group with what is loaded, and play it.
fn play_now() -> LoadOptions {
    LoadOptions {
        action: Some(LoadAction::Replace),
        play_on_completion: Some(true),
        ..Default::default()
    }
}

/// Connect to the household of one of the provided speakers, or of the first speaker discovered
/// if none were provided.
pub async fn connect(devices: ProvidedDevices) -> Result<Household> {
    let ProvidedDevices { ips, names } = devices;

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
    let mut players = sinuous_client::discover_stream(DISCOVERY_TIMEOUT).await?;

    // Use the first player that answers. Players from several households may answer: try one
    // player from each.
    let mut tried_households = HashSet::new();
    while let Some(player) = players.next().await {
        if !tried_households.insert(player.household_id.clone()) {
            continue;
        }
        info!("Connecting to {} at {}", player.player_id, player.address);
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
fn group_states(topology: &Topology) -> Vec<GroupState> {
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
            GroupState::new(
                group.id.clone(),
                name,
                group.coordinator_id.clone(),
                group.player_ids.clone(),
            )
        })
        .collect();
    groups.sort_by(|a, b| a.name.cmp(&b.name));
    groups
}

/// The players of the household, sorted by name.
fn player_infos(topology: &Topology) -> Vec<PlayerInfo> {
    let mut players: Vec<_> = topology
        .players
        .iter()
        .map(|player| PlayerInfo {
            id: player.id.clone(),
            name: player.name.clone(),
            // A player made of several devices, e.g. a soundbar with a sub, has the ID of its main
            // one.
            model: player
                .devices
                .iter()
                .find(|device| device.id == player.id)
                .or(player.devices.first())
                .map(|device| device.model_display_name.clone()),
            volume: None,
            battery: None,
            wifi: None,
        })
        .collect();
    players.sort_by(|a, b| a.name.cmp(&b.name));
    players
}

/// Copy what we know about the groups in `old_groups` to the same groups in `groups`.
fn keep_group_states(groups: &mut [GroupState], old_groups: &[GroupState]) {
    for group in groups {
        if let Some(old) = old_groups.iter().find(|old| old.id == group.id) {
            group.playback = old.playback;
            group.volume = old.volume;
            group.now_playing.clone_from(&old.now_playing);
            group.next_track.clone_from(&old.next_track);
            group.source.clone_from(&old.source);
            group.sleep_timer = old.sleep_timer;
            group.queue_version.clone_from(&old.queue_version);
            group.queue_position = old.queue_position;
        }
    }
}

/// Copy what we know about the players in `old_players` to the same players in `players`.
fn keep_player_states(players: &mut [PlayerInfo], old_players: &[PlayerInfo]) {
    for player in players {
        if let Some(old) = old_players.iter().find(|old| old.id == player.id) {
            player.volume = old.volume;
            player.battery = old.battery;
            player.wifi.clone_from(&old.wifi);
        }
    }
}

/// How to find the images of tracks.
struct Images<'a> {
    /// The websocket URL of the player that sent the tracks.
    coordinator_url: Option<&'a Url>,
    /// The image to use for tracks without one, e.g. the logo of a radio station.
    fallback: Option<&'a ImageUrl>,
}

impl Images<'_> {
    fn resolve(&self, track: &Track) -> Option<Url> {
        let image = track
            .image_url
            .as_ref()
            .or_else(|| track.album.as_ref()?.image_url.as_ref())
            .or(self.fallback)?;
        self.url(image)
    }

    fn url(&self, image: &ImageUrl) -> Option<Url> {
        match self.coordinator_url {
            Some(coordinator_url) => image.resolve(coordinator_url),
            None => Url::parse(image.as_str()).ok(),
        }
    }
}

fn millis_to_secs(millis: u64) -> u32 {
    u32::try_from(millis / 1000).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn position_advances_while_playing() {
        let start = Instant::now();
        let playback = Playback::from_parts(PlaybackState::Playing, Duration::from_secs(10), start);
        assert!(playback.is_playing);
        assert!(playback.is_advancing());
        assert_eq!(playback.position(start), Duration::from_secs(10));
        assert_eq!(
            playback.position(start + Duration::from_secs(5)),
            Duration::from_secs(15)
        );
    }

    #[test]
    fn position_stays_put_unless_playing() {
        let start = Instant::now();
        let later = start + Duration::from_secs(5);
        for (state, is_playing) in [
            (PlaybackState::Paused, false),
            (PlaybackState::Idle, false),
            (PlaybackState::Buffering, true),
        ] {
            let playback = Playback::from_parts(state, Duration::from_secs(10), start);
            assert_eq!(playback.is_playing, is_playing, "{state:?}");
            assert!(!playback.is_advancing(), "{state:?}");
            assert_eq!(
                playback.position(later),
                Duration::from_secs(10),
                "{state:?}"
            );
        }
    }

    #[test]
    fn only_volume_changes_of_the_same_target_are_merged() {
        let group_volume = |group: &str, delta| Command::Group {
            group: group.into(),
            action: GroupAction::AdjustVolume(delta),
        };
        let player_volume = |player: &str, delta| Command::Player {
            player: player.into(),
            action: PlayerAction::AdjustVolume(delta),
        };

        let mut command = group_volume("kitchen", 1);
        assert!(command.merge(group_volume("kitchen", 1)).is_ok());
        assert!(command.merge(group_volume("kitchen", i8::MAX)).is_ok());
        assert!(matches!(
            command,
            Command::Group {
                action: GroupAction::AdjustVolume(i8::MAX),
                ..
            }
        ));
        assert!(command.merge(group_volume("office", 1)).is_err());
        assert!(command.merge(player_volume("kitchen", 1)).is_err());
        let play = Command::Group {
            group: "kitchen".into(),
            action: GroupAction::Play,
        };
        assert!(command.merge(play).is_err());

        let mut command = player_volume("k", -1);
        assert!(command.merge(player_volume("k", -1)).is_ok());
        assert!(matches!(
            command,
            Command::Player {
                action: PlayerAction::AdjustVolume(-2),
                ..
            }
        ));
        assert!(command.merge(player_volume("o", -1)).is_err());
    }

    #[test]
    fn group_states_are_kept_across_topology_changes() {
        let mut kitchen = GroupState::new(
            "kitchen".into(),
            "Kitchen".to_owned(),
            "k".into(),
            vec!["k".into()],
        );
        kitchen.volume = Some(20);
        let office = GroupState::new(
            "office".into(),
            "Office".to_owned(),
            "o".into(),
            vec!["o".into()],
        );
        let old_groups = [kitchen, office];

        let mut groups = [
            GroupState::new(
                "kitchen".into(),
                "Kitchen + Office".to_owned(),
                "k".into(),
                vec!["k".into(), "o".into()],
            ),
            GroupState::new(
                "bedroom".into(),
                "Bedroom".to_owned(),
                "b".into(),
                vec!["b".into()],
            ),
        ];
        keep_group_states(&mut groups, &old_groups);

        assert_eq!(groups[0].name, "Kitchen + Office");
        assert_eq!(groups[0].player_ids, ["k".into(), "o".into()]);
        assert_eq!(groups[0].volume, Some(20));
        assert_eq!(groups[1].name, "Bedroom");
        assert_eq!(groups[1].volume, None);
    }

    #[test]
    fn stations_that_do_not_say_what_they_play_are_shown_instead() {
        // As Radio France reports it for a live stream, playing or stopped.
        let metadata: MetadataStatus = serde_json::from_value(serde_json::json!({
            "_objectType": "metadataStatus",
            "container": {
                "_objectType": "container",
                "name": "franceinfo",
                "type": "station",
                "service": {"_objectType": "service", "name": "Radio France", "id": "585"},
                "imageUrl": "https://www.radiofrance.fr/franceinfo.jpg",
            },
            "streamInfo": "franceinfo-hifi.aac",
        }))
        .unwrap();
        let images = Images {
            coordinator_url: None,
            fallback: metadata.container.as_ref().unwrap().image_url.as_ref(),
        };
        let station = TrackInfo::station(&metadata, &images).unwrap();
        assert_eq!(station.title.as_deref(), Some("franceinfo-hifi.aac"));
        assert_eq!(station.artist, None);
        assert!(station.is_station);
        assert_eq!(
            station.image_url.map(String::from).as_deref(),
            Some("https://www.radiofrance.fr/franceinfo.jpg")
        );

        let without_stream_info = MetadataStatus {
            stream_info: None,
            ..metadata.clone()
        };
        let station = TrackInfo::station(&without_stream_info, &images).unwrap();
        assert_eq!(station.title.as_deref(), Some("franceinfo"));

        let playlist = MetadataStatus {
            container: metadata.container.clone().map(|container| {
                sinuous_client::playback_metadata::Container {
                    r#type: Some("playlist".to_owned()),
                    ..container
                }
            }),
            ..metadata
        };
        assert!(TrackInfo::station(&playlist, &images).is_none());
    }
}
