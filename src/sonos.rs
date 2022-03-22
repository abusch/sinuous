use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use futures::TryStreamExt;
use sonor::{Speaker, Track, TrackInfo};
use std::net::Ipv4Addr;
use tokio::select;
use tokio::sync::mpsc::Receiver;
use tokio::sync::mpsc::Sender;
use tracing::info;
use tracing::{debug, error, warn};

use crate::Action;
use crate::Update;

#[derive(Debug)]
pub struct SpeakerState {
    pub is_playing: bool,
    pub current_volume: u16,
    pub speaker_names: Vec<String>,
    pub selected_speaker: usize,
    pub now_playing: Option<TrackInfo>,
    pub queue: Vec<Track>,
}

impl SpeakerState {
    pub fn speaker_name(&self) -> &str {
        &self.speaker_names[self.selected_speaker]
    }
}

pub struct SonosService {
    update_tx: Sender<Update>,
    cmd_rx: Receiver<Action>,
    speakers: Vec<Speaker>,
    selected_speaker: usize,
}

impl SonosService {
    pub fn new(update_tx: Sender<Update>, cmd_rx: Receiver<Action>) -> Self {
        Self {
            update_tx,
            cmd_rx,
            speakers: vec![],
            selected_speaker: 0,
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
        self.speakers = get_speakers(provided_devices).await?;
        // let speaker = get_speaker().await?;
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
        match cmd {
            Action::Play => self.current_speaker().play().await,
            Action::Pause => self.current_speaker().pause().await,
            Action::Next => self.current_speaker().next().await,
            Action::Prev => self.current_speaker().previous().await,
            Action::VolAdjust(v) => self
                .current_speaker()
                .set_volume_relative(v)
                .await
                .map(drop),
            Action::NextSpeaker => {
                self.select_next_speaker();
                Ok(())
            }
            Action::PrevSpeaker => {
                self.select_prev_speaker();
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

    fn select_prev_speaker(&mut self) {
        if self.selected_speaker == 0 {
            self.selected_speaker = self.speakers.len();
        } else {
            self.selected_speaker -= 1;
        }
    }

    fn select_next_speaker(&mut self) {
        self.selected_speaker += 1;
        if self.selected_speaker >= self.speakers.len() {
            self.selected_speaker = 0;
        }
    }

    fn current_speaker(&self) -> &Speaker {
        &self.speakers[self.selected_speaker]
    }

    async fn get_state(&self) -> Result<SpeakerState> {
        let speaker = self.current_speaker();
        let is_playing = speaker.is_playing().await?;
        let current_volume = speaker.volume().await?;
        let current_track = speaker.track().await?;
        let queue = speaker.queue().await?;
        let mut names = vec![];
        for speaker in &self.speakers {
            names.push(speaker.name().await?);
        }

        // let groups = speaker.zone_group_state().await?;
        // debug!("zone_group_state: {:?}", groups);

        Ok(SpeakerState {
            is_playing,
            current_volume,
            speaker_names: names,
            selected_speaker: self.selected_speaker,
            now_playing: current_track,
            queue,
        })
    }
}

/* pub async fn get_speaker() -> Result<Speaker> {
    let speakers = get_speakers().await?;

    let mut speaker_idx = 0;
    // Try to find the first currently playing speaker if there is one
    for (i, speaker) in speakers.iter().enumerate() {
        let is_playing = speaker.is_playing().await?;
        if is_playing {
            speaker_idx = i;
            break;
        }
    }

    speakers
        .into_iter()
        .skip(speaker_idx)
        .next()
        .ok_or(anyhow::anyhow!("Unable to fin a speaker on the network"))
} */

async fn get_speakers(provided_devices: (Vec<Ipv4Addr>, Vec<String>)) -> Result<Vec<Speaker>> {
    let mut speakers: Vec<Speaker> = vec![];
    debug!("Connecting to provided speakers...");
    for e in provided_devices.0.iter() {
        if let Some(spk) = sonor::Speaker::from_ip(*e).await.unwrap_or(None) {
            speakers.push(spk);
        } else {
            debug!("Not connecting to {} due to errors", e);
        }
    }
    for e in provided_devices.1.iter() {
        if let Some(device) = sonor::find(e, Duration::from_secs(2)).await? {
            speakers.push(device);
        } else {
            debug!("Not connecting to {} due to errors", e);
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
