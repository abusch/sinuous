//! The background service talking to the Sonos speakers.
//!
//! The service publishes a [`HouseholdState`] snapshot that the UI renders, and runs the
//! [`Command`]s the UI sends. Commands run in their own task, so that a slow speaker never holds up
//! state updates.

use std::collections::HashSet;
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use futures::StreamExt;
use sinuous_client::{
    FavoriteId, GroupId, Household, Topology,
    favorites::Favorite,
    playback::{LoadAction, LoadOptions, PlaybackState},
    playback_metadata::{QueueItem, Track},
};
use tokio::{
    select,
    sync::{mpsc, watch},
    time::MissedTickBehavior,
};
use tracing::{debug, error, info, warn};

const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(2);
const REFRESH_INTERVAL: Duration = Duration::from_secs(1);
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
    pub favorites: Arc<[Favorite]>,
    pub last_error: Option<ErrorMessage>,
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

#[derive(Debug, Clone)]
pub struct GroupState {
    pub id: GroupId,
    pub name: String,
    /// `None` until fetched. Only the focused group is kept up to date.
    pub playback: Option<Playback>,
}

#[derive(Debug, Clone)]
pub struct Playback {
    pub is_playing: bool,
    pub volume: u8,
    pub now_playing: Option<TrackInfo>,
    pub elapsed_secs: u32,
    pub next_track: Option<TrackInfo>,
}

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

/// Something to do with a group.
#[derive(Debug)]
pub struct Command {
    pub group: GroupId,
    pub action: GroupAction,
}

#[derive(Debug)]
pub enum GroupAction {
    Play,
    Pause,
    Next,
    Previous,
    AdjustVolume(i8),
    LoadFavorite(FavoriteId),
}

/// The UI's side of the service.
pub struct SonosHandle {
    state_rx: watch::Receiver<HouseholdState>,
    cmd_tx: mpsc::Sender<Command>,
    focus_tx: watch::Sender<Option<GroupId>>,
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

    /// Set the group the UI is showing, which is the one the service keeps up to date.
    pub fn focus(&self, group: Option<&GroupId>) {
        self.focus_tx.send_if_modified(|focused| {
            if focused.as_ref() == group {
                false
            } else {
                *focused = group.cloned();
                true
            }
        });
    }
}

/// Start the service in the background.
pub fn spawn(devices: ProvidedDevices) -> SonosHandle {
    let (state_tx, state_rx) = watch::channel(HouseholdState::default());
    let (cmd_tx, cmd_rx) = mpsc::channel(COMMAND_QUEUE_SIZE);
    let (focus_tx, focus_rx) = watch::channel(None);

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

        let (done_tx, done_rx) = mpsc::unbounded_channel();
        tokio::spawn(run_commands(household.clone(), cmd_rx, done_tx));
        Service {
            household,
            state_tx,
            focus_rx,
            done_rx,
        }
        .run()
        .await;
    });

    SonosHandle {
        state_rx,
        cmd_tx,
        focus_tx,
    }
}

/// Keeps the [`HouseholdState`] up to date.
struct Service {
    household: Household,
    state_tx: watch::Sender<HouseholdState>,
    focus_rx: watch::Receiver<Option<GroupId>>,
    /// Outcomes of the commands run by [`run_commands`].
    done_rx: mpsc::UnboundedReceiver<Result<(), String>>,
}

impl Service {
    async fn run(mut self) {
        self.update_groups();
        self.state_tx.send_modify(|state| {
            state.status = ConnectionStatus::Connected;
        });
        self.load_favorites().await;

        let mut ticker = tokio::time::interval(REFRESH_INTERVAL);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        debug!("Starting sonos loop");

        loop {
            select! {
                _ = ticker.tick() => {}
                changed = self.focus_rx.changed() => {
                    if changed.is_err() {
                        debug!("UI is gone: exiting...");
                        break;
                    }
                }
                done = self.done_rx.recv() => {
                    let Some(result) = done else { break };
                    self.report(result);
                    // Refresh once for all the commands that completed.
                    while let Ok(result) = self.done_rx.try_recv() {
                        self.report(result);
                    }
                }
            }
            self.refresh().await;
        }
        self.household.close().await;
    }

    async fn load_favorites(&mut self) {
        debug!("Fetching favorites...");
        match self.household.connection().get_favorites().await {
            Ok(favorites) => {
                info!("Found {} favorites", favorites.items.len());
                self.state_tx.send_modify(|state| {
                    state.favorites = favorites.items.into();
                });
            }
            Err(err) => self.report_error(format!("Failed to fetch favorites: {err}")),
        }
    }

    async fn refresh(&mut self) {
        // Groups can be created and modified at any time, e.g. from the Sonos app.
        self.update_groups();

        let focused = self.focus_rx.borrow_and_update().clone();
        let Some(id) = focused else { return };
        if !self.state_tx.borrow().groups.iter().any(|g| g.id == id) {
            // The UI will pick another group.
            return;
        }
        match fetch_playback(&self.household, &id).await {
            Ok(playback) => self.state_tx.send_modify(|state| {
                if let Some(group) = state.groups.iter_mut().find(|g| g.id == id) {
                    group.playback = Some(playback);
                }
            }),
            Err(err) => self.report_error(format!("Failed to refresh state: {err:#}")),
        }
    }

    /// Update the groups from the household's topology, keeping what we know about them.
    fn update_groups(&self) {
        let groups = group_states(&self.household.topology());
        self.state_tx.send_modify(|state| {
            let mut old_groups = std::mem::replace(&mut state.groups, groups);
            for group in &mut state.groups {
                if let Some(old) = old_groups.iter_mut().find(|old| old.id == group.id) {
                    group.playback = old.playback.take();
                }
            }
        });
    }

    fn report(&self, result: Result<(), String>) {
        if let Err(message) = result {
            self.report_error(message);
        }
    }

    fn report_error(&self, message: String) {
        warn!("{message}");
        self.state_tx.send_modify(|state| {
            state.last_error = Some(ErrorMessage::new(message));
        });
    }
}

async fn fetch_playback(household: &Household, id: &GroupId) -> Result<Playback> {
    let group = household.group(id).await?;
    let (status, volume, metadata) = tokio::try_join!(
        group.get_playback_status(),
        group.get_volume(),
        group.get_metadata_status(),
    )?;

    Ok(Playback {
        is_playing: matches!(
            status.playback_state,
            PlaybackState::Playing | PlaybackState::Buffering
        ),
        volume: volume.volume,
        elapsed_secs: status.position_millis.map_or(0, millis_to_secs),
        now_playing: metadata.current_item.and_then(|item| track_info(&item)),
        next_track: metadata.next_item.and_then(|item| track_info(&item)),
    })
}

/// Run commands one at a time, in the order they were sent, and report their outcome.
async fn run_commands(
    household: Household,
    mut cmd_rx: mpsc::Receiver<Command>,
    done_tx: mpsc::UnboundedSender<Result<(), String>>,
) {
    while let Some(command) = cmd_rx.recv().await {
        debug!(?command, "Running command");
        let result = run_command(&household, &command)
            .await
            .map_err(|err| format!("{err:#}"));
        if done_tx.send(result).is_err() {
            break;
        }
    }
}

async fn run_command(household: &Household, command: &Command) -> Result<()> {
    let group = household.group(&command.group).await?;
    match &command.action {
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
            GroupState {
                id: group.id.clone(),
                name,
                playback: None,
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
