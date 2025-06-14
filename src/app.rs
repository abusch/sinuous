use std::{net::Ipv4Addr, str::FromStr};

use anyhow::{Result, anyhow};
use clap::ArgMatches;
use crossterm::event::{Event, EventStream};
use futures::TryStreamExt;
use ratatui::DefaultTerminal;
use tokio::{select, sync::mpsc};
use tracing::{debug, warn};

use crate::{State, Update, input, sonos, view};

pub struct App {
    provided_ips: Vec<Ipv4Addr>,
    provided_names: Vec<String>,
}

impl App {
    pub fn new(args: ArgMatches) -> Self {
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
        App {
            provided_ips,
            provided_names,
        }
    }

    pub async fn run(self, terminal: &mut DefaultTerminal) -> Result<()> {
        let mut state = State::Connecting;

        // Channel used to send SpeakerState updates from SonosService to the UI
        let (update_tx, mut update_rx) = mpsc::channel(2);
        // Channel to send commands from the UI to SonosService
        let (cmd_tx, cmd_rx) = mpsc::channel(2);

        // Background service handling all the Sonos stuff
        let sonos = sonos::SonosService::new(update_tx, cmd_rx);
        sonos.start((self.provided_ips, self.provided_names));

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
}
