use std::{net::Ipv4Addr, str::FromStr, time::Duration};

use anyhow::{Result, anyhow};
use clap::ArgMatches;
use crossterm::event::{Event, EventStream};
use futures::TryStreamExt;
use ratatui::DefaultTerminal;
use tokio::{
    select,
    time::{self, MissedTickBehavior},
};
use tracing::{debug, warn};

use crate::{
    input,
    sonos::{self, ProvidedDevices},
    view::{self, UiState},
};

/// How often to redraw while what is displayed changes over time, e.g. the playback position.
const REDRAW_INTERVAL: Duration = Duration::from_millis(250);

pub struct App {
    devices: ProvidedDevices,
}

impl App {
    pub fn new(args: ArgMatches) -> Self {
        // Provided devices are given either as IPs or as names
        let mut devices = ProvidedDevices::default();

        // Iterate over the provided device argument, if present
        if let Some(provided_device) = args.get_one::<String>("device") {
            // Split the device argument by commas and iterate over the single provided devices
            for e in provided_device.split(',') {
                // Try to parse the element into an Ipv4Addr, if not possible accept it as a name
                if let Ok(ip) = Ipv4Addr::from_str(e) {
                    devices.ips.push(ip);
                } else {
                    devices.names.push(e.to_string());
                }
            }
        }
        App { devices }
    }

    pub async fn run(self, terminal: &mut DefaultTerminal) -> Result<()> {
        // Background service handling all the Sonos stuff
        let sonos = sonos::spawn(self.devices);
        let mut state_rx = sonos.state();
        // Whether the service is still running, i.e. whether the state may still change.
        let mut service_running = true;

        let mut ui = UiState::default();
        let mut events = EventStream::new();
        let mut redraw = time::interval(REDRAW_INTERVAL);
        redraw.set_missed_tick_behavior(MissedTickBehavior::Skip);

        debug!("Starting main loop...");
        loop {
            let state = state_rx.borrow_and_update().clone();
            ui.sync(&state);
            terminal.draw(|f| view::render_ui(f, &state, &ui))?;
            let animated = view::is_animated(&state, &ui);

            select! {
                event = events.try_next() => {
                    let event = event?.ok_or_else(|| anyhow!("Failed to receive keyboard input"))?;
                    if let Event::Key(key) = event {
                        if input::should_quit(&event) {
                            break;
                        }
                        if let Some(command) = view::handle_input(&key, &state, &mut ui) {
                            sonos.send(command);
                        }
                    }
                }
                _ = redraw.tick(), if animated => {}
                changed = state_rx.changed(), if service_running => {
                    if changed.is_err() {
                        // Keep displaying the last state, e.g. why we failed to connect.
                        warn!("Sonos service has stopped");
                        service_running = false;
                    }
                }
            }
        }

        Ok(())
    }
}
