//! The background service talking to the Sonos speakers.
//!
//! The service publishes a [`HouseholdState`] snapshot that the UI renders, and runs the
//! [`Command`]s the UI sends. The state is kept up to date from the events the speakers send,
//! rather than by polling them. Commands run in their own task, so that a slow speaker never holds
//! up state updates.

use std::collections::HashSet;
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use futures::StreamExt;
use sinuous_client::{
    Event, EventPayload, FavoriteId, GroupId, Household, PlayerId, Subscription, Topology,
    favorites::Favorite,
    playback::{
        LoadAction, LoadOptions, PlayModes, PlayModesUpdate, PlaybackActions, PlaybackState,
        PlaybackStatus,
    },
    playback_metadata::{MetadataStatus, QueueItem, Tag, Track},
};
use tokio::{
    select,
    sync::{broadcast, mpsc, watch},
    task::{JoinError, JoinSet},
    time::{self, MissedTickBehavior},
};
use tracing::{debug, error, info, warn};

const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(2);
/// How often to retry subscriptions that failed, e.g. because a speaker couldn't be reached. Once
/// made, subscriptions are restored by the household if a connection drops.
const RETRY_INTERVAL: Duration = Duration::from_secs(30);
/// Commands sent while this many are still waiting to run are dropped.
const COMMAND_QUEUE_SIZE: usize = 16;

/// The speakers to connect to, as given on the command line.
#[derive(Debug, Default)]
pub struct ProvidedDevices {
    pub ips: Vec<Ipv4Addr>,
    pub names: Vec<String>,
}

/// What the service knows about the household.
#[derive(Debug, Clone, Default)]
pub struct HouseholdState {
    pub status: ConnectionStatus,
    /// Sorted by name.
    pub groups: Vec<GroupState>,
    /// Sorted by name.
    pub players: Vec<PlayerInfo>,
    pub favorites: Arc<[Favorite]>,
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

#[derive(Debug, Clone, Default)]
pub enum ConnectionStatus {
    #[default]
    Connecting,
    Connected,
    Failed(String),
}

#[derive(Debug, Clone)]
pub struct ErrorMessage {
    pub message: String,
    pub at: Instant,
}

impl ErrorMessage {
    fn new(message: String) -> Self {
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
        }
    }
}

/// A player, and its volume once it has reported it.
#[derive(Debug, Clone)]
pub struct PlayerInfo {
    pub id: PlayerId,
    pub name: String,
    pub volume: Option<u8>,
}

#[derive(Debug, Clone, Copy)]
pub struct Playback {
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
}

#[derive(Debug, Clone)]
pub struct TrackInfo {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    /// Whether the album is tagged explicit. Tracks have no tags of their own.
    pub album_explicit: bool,
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
            album_explicit: track
                .album
                .as_ref()
                .is_some_and(|a| a.tags.contains(&Tag::TagExplicit)),
            duration_secs: track.duration_millis.map(millis_to_secs),
        }
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
    Pause,
    Next,
    Previous,
    AdjustVolume(i8),
    LoadFavorite(FavoriteId),
    /// Make these players the members of the group. The coordinator must come first.
    SetMembers(Vec<PlayerId>),
    SetPlayModes(PlayModesUpdate),
}

#[derive(Debug)]
pub enum PlayerAction {
    AdjustVolume(i8),
}

/// The UI's side of the service.
pub struct SonosHandle {
    state_rx: watch::Receiver<HouseholdState>,
    cmd_tx: mpsc::Sender<Command>,
}

impl SonosHandle {
    /// Watch the state of the household.
    ///
    /// The last state stays available once the service has stopped, e.g. after failing to
    /// connect.
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
}

/// Start the service in the background.
pub fn spawn(devices: ProvidedDevices) -> SonosHandle {
    let (state_tx, state_rx) = watch::channel(HouseholdState::default());
    let (cmd_tx, cmd_rx) = mpsc::channel(COMMAND_QUEUE_SIZE);

    tokio::spawn(async move {
        let household = match connect_household(devices).await {
            Ok(household) => household,
            Err(err) => {
                error!("Failed to connect: {err:#}");
                state_tx.send_modify(|state| {
                    state.status = ConnectionStatus::Failed(format!("{err:#}"));
                });
                return;
            }
        };

        tokio::spawn(run_commands(household.clone(), cmd_rx, state_tx.clone()));
        Service::new(household, state_tx).run().await;
    });

    SonosHandle { state_rx, cmd_tx }
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
    favorites_subscribed: bool,
    /// The version of the favorites in the state.
    favorites_version: Option<String>,
}

impl Service {
    fn new(household: Household, state_tx: watch::Sender<HouseholdState>) -> Self {
        Self {
            // Before subscribing to anything, so that we get the initial events.
            events: household.events(),
            topology_rx: household.topology_updates(),
            household,
            state_tx,
            subscribed: HashSet::new(),
            subscriptions: JoinSet::new(),
            favorites_subscribed: false,
            favorites_version: None,
        }
    }

    async fn run(mut self) {
        self.update_groups();
        self.state_tx.send_modify(|state| {
            state.status = ConnectionStatus::Connected;
        });
        // The favorites get loaded when the initial event arrives.
        self.subscribe_to_favorites().await;

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
            EventPayload::PlaybackError(err) => self.report_error(format!(
                "Playback error: {}",
                err.reason.unwrap_or(err.error_code)
            )),
            EventPayload::PlayerVolume(volume) => {
                let Some(id) = event.player_id else { return };
                self.state_tx.send_if_modified(|state| {
                    let Some(player) = state.players.iter_mut().find(|p| p.id == id) else {
                        return false;
                    };
                    player.volume = Some(volume.volume);
                    true
                });
            }
            payload => {
                let Some(id) = event.group_id else { return };
                self.state_tx.send_if_modified(|state| {
                    let Some(group) = state.groups.iter_mut().find(|g| g.id == id) else {
                        return false;
                    };
                    match payload {
                        EventPayload::PlaybackStatus(status) => {
                            group.playback = Some(Playback::new(&status, Instant::now()));
                        }
                        EventPayload::MetadataStatus(metadata) => {
                            group.now_playing = metadata.current_item.as_ref().and_then(track_info);
                            group.next_track = metadata.next_item.as_ref().and_then(track_info);
                            group.source = Source::new(&metadata);
                        }
                        EventPayload::GroupVolume(volume) => group.volume = Some(volume.volume),
                        _ => return false,
                    }
                    true
                });
            }
        }
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

    /// Update the groups and players from the household's topology, keeping what we know about
    /// them.
    fn update_groups(&mut self) {
        let (groups, players) = {
            let topology = self.topology_rx.borrow_and_update();
            (group_states(&topology), player_infos(&topology))
        };
        self.state_tx.send_modify(|state| {
            let old_groups = std::mem::replace(&mut state.groups, groups);
            keep_group_states(&mut state.groups, &old_groups);
            let old_players = std::mem::replace(&mut state.players, players);
            keep_player_volumes(&mut state.players, &old_players);
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
                ]
            });
            let players = state
                .players
                .iter()
                .map(|player| Subscription::PlayerVolume(player.id.clone()));
            groups.chain(players).collect()
        };
        self.subscribed.retain(|s| wanted.contains(s));
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
            Ok((_, Ok(()))) => {}
            Ok((subscription, Err(err))) => {
                // Try again later.
                self.subscribed.remove(&subscription);
                self.report_error(format!("Failed to get updates from a speaker: {err}"));
            }
            Err(err) => error!("Subscription task failed: {err}"),
        }
    }

    async fn subscribe_to_favorites(&mut self) {
        match self.household.subscribe(&Subscription::Favorites).await {
            Ok(()) => self.favorites_subscribed = true,
            Err(err) => self.report_error(format!("Failed to get updates to favorites: {err}")),
        }
    }

    async fn retry_subscriptions(&mut self) {
        self.subscribe_to_members();
        if !self.favorites_subscribed {
            self.subscribe_to_favorites().await;
        }
    }

    /// Subscribe to everything again, to catch up with missed events: subscribing makes the
    /// speakers send their whole state.
    async fn resync(&mut self) {
        debug!("Resyncing");
        self.subscribed.clear();
        self.subscribe_to_members();
        self.subscribe_to_favorites().await;
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
    while let Some(command) = cmd_rx.recv().await {
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
            let options = LoadOptions {
                action: Some(LoadAction::Replace),
                play_on_completion: Some(true),
                ..Default::default()
            };
            group
                .load_favorite(favorite, &options)
                .await
                .context("Failed to play favorite")?;
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
    }
    Ok(())
}

/// Connect to the household of one of the provided speakers, or of the first speaker discovered
/// if none were provided.
async fn connect_household(devices: ProvidedDevices) -> Result<Household> {
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
            volume: None,
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
        }
    }
}

/// Copy the volumes of the players in `old_players` to the same players in `players`.
fn keep_player_volumes(players: &mut [PlayerInfo], old_players: &[PlayerInfo]) {
    for player in players {
        if let Some(old) = old_players.iter().find(|old| old.id == player.id) {
            player.volume = old.volume;
        }
    }
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
}
