use anyhow::{anyhow, Result};
use clap::{arg, command};
use crossterm::event::{Event, EventStream};
use futures::TryStreamExt;
use std::net::Ipv4Addr;
use std::str::FromStr;
use tokio::{select, sync::mpsc};
use tracing::{debug, error, info, warn};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{fmt::format::FmtSpan, EnvFilter};

mod input;
mod sonos;
mod term;
mod view;

use crate::sonos::SpeakerState;

#[derive(Debug)]
pub enum State {
    Ready(Box<SpeakerState>),
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
    VolAdjust(i16),
    Nop,
}

#[derive(Debug)]
pub enum Update {
    NewState(Box<SpeakerState>),
    Nop,
}

#[tokio::main]
async fn main() {
    human_panic::setup_panic!();
    // if a panic happens, we want to reset the terminal first so the backtraces and panic info can
    // be visible on the screen
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        term::reset_term().unwrap();
        prev(info);
    }));

    let _guard = init_logger();
    info!("Welcome to Sinuous!");

    // Set the App with clap to accept Command Line Arguments
    let args = command!()
        .arg(
            arg!(
                -d --device <device> "Specify a speaker to connect to. Provide either an Ipv4 Address or a name to search for. Multiple values are possible by seperating them with a comma"
            )
            .required(false)
        )
        .get_matches();

    // Set two Vectors: One for provided IPs, one for provided device names
    let mut provided_ips: Vec<Ipv4Addr> = Vec::new();
    let mut provided_names: Vec<String> = Vec::new();

    // Iterate over the provided device argument, if present
    if let Some(provided_device) = args.get_one::<String>("device") {
        // Split the device argument by commas and iterate over the single provided devices
        for e in provided_device.split(',') {
            // Try to parse the element into an Ipv4Addr, if not possible accept it as a name
            if let Ok(ip) = Ipv4Addr::from_str(e) {
                provided_ips.push(ip);
            } else {
                provided_names.push(e.to_string());
            }
        }
    }

    if let Err(err) = run_app(provided_names, provided_ips).await {
        error!("Main loop exited with error: {}", err);
    } else {
        info!("Bye!");
    }
}

async fn run_app(provided_names: Vec<String>, provided_ips: Vec<Ipv4Addr>) -> Result<()> {
    let mut state = State::Connecting;

    // Channel used to send SpeakerState updates from SonosService to the UI
    let (update_tx, mut update_rx) = mpsc::channel(2);
    // Channel to send commands from the UI to SonosService
    let (cmd_tx, cmd_rx) = mpsc::channel(2);

    // Background service handling all the Sonos stuff
    let sonos = sonos::SonosService::new(update_tx, cmd_rx);
    sonos.start((provided_ips, provided_names));

    // Initialize the terminal user interface.
    let (mut terminal, _cleanup) = term::init_crossterm()?;

    let mut events = EventStream::new();

    debug!("Starting main loop...");
    loop {
        select! {
            event = events.try_next() => {
                let event = event?.ok_or_else(|| anyhow!("Failed to receive keyboard input"))?;
                if let Event::Key(key) = event {
                    if input::should_quit(&event) {
                        break;
                    }
                    if let State::Ready(ref speaker_state) = state {
                        let cmd = view::handle_input(&key, speaker_state);
                        cmd_tx.send(cmd).await?;
                    }
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

fn init_logger() -> WorkerGuard {
    // Initialize logging framework
    let rolling = tracing_appender::rolling::never(std::env::temp_dir(), "sinuous.log");
    let (appender, guard) = tracing_appender::non_blocking(rolling);
    tracing_subscriber::fmt::SubscriberBuilder::default()
        .with_writer(appender)
        .with_env_filter(EnvFilter::from_default_env())
        .with_ansi(false)
        .with_span_events(FmtSpan::FULL)
        .init();
    guard
}
