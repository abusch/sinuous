use std::{net::Ipv4Addr, str::FromStr, time::Duration};

use anyhow::{Result, anyhow};
use crossterm::event::{Event, EventStream};
use futures::TryStreamExt;
use ratatui::DefaultTerminal;
use ratatui_image::picker::{Picker, ProtocolType};
use tokio::{
    select,
    time::{self, MissedTickBehavior},
};
use tracing::{debug, warn};

use crate::{
    artwork::Artwork,
    config::{Config, UiConfig},
    input,
    sonos::{self, ProvidedDevices},
    view::{self, UiState},
};

/// How often to redraw while what is displayed changes over time, e.g. the playback position.
const REDRAW_INTERVAL: Duration = Duration::from_millis(250);

pub struct App {
    devices: ProvidedDevices,
    ui_config: UiConfig,
}

impl App {
    pub fn new(config: Config) -> Self {
        // Provided devices are given either as IPs or as names
        let mut devices = ProvidedDevices::default();
        for device in config.core.devices {
            if let Ok(ip) = Ipv4Addr::from_str(&device) {
                devices.ips.push(ip);
            } else {
                devices.names.push(device);
            }
        }
        App {
            devices,
            ui_config: config.ui,
        }
    }

    pub async fn run(self, terminal: &mut DefaultTerminal) -> Result<()> {
        // Background service handling all the Sonos stuff
        let sonos = sonos::spawn(self.devices);
        let mut state_rx = sonos.state();
        // Whether the service is still running, i.e. whether the state may still change.
        let mut service_running = true;

        // Before reading key presses (see `query_picker`). Album art is never shown when disabled,
        // so don't bother querying the terminal then.
        let picker = if self.ui_config.album_art {
            query_picker()
        } else {
            Picker::halfblocks()
        };
        let (mut artwork, mut loaded_rx) = Artwork::new(picker);

        let mut ui = UiState::default();
        let mut events = EventStream::new();
        let mut redraw = time::interval(REDRAW_INTERVAL);
        redraw.set_missed_tick_behavior(MissedTickBehavior::Skip);

        debug!("Starting main loop...");
        loop {
            let state = state_rx.borrow_and_update().clone();
            ui.sync(&state);
            artwork.show(view::artwork_url(&state, &ui).filter(|_| self.ui_config.album_art));
            terminal.draw(|f| view::render_ui(f, &state, &ui, &mut artwork))?;
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
                Some(loaded) = loaded_rx.recv() => artwork.loaded(loaded),
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

/// Find out how to draw images in the terminal.
///
/// This must be called before reading key presses, as it reads the terminal's answers to queries.
fn query_picker() -> Picker {
    // Terminals that don't answer the queries delay startup, and the first key pressed may
    // be lost: ratatui-image keeps waiting for an answer in the background.
    let started = std::time::Instant::now();
    let mut picker = Picker::from_query_stdio().unwrap_or_else(|err| {
        warn!("Failed to query the terminal's graphics support: {err}");
        Picker::halfblocks()
    });
    debug!(
        protocol = ?picker.protocol_type(),
        font_size = ?picker.font_size(),
        elapsed = ?started.elapsed(),
        "Queried the terminal's graphics support"
    );
    // Zellij supports the Kitty protocol, but not yet the Unicode placeholders ratatui-image
    // draws Kitty images with (https://github.com/zellij-org/zellij/pull/5531), nor iTerm2's
    // protocol. It only reports Sixel when the terminal it runs in supports it.
    if std::env::var_os("ZELLIJ").is_some() && picker.protocol_type() != ProtocolType::Sixel {
        debug!("Running in Zellij: using coloured blocks for images");
        picker.set_protocol_type(ProtocolType::Halfblocks);
    }
    picker
}
