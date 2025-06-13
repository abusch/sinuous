use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{Context, Result};
use futures::TryStreamExt;
use sonor::{Speaker, SpeakerInfo, Track, TrackInfo};
use std::net::Ipv4Addr;
use tokio::{
    select,
    sync::mpsc::{Receiver, Sender},
};
use tracing::{debug, error, info, warn};

use crate::{Action, Update};

#[derive(Debug)]
pub struct SpeakerState {
    pub is_playing: bool,
    pub current_volume: u16,
    pub group_names: Vec<String>,
    pub selected_group: usize,
    pub now_playing: Option<TrackInfo>,
    pub queue: Vec<Track>,
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
}

impl SonosService {
    pub fn new(update_tx: Sender<Update>, cmd_rx: Receiver<Action>) -> Self {
        Self {
            update_tx,
            cmd_rx,
            speakers_by_uuid: BTreeMap::new(),
            groups: vec![],
            selected_group: 0,
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

        let group_list = groups
            .into_iter()
            .map(|(uuid, speaker_list)| SpeakerGroup::new(uuid, speaker_list))
            .collect::<Vec<_>>();
        self.groups = group_list;
        self.speakers_by_uuid = speakers_by_uuid;

        let mut ticker = tokio::time::interval(Duration::from_secs(1));
        debug!("Starting sonos loop");

        loop {
            select! {
                _tick = ticker.tick() => {
                    // time to refresh our state
                    self.send_update().await;
                }
                cmd = self.cmd_rx.recv() => {
                    if let Some(c) = cmd {
                        self.handle_command(c).await.unwrap_or_else(|e| warn!("{}", e));
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

    async fn handle_command(&mut self, cmd: Action) -> Result<()> {
        debug!(?cmd, "Handling command");
        let current_speaker = self.current_speaker().context("No selected group")?;
        match cmd {
            Action::Play => current_speaker.play().await,
            Action::Pause => current_speaker.pause().await,
            Action::Next => current_speaker.next().await,
            Action::Prev => current_speaker.previous().await,
            Action::VolAdjust(v) => current_speaker.set_volume_relative(v).await.map(drop),
            Action::NextSpeaker => {
                self.select_next_group();
                Ok(())
            }
            Action::PrevSpeaker => {
                self.select_prev_group();
                Ok(())
            }
            Action::Nop => Ok(()), // Nop
        }
        .context("Error while handling command")
    }

    async fn send_update(&self) {
        match self.get_state().await {
            Ok(speaker_state) => {
                if let Err(err) = self
                    .update_tx
                    .send(Update::NewState(Box::new(speaker_state)))
                    .await
                {
                    warn!(%err, "Updates channel was closed: exiting");
                }
            }
            Err(err) => warn!(%err, "Failed to get state from speaker"),
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

    async fn get_state(&self) -> Result<SpeakerState> {
        let speaker = self.current_speaker().context("No selected group")?;
        let is_playing = speaker.is_playing().await?;
        let current_volume = speaker.volume().await?;
        let current_track = speaker.track().await?;
        let queue = speaker.queue().await?;
        let mut names = vec![];
        for group in &self.groups {
            names.push(group.name());
        }

        Ok(SpeakerState {
            is_playing,
            current_volume,
            group_names: names,
            selected_group: self.selected_group,
            now_playing: current_track,
            queue,
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
