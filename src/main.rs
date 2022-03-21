use anyhow::{anyhow, Result};
use clap::{Arg, Command, crate_authors, crate_version};
use crossterm::event::{Event, EventStream};
use futures::TryStreamExt;
use std::net::{Ipv4Addr};
use std::str::FromStr;
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

    // Set the App with clap to accept Command Line Arguments
    let args = Command::new("Sinuous")
        .version(crate_version!())
        .author(crate_authors!("\n"))
        .about("A simple TUI for controlling Sonos speakers")
        .arg(
            Arg::new("device")
                .short('d')
                .long("device")
                .help("Specify a speaker to connect to. Provide either an Ipv4 Address or a name to search for. Multiple values are possible by seperating them with a comma")
                .takes_value(true)
        ).get_matches();

    // Set two Vectors: One for provided IPs, one for provided device names
    let mut provided_ips: Vec<Ipv4Addr> = Vec::new();
    let mut provided_names: Vec<String> = Vec::new();

    // Iterate over the provided device argument, if present
    if let Some(provided_device) = args.value_of("device") {
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
