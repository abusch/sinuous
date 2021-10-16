use std::time::Duration;

use anyhow::Result;
use futures::TryStreamExt;
use sonor::{Speaker, Track, TrackInfo};
use tokio::select;
use tokio::sync::mpsc::Receiver;
use tokio::sync::mpsc::Sender;
use tracing::{debug, error, warn};

use crate::Action;
use crate::Update;

#[derive(Debug)]
pub struct SpeakerState {
    pub is_playing: bool,
    pub speaker_name: String,
    pub now_playing: Option<TrackInfo>,
    pub queue: Vec<Track>,
}

pub async fn main_loop(speaker: Speaker, update_tx: Sender<Update>, cmd_rx: Receiver<Action>) {
    if let Err(err) = inner_loop(speaker, update_tx, cmd_rx).await {
        error!(%err, "Sonos error");
    }
}

async fn inner_loop(speaker: Speaker, update_tx: Sender<Update>, mut cmd_rx: Receiver<Action>) -> Result<()> {
        let mut ticker = tokio::time::interval(Duration::from_secs(1));
        debug!("Starting sonos loop");
        loop {
            select! {
                _tick = ticker.tick() => {
                    let speaker_state = match get_state(&speaker).await {
                        Ok(speaker_state) => speaker_state,
                        Err(err) => {
                            warn!(%err, "Failed to get state from speaker");
                            continue;
                        }
                    };

                    if let Err(err) = update_tx.send(Update::NewState(speaker_state)).await {
                        warn!(%err, "Updates channel was closed: exiting");
                        break;
                    }
                }
                cmd = cmd_rx.recv() => {
                    match cmd {
                        Some(Action::Play) => speaker.play().await.unwrap_or_else(|_e| {}),
                        Some(Action::Pause) => speaker.pause().await.unwrap_or_else(|_e| {}),
                        Some(Action::Next) => speaker.next().await.unwrap_or_else(|_e| {}),
                        Some(Action::Prev) => speaker.previous().await.unwrap_or_else(|_e| {}),
                        Some(_) => {}, // Nop
                        None => {
                            warn!("Command channel was closed: exiting...");
                            break;
                        }
                    }
                }
            }
        }
        Ok(())
}

pub async fn get_state(speaker: &Speaker) -> Result<SpeakerState> {
    let is_playing = speaker.is_playing().await?;
    let name = speaker.name().await?;
    let current_track = speaker.track().await?;
    let queue = speaker.queue().await?;

    Ok(SpeakerState {
        is_playing,
        speaker_name: name,
        now_playing: current_track,
        queue,
    })
}

pub async fn get_speaker() -> Result<Speaker> {
    let mut devices = sonor::discover(Duration::from_secs(2)).await?;

    let mut speakers = vec![];
    while let Some(device) = devices.try_next().await? {
        let is_playing = device.is_playing().await?;
        speakers.push((device, is_playing));
    }

    // Return the first speaker that is currently playing, or the first speaker we find otherwise
    let speaker_idx = speakers
        .iter()
        .position(|(_, is_playing)| *is_playing)
        .unwrap_or(0);

    speakers
        .into_iter()
        .skip(speaker_idx)
        .next()
        .map(|(speaker, _)| speaker)
        .ok_or(anyhow::anyhow!("Unable to find a speaker on the network"))
}
