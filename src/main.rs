use anyhow::{anyhow, Result};
use crossterm::event::{Event, EventStream};
use futures::TryStreamExt;
use tokio::{select, sync::mpsc};
use tracing::warn;
use tracing_subscriber::EnvFilter;

mod input;
mod sonos;
mod term;
mod view;

use crate::sonos::SpeakerState;

#[derive(Debug)]
pub enum State {
    Ready(SpeakerState),
    Connecting,
}

#[derive(Debug)]
pub enum Action {
    Play,
    Pause,
    Next,
    Prev,
    NextSpeaker,
    PrevSpeaker,
    Nop,
}

#[derive(Debug)]
pub enum Update {
    NewState(SpeakerState),
    Nop,
}

#[tokio::main]
async fn main() -> Result<()> {
    init_logger();

    let mut state = State::Connecting;

    // Channel used to send SpeakerState updates from SonosService to the UI
    let (update_tx, mut update_rx) = mpsc::channel(2);
    // Channel to send commands from the UI to SonosService
    let (cmd_tx, cmd_rx) = mpsc::channel(2);

    // Background service handling all the Sonos stuff
    let sonos = sonos::SonosService::new(update_tx, cmd_rx);
    sonos.start();

    // Initialize the terminal user interface.
    let (mut terminal, _cleanup) = term::init_crossterm()?;

    let mut events = EventStream::new();

    loop {
        select! {
            event = events.try_next() => {
                let event = event?.ok_or_else(|| anyhow!("Failed to receive keyboard input"))?;
                match event {
                    Event::Key(key) => {
                        if input::should_quit(&event) {
                            break;
                        }
                        if let State::Ready(ref speaker_state) = state {
                            let cmd = view::handle_input(&key, speaker_state);
                            cmd_tx.send(cmd).await?;
                        }
                    }
                    Event::Mouse(_mouse) => {},
                    Event::Resize(_, _) => {},
                }
            }
            update = update_rx.recv() => match update {
                Some(Update::NewState(speaker_state)) => state = State::Ready(speaker_state),
                Some(_) => {},
                None => {
                    // channel was closed for some reason...
                    warn!("Update channel was closed: exiting main loop");
                    break;
                }
            }
        }
        if let State::Ready(ref speaker_state) = state {
            terminal.draw(|f| view::render_ui(f, speaker_state))?;
        }
    }

    Ok(())
}

fn init_logger() {
    // Initialize logging framework
    let rolling = tracing_appender::rolling::never(std::env::temp_dir(), "sinuous.log");
    let (appender, _guard) = tracing_appender::non_blocking(rolling);
    tracing_subscriber::fmt::SubscriberBuilder::default()
        .with_writer(appender)
        .with_env_filter(EnvFilter::from_default_env())
        .with_ansi(false)
        .init();
}
