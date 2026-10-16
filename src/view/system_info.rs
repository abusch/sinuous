//! The system info: the details of each speaker, for troubleshooting.

use ratatui::{
    Frame,
    layout::{Margin, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, BorderType::Rounded, Padding, Paragraph, Scrollbar, ScrollbarOrientation,
        ScrollbarState,
    },
};
use sinuous_client::{
    groups::{Capability, DeviceInfo, Player},
    hardware_status::{
        BatteryStatus, ChargerValidity, ChargingState, ConnectionHealth, WirelessNetworkStatus,
    },
    playback::PlaybackState,
};

use crate::sonos::{HouseholdState, PlayerInfo};
use crate::theme::Theme;

use super::Scroll;

/// The keys to use in the system info.
const HELP: &str = " ↑↓ scroll • PGUP/PGDN page • ESC close ";

/// How wide labels are, for the values to line up.
const LABEL_WIDTH: usize = 14;

/// Below this charge, in percent, the battery is shown as low.
const LOW_BATTERY: f64 = 20.0;

/// How far the details of the devices of a speaker are indented.
const DEVICE_INDENT: usize = 2;

/// The details of the household and of each of its speakers, in `area`, scrolled to `scroll`.
pub(super) fn render_system_info(
    state: &HouseholdState,
    theme: &Theme,
    scroll: &mut Scroll,
    frame: &mut Frame,
    area: Rect,
) {
    let block = Block::bordered()
        .border_type(Rounded)
        .border_style(Style::default().fg(theme.border))
        .padding(Padding::horizontal(1))
        .title_top(" System info ")
        .title_bottom(
            Line::from(HELP)
                .centered()
                .style(Style::default().fg(theme.muted)),
        );
    let inner = block.inner(area);
    let lines = system_info_lines(state, theme, usize::from(inner.width));

    // Don't scroll past the last line.
    let len = lines.len();
    scroll.page = usize::from(inner.height);
    let max_offset = len.saturating_sub(scroll.page);
    scroll.offset = scroll.offset.min(max_offset);

    let offset = u16::try_from(scroll.offset).unwrap_or(u16::MAX);
    frame.render_widget(Paragraph::new(lines).block(block).scroll((offset, 0)), area);
    if max_offset > 0 {
        let mut scrollbar = ScrollbarState::new(max_offset).position(scroll.offset);
        frame.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .track_style(Style::default().fg(theme.border))
                .thumb_style(Style::default().fg(theme.secondary)),
            area.inner(Margin::new(0, 1)),
            &mut scrollbar,
        );
    }
}

/// The lines to show, with long lists wrapped to fit in `width`.
fn system_info_lines(state: &HouseholdState, theme: &Theme, width: usize) -> Vec<Line<'static>> {
    let rows = Rows { theme, width };
    let household = state
        .household_id
        .as_ref()
        .map_or_else(|| "unknown".to_owned(), ToString::to_string);
    let mut lines = vec![
        rows.row(0, "Household", household),
        rows.row(0, "Speakers", state.players.len().to_string()),
        rows.row(0, "Groups", state.groups.len().to_string()),
    ];
    for player in &state.players {
        lines.push(Line::default());
        lines.extend(player_lines(state, player, &rows));
    }
    lines
}

/// The details of `player`, and of each of its devices.
fn player_lines(state: &HouseholdState, player: &PlayerInfo, rows: &Rows) -> Vec<Line<'static>> {
    let theme = rows.theme;
    let mut lines = vec![Line::styled(
        player.name.clone(),
        Style::default()
            .fg(theme.title)
            .add_modifier(Modifier::BOLD),
    )];

    if let Some(group) = state
        .groups
        .iter()
        .find(|g| g.player_ids.contains(&player.id))
    {
        let mut value = group.name.clone();
        if group.coordinator_id == player.id {
            value.push_str(" · coordinator");
        }
        if let Some(playback) = group.playback {
            value.push_str(" · ");
            value.push_str(playback_state(playback.state));
        }
        lines.push(rows.row(0, "Group", value));
    }
    let volume = player
        .volume
        .map_or_else(|| "unknown".to_owned(), |volume| volume.to_string());
    lines.push(rows.row(0, "Volume", volume));
    if let Some(battery) = &player.battery {
        lines.push(rows.row(0, "Battery", battery_text(battery, theme)));
    }
    match &player.wifi {
        Some(wifi) => lines.extend(wifi_rows(wifi, rows)),
        None => lines.push(rows.row(
            0,
            "Wi-Fi",
            Span::styled("not reported", Style::default().fg(theme.muted)),
        )),
    }

    let Some(details) = state.topology.players.get(&player.id) else {
        return lines;
    };
    lines.extend([
        rows.row(0, "Address", address(&details.websocket_url)),
        rows.row(0, "ID", details.id.to_string()),
        rows.row(0, "Software", details.software_version.clone()),
        rows.row(
            0,
            "API version",
            format!("{} (min {})", details.api_version, details.min_api_version),
        ),
    ]);
    let registered = if details.is_unregistered {
        Span::styled("no", Style::default().fg(theme.error))
    } else {
        Span::raw("yes")
    };
    lines.push(rows.row(0, "Registered", registered));
    lines.extend(rows.list(0, "Capabilities", capabilities(details.capabilities.iter())));

    for device in &details.devices {
        lines.extend(device_lines(details, device, rows));
    }
    lines
}

/// The details of `device`, one of the devices making up `player`.
fn device_lines(player: &Player, device: &DeviceInfo, rows: &Rows) -> Vec<Line<'static>> {
    let mut lines = vec![
        rows.row(
            0,
            "Device",
            Span::styled(
                device.model_display_name.clone(),
                Style::default().fg(rows.theme.name),
            ),
        ),
        rows.row(DEVICE_INDENT, "Model", device.model.clone()),
    ];
    // The main device of a player has its ID and address.
    if device.id != player.id {
        lines.push(rows.row(DEVICE_INDENT, "ID", device.id.to_string()));
        lines.push(rows.row(DEVICE_INDENT, "Address", address(&device.websocket_url)));
    }
    lines.extend([
        rows.row(DEVICE_INDENT, "Serial", device.serial_number.clone()),
        rows.row(DEVICE_INDENT, "Hardware", device.hw_version.clone()),
        rows.row(
            DEVICE_INDENT,
            "Software",
            format!("{} (Sonos S{})", device.software_version, device.sw_gen),
        ),
    ]);
    if let Some(color) = &device.color {
        lines.push(rows.row(DEVICE_INDENT, "Colour", color.clone()));
    }
    lines
}

/// The charge of `battery`, with whether it is charging, and its temperature.
fn battery_text(battery: &BatteryStatus, theme: &Theme) -> Span<'static> {
    let mut text = format!("{:.0}%", battery.battery_percentage);
    if let Some(raw) = battery
        .raw_battery_percentage
        .filter(|raw| raw.round() != battery.battery_percentage.round())
    {
        text.push_str(&format!(" (measured {raw:.0}%)"));
    }
    text.push_str(match battery.charging_state {
        ChargingState::Charging => " · charging",
        ChargingState::NotCharging => " · not charging",
        ChargingState::Unknown => "",
    });
    if battery.valid_charger == ChargerValidity::Valid {
        text.push_str(" · on its charger");
    }
    if let Some(temperature) = battery.battery_temperature {
        text.push_str(&format!(" · {temperature:.1} °C"));
    }
    let low = battery.battery_percentage < LOW_BATTERY
        && battery.charging_state != ChargingState::Charging;
    if low {
        Span::styled(text, Style::default().fg(theme.error))
    } else {
        Span::raw(text)
    }
}

/// The network `wifi` is connected to, and to which access point and how.
fn wifi_rows(wifi: &WirelessNetworkStatus, rows: &Rows) -> Vec<Line<'static>> {
    let health = match wifi.connection_health {
        ConnectionHealth::Excellent => "excellent",
        ConnectionHealth::Good => "good",
        // Players may report weaker connections with values the client doesn't know yet.
        ConnectionHealth::Unknown => "other",
    };
    let mut lines = vec![rows.row(0, "Wi-Fi", format!("{} · {health}", wifi.ssid))];

    let mut access_point = Vec::new();
    if let Some(bssid) = &wifi.bssid {
        access_point.push(bssid.clone());
    }
    if let Some(frequency) = wifi.frequency {
        access_point.push(band(frequency).to_owned());
    }
    if let Some(channel) = wifi.channel {
        access_point.push(format!("channel {channel}"));
    }
    if let Some(rssi) = wifi.rssi {
        access_point.push(format!("signal {rssi}"));
    }
    if !access_point.is_empty() {
        lines.push(rows.row(0, "Access point", access_point.join(" · ")));
    }
    lines
}

/// The Wi-Fi band a frequency in MHz is in.
fn band(frequency: u32) -> &'static str {
    match frequency {
        ..3000 => "2.4 GHz",
        3000..5925 => "5 GHz",
        5925.. => "6 GHz",
    }
}

/// Lays out labelled values.
struct Rows<'a> {
    theme: &'a Theme,
    /// How wide lines may be.
    width: usize,
}

impl Rows<'_> {
    /// `value`, labelled with `label`, indented by `indent`.
    fn row(&self, indent: usize, label: &str, value: impl Into<Span<'static>>) -> Line<'static> {
        let label = format!(
            "{}{label:<width$}",
            " ".repeat(indent),
            width = LABEL_WIDTH - indent
        );
        Line::from(vec![
            Span::styled(label, Style::default().fg(self.theme.label)),
            value.into(),
        ])
    }

    /// `items`, separated by commas and labelled with `label`, wrapped onto as many lines as
    /// needed for each of them to fit.
    fn list(&self, indent: usize, label: &str, items: Vec<&str>) -> Vec<Line<'static>> {
        let available = self.width.saturating_sub(LABEL_WIDTH).max(1);
        let mut values: Vec<String> = Vec::new();
        for (i, item) in items.iter().enumerate() {
            let item = if i + 1 < items.len() {
                format!("{item},")
            } else {
                (*item).to_owned()
            };
            match values.last_mut() {
                Some(line) if line.len() + 1 + item.len() <= available => {
                    line.push(' ');
                    line.push_str(&item);
                }
                _ => values.push(item),
            }
        }
        if values.is_empty() {
            values.push("none".to_owned());
        }
        values
            .into_iter()
            .enumerate()
            .map(|(i, value)| self.row(indent, if i == 0 { label } else { "" }, value))
            .collect()
    }
}

/// The IP address (or host name) of a speaker, from the URL of its websocket.
fn address(websocket_url: &url::Url) -> String {
    websocket_url
        .host_str()
        .map_or_else(|| websocket_url.to_string(), ToOwned::to_owned)
}

fn playback_state(state: PlaybackState) -> &'static str {
    match state {
        PlaybackState::Buffering => "buffering",
        PlaybackState::Idle => "idle",
        PlaybackState::Paused => "paused",
        PlaybackState::Playing => "playing",
        PlaybackState::Unknown => "unknown",
    }
}

/// The names of `capabilities`, as the Sonos API calls them, to be looked up in its documentation.
fn capabilities(capabilities: impl Iterator<Item = Capability>) -> Vec<&'static str> {
    capabilities
        .map(|capability| match capability {
            Capability::Playback => "PLAYBACK",
            Capability::Cloud => "CLOUD",
            Capability::HtPlayback => "HT_PLAYBACK",
            Capability::HtPowerState => "HT_POWER_STATE",
            Capability::Airplay => "AIRPLAY",
            Capability::LineIn => "LINE_IN",
            Capability::AudioClip => "AUDIO_CLIP",
            Capability::Voice => "VOICE",
            Capability::SpeakerDetection => "SPEAKER_DETECTION",
            Capability::FixedVolume => "FIXED_VOLUME",
            Capability::MicrophoneSwitch => "MICROPHONE_SWITCH",
            Capability::IrControl => "IR_CONTROL",
            Capability::Hdmi => "HDMI",
            Capability::Svc => "SVC",
            Capability::Unknown => "unknown",
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crossterm::event::KeyCode;
    use ratatui::{Terminal, backend::TestBackend};
    use sinuous_client::Topology;

    use super::*;
    use crate::artwork::Artwork;
    use crate::config::UiConfig;
    use crate::view::tests::{household, press};
    use crate::view::{UiState, render_ui};

    /// The bedroom, as a soundbar with a sub.
    fn bedroom() -> Player {
        let device = |id: &str, model: &str, name: &str, ip: &str| {
            serde_json::json!({
                "_objectType": "deviceInfo",
                "id": id,
                "name": "Bedroom",
                "serialNumber": format!("{id}-serial"),
                "model": model,
                "modelDisplayName": name,
                "color": "Black",
                "capabilities": ["PLAYBACK"],
                "apiVersion": "1.44.1",
                "minApiVersion": "1.1.0",
                "websocketUrl": format!("wss://{ip}:1443/websocket/api"),
                "softwareVersion": "85.0-65270",
                "hwVersion": "1.36.1.2-1.0",
                "swGen": 2,
            })
        };
        serde_json::from_value(serde_json::json!({
            "_objectType": "player",
            "id": "b",
            "name": "Bedroom",
            "websocketUrl": "wss://10.0.0.2:1443/websocket/api",
            "apiVersion": "1.44.1",
            "minApiVersion": "1.1.0",
            "softwareVersion": "85.0-65270",
            "capabilities": [
                "PLAYBACK", "CLOUD", "HT_PLAYBACK", "HT_POWER_STATE", "AIRPLAY",
                "AUDIO_CLIP", "VOICE", "SPEAKER_DETECTION", "FIXED_VOLUME",
            ],
            "isUnregistered": false,
            "deviceIds": ["b", "sub"],
            "devices": [
                device("b", "S45", "Arc Ultra", "10.0.0.2"),
                device("sub", "S41", "Sub 4", "10.0.0.3"),
            ],
        }))
        .unwrap()
    }

    fn screen(state: &HouseholdState, ui: &mut UiState, width: u16, height: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        let (mut artwork, _) = Artwork::new(ratatui_image::picker::Picker::halfblocks(), None);
        terminal
            .draw(|f| {
                render_ui(
                    f,
                    state,
                    ui,
                    &mut artwork,
                    &UiConfig::default(),
                    &Theme::TERMINAL,
                );
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect()
            })
            .collect()
    }

    fn state() -> HouseholdState {
        let mut topology = Topology::default();
        topology.players.insert_overwrite(bedroom());
        let mut state = HouseholdState {
            household_id: Some("Sonos_1".into()),
            topology: Arc::new(topology),
            ..household()
        };
        let bedroom = &mut state.players[0];
        bedroom.battery = Some(
            serde_json::from_value(serde_json::json!({
                "_objectType": "battery",
                "batteryPercentage": 15.0,
                "rawBatteryPercentage": 12.0,
                "chargingState": "NOT_CHARGING",
                "batteryTemperature": 31.5,
                "validCharger": "NOT_APPLICABLE",
            }))
            .unwrap(),
        );
        bedroom.wifi = Some(
            serde_json::from_value(serde_json::json!({
                "_objectType": "wirelessNetworkStatus",
                // "Home", base64-encoded
                "ssid": "SG9tZQ==",
                "bssid": "aa:bb:cc:dd:ee:ff",
                "frequency": 5320,
                "channel": 64,
                "rssi": 45,
                "connectionHealth": "EXCELLENT",
            }))
            .unwrap(),
        );
        state
    }

    #[test]
    fn shows_the_details_of_speakers_and_their_devices() {
        let state = state();
        let mut ui = UiState::default();
        ui.sync(&state);
        press(&[KeyCode::Char('i')], &state, &mut ui);

        let screen = screen(&state, &mut ui, 80, 60).join("\n");
        for shown in [
            "Household     Sonos_1",
            "Group         Bedroom + Office · coordinator",
            "Address       10.0.0.2",
            "Battery       15% (measured 12%) · not charging · 31.5 °C",
            "Wi-Fi         Home · excellent",
            "Access point  aa:bb:cc:dd:ee:ff · 5 GHz · channel 64 · signal 45",
            // The kitchen hasn't reported its Wi-Fi, e.g. because it's wired.
            "Wi-Fi         not reported",
            "Device        Arc Ultra",
            "  Model       S45",
            "Device        Sub 4",
            "  Address     10.0.0.3",
            "  Software    85.0-65270 (Sonos S2)",
            // Speakers the topology doesn't know about yet still show what is known.
            "Kitchen",
        ] {
            assert!(screen.contains(shown), "{shown}\n{screen}");
        }
        // The capabilities are wrapped, and lined up.
        assert!(
            screen.contains("Capabilities  PLAYBACK, CLOUD,"),
            "{screen}"
        );
        assert!(
            screen.contains("              AUDIO_CLIP, VOICE, SPEAKER_DETECTION"),
            "{screen}"
        );
        // The main device has the speaker's address, so it isn't repeated.
        assert_eq!(screen.matches("10.0.0.2").count(), 1, "{screen}");
    }

    #[test]
    fn scrolls_no_further_than_the_last_line() {
        let state = state();
        let mut ui = UiState::default();
        ui.sync(&state);
        press(&[KeyCode::Char('i'), KeyCode::End], &state, &mut ui);

        let at_end = screen(&state, &mut ui, 60, 12);
        // The title bar, then the border: the last line is just above the bottom border.
        assert!(at_end[10].contains("Wi-Fi"), "{at_end:#?}");

        press(&[KeyCode::Down], &state, &mut ui);
        assert_eq!(screen(&state, &mut ui, 60, 12), at_end);
    }
}
