use anyhow::{anyhow, Result};
use crossterm::event::{Event, EventStream};
use futures::TryStreamExt;
use tokio::select;
use tracing::warn;
use tracing_subscriber::EnvFilter;

mod input;
mod sonos;
mod term;
mod view;

use crate::sonos::SpeakerState;

#[derive(Debug)]
pub struct State {
    speaker_state: SpeakerState,
}

impl State {
    pub fn new(speaker_state: SpeakerState) -> Self {
        Self { speaker_state }
    }
}

#[derive(Debug)]
pub enum Action {
    Play,
    Pause,
    Next,
    Prev,
    Nop,
}

#[derive(Debug)]
pub enum Update {
    NewState(SpeakerState),
    Nop,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging framework
    let rolling = tracing_appender::rolling::never(std::env::temp_dir(), "sinuous.log");
    let (appender, _guard) = tracing_appender::non_blocking(rolling);
    tracing_subscriber::fmt::SubscriberBuilder::default()
        .with_writer(appender)
        .with_env_filter(EnvFilter::from_default_env())
        .with_ansi(false)
        .init();

    let speaker = sonos::get_speaker().await?;
    let speaker_state = sonos::get_state(&speaker).await?;
    let mut state = State::new(speaker_state);

    let (update_tx, mut update_rx) = tokio::sync::mpsc::channel(2);
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel(2);

    tokio::spawn(sonos::main_loop(speaker, update_tx, cmd_rx));

    // Initialize the terminal user interface.
    let (mut terminal, _cleanup) = term::init_crossterm()?;
    terminal.clear()?;

    let mut events = EventStream::new();

    loop {
        select! {
            event = events.try_next() => {
                let event = event?.ok_or_else(|| anyhow!(""))?;
                match event {
                    Event::Key(key) => {
                        if input::should_quit(&event) {
                            break;
                        }
                        let cmd = view::handle_input(&key, &state);
                        cmd_tx.send(cmd).await?;
                    }
                    Event::Mouse(_mouse) => todo!(),
                    Event::Resize(_, _) => todo!(),
                }
            }
            update = update_rx.recv() => match update {
                Some(Update::NewState(speaker_state)) => state.speaker_state = speaker_state,
                Some(_) => {},
                None => {
                    // channel was closed for some reason...
                    warn!("Update channel was closed: exiting main loop");
                    break;
                }
            }
        }
        terminal.draw(|f| view::render_ui(f, &state))?;
    }

    Ok(())
}
