//! What actions and statuses are shown as: text labels, or icons for terminals using a
//! [Nerd Font](https://www.nerdfonts.com).

use std::borrow::Cow;

use sinuous_client::hardware_status::{BatteryStatus, ChargingState};

use crate::sonos::battery_is_low;

#[derive(Debug)]
pub struct Symbols {
    // Playback status
    pub playing: &'static str,
    pub paused: &'static str,
    /// Idle groups, e.g. once a live radio stream is stopped.
    pub stopped: &'static str,
    // Volume levels, shown before the volume of the current group
    pub volume_off: &'static str,
    pub volume_low: &'static str,
    pub volume_medium: &'static str,
    pub volume_high: &'static str,

    // Play modes
    pub shuffle: &'static str,
    pub repeat: &'static str,
    pub repeat_one: &'static str,

    // Kinds of sources, e.g. a group playing an album
    pub album: &'static str,
    pub artist: &'static str,
    pub playlist: &'static str,
    pub radio: &'static str,
    pub podcast: &'static str,
    pub line_in: &'static str,
    /// Between the kind of a source and its name.
    pub source_kind_separator: &'static str,
    /// The icons of music services, by a word in their name, in lower case.
    services: &'static [(&'static str, &'static str)],

    // Actions
    pub next: &'static str,
    pub previous: &'static str,
    /// Shown before the name of the current group, with what it is playing.
    pub playing_on: &'static str,

    // Whether groups are playing, in the group panel
    pub group_playing: &'static str,
    pub group_paused: &'static str,
    pub group_stopped: &'static str,

    /// Where the album art goes when there is none to show.
    pub no_art: &'static str,
    /// After the name of explicit tracks.
    pub explicit: Badge,

    // Settings in the configuration dialog
    pub checked: &'static str,
    pub unchecked: &'static str,
    pub theme: &'static str,

    // In the playback menu
    pub sleep_timer: &'static str,

    // The battery of portable speakers, by tenths of charge, from empty to full
    battery: [&'static str; 11],
    battery_charging: [&'static str; 11],
    /// A battery running out.
    battery_low: &'static str,
}

/// A mark after the name of something, e.g. of explicit tracks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Badge {
    /// Text on a background, padded on both sides, e.g. ` E `.
    Label(&'static str),
    /// An icon that is a badge itself, with no background.
    Icon(&'static str),
}

impl Symbols {
    const LABELS: Self = Self {
        playing: "⏵",
        paused: "⏸",
        stopped: "⏹",
        volume_off: "🔇",
        volume_low: "🔈",
        volume_medium: "🔉",
        volume_high: "🔊",
        shuffle: "shuffle",
        repeat: "repeat",
        repeat_one: "repeat one",
        album: "Album",
        artist: "Artist",
        playlist: "Playlist",
        radio: "Radio",
        podcast: "Podcast",
        line_in: "Line-in",
        source_kind_separator: " · ",
        services: &[],
        next: "next",
        previous: "prev",
        playing_on: "Playing on",
        group_playing: "⏵",
        group_paused: "⏸",
        group_stopped: "⏹",
        no_art: "♪",
        explicit: Badge::Label(" E "),
        checked: "[x]",
        unchecked: "[ ]",
        theme: "🎨",
        sleep_timer: "🌙",
        battery: ["🔋"; 11],
        battery_charging: ["⚡"; 11],
        battery_low: "🪫",
    };

    /// Material Design icons, see https://www.nerdfonts.com/cheat-sheet.
    const NERD_FONT: Self = Self {
        playing: "\u{f040a}",       // nf-md-play
        paused: "\u{f03e4}",        // nf-md-pause
        stopped: "\u{f04db}",       // nf-md-stop
        volume_off: "\u{f0581}",    // nf-md-volume_off
        volume_low: "\u{f057f}",    // nf-md-volume_low
        volume_medium: "\u{f0580}", // nf-md-volume_medium
        volume_high: "\u{f057e}",   // nf-md-volume_high
        shuffle: "\u{f049f}",       // nf-md-shuffle_variant
        repeat: "\u{f0456}",        // nf-md-repeat
        repeat_one: "\u{f0458}",    // nf-md-repeat_once
        album: "\u{f0025}",         // nf-md-album
        artist: "\u{f0803}",        // nf-md-account_music
        playlist: "\u{f0cb8}",      // nf-md-playlist_music
        radio: "\u{f0439}",         // nf-md-radio
        podcast: "\u{f0994}",       // nf-md-podcast
        line_in: "\u{f186b}",       // nf-md-audio_input_rca
        source_kind_separator: " ",
        services: &[
            ("spotify", "\u{f04c7}"),    // nf-md-spotify
            ("apple", "\u{f0035}"),      // nf-md-apple
            ("amazon", "\u{f270}"),      // nf-fa-amazon
            ("soundcloud", "\u{f04c0}"), // nf-md-soundcloud
            ("youtube", "\u{f05c3}"),    // nf-md-youtube
            ("pandora", "\u{f03db}"),    // nf-md-pandora
            ("plex", "\u{f06ba}"),       // nf-md-plex
            ("audible", "\u{f0df}"),     // nf-fa-audible
            ("bandcamp", "\u{f2d5}"),    // nf-fa-bandcamp
            ("mixcloud", "\u{f289}"),    // nf-fa-mixcloud
        ],
        next: "\u{f04ad}",                  // nf-md-skip_next
        previous: "\u{f04ae}",              // nf-md-skip_previous
        playing_on: "\u{f04c3}",            // nf-md-speaker
        group_playing: "\u{f040c}",         // nf-md-play_circle
        group_paused: "\u{f03e5}",          // nf-md-pause_circle
        group_stopped: "\u{f0666}",         // nf-md-stop_circle
        no_art: "\u{f075a}",                // nf-md-music
        explicit: Badge::Icon("\u{f0b0c}"), // nf-md-alpha_e_box
        checked: "\u{f0132}",               // nf-md-checkbox_marked
        unchecked: "\u{f0131}",             // nf-md-checkbox_blank_outline
        theme: "\u{f03d8}",                 // nf-md-palette
        sleep_timer: "\u{f04b2}",           // nf-md-sleep
        battery: [
            "\u{f008e}", // nf-md-battery_outline
            "\u{f007a}", // nf-md-battery_10
            "\u{f007b}", // nf-md-battery_20
            "\u{f007c}", // nf-md-battery_30
            "\u{f007d}", // nf-md-battery_40
            "\u{f007e}", // nf-md-battery_50
            "\u{f007f}", // nf-md-battery_60
            "\u{f0080}", // nf-md-battery_70
            "\u{f0081}", // nf-md-battery_80
            "\u{f0082}", // nf-md-battery_90
            "\u{f0079}", // nf-md-battery
        ],
        battery_charging: [
            "\u{f089f}", // nf-md-battery_charging_outline
            "\u{f089c}", // nf-md-battery_charging_10
            "\u{f0086}", // nf-md-battery_charging_20
            "\u{f0087}", // nf-md-battery_charging_30
            "\u{f0088}", // nf-md-battery_charging_40
            "\u{f089d}", // nf-md-battery_charging_50
            "\u{f0089}", // nf-md-battery_charging_60
            "\u{f089e}", // nf-md-battery_charging_70
            "\u{f008a}", // nf-md-battery_charging_80
            "\u{f008b}", // nf-md-battery_charging_90
            "\u{f0085}", // nf-md-battery_charging_100
        ],
        battery_low: "\u{f0083}", // nf-md-battery_alert
    };

    /// The volume level `volume` is shown as.
    pub fn volume_level(&self, volume: Option<u8>) -> &'static str {
        match volume {
            None | Some(0) => self.volume_off,
            Some(1..=33) => self.volume_low,
            Some(34..=66) => self.volume_medium,
            Some(_) => self.volume_high,
        }
    }

    /// The icon showing how charged `battery` is, and whether it is charging.
    pub fn battery(&self, battery: &BatteryStatus) -> &'static str {
        if battery_is_low(battery) {
            return self.battery_low;
        }
        let tenths = (battery.battery_percentage / 10.0).round().clamp(0.0, 10.0) as usize;
        if battery.charging_state == ChargingState::Charging {
            self.battery_charging[tenths]
        } else {
            self.battery[tenths]
        }
    }

    /// The music service called `name`, after its icon if it has one.
    pub fn service<'a>(&self, name: &'a str) -> Cow<'a, str> {
        let lower = name.to_lowercase();
        match self.services.iter().find(|(word, _)| lower.contains(word)) {
            Some((_, icon)) => Cow::Owned(format!("{icon} {name}")),
            None => Cow::Borrowed(name),
        }
    }

    /// Nerd Font icons if `nerd_font`, text labels otherwise.
    pub fn choose(nerd_font: bool) -> &'static Self {
        if nerd_font {
            &Self::NERD_FONT
        } else {
            &Self::LABELS
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn services_get_their_icon_with_nerd_fonts_only() {
        let nerd_font = Symbols::choose(true);
        assert_eq!(nerd_font.service("Spotify"), "\u{f04c7} Spotify");
        assert_eq!(nerd_font.service("Apple Music"), "\u{f0035} Apple Music");
        assert_eq!(nerd_font.service("TIDAL"), "TIDAL");
        assert_eq!(Symbols::choose(false).service("Spotify"), "Spotify");
    }

    fn battery(percentage: f64, charging_state: &str) -> BatteryStatus {
        serde_json::from_value(serde_json::json!({
            "_objectType": "battery",
            "batteryPercentage": percentage,
            "chargingState": charging_state,
        }))
        .unwrap()
    }

    #[test]
    fn battery_icons_show_the_charge_and_whether_it_is_charging() {
        let nerd_font = Symbols::choose(true);
        assert_eq!(
            nerd_font.battery(&battery(100.0, "NOT_CHARGING")),
            "\u{f0079}"
        );
        assert_eq!(
            nerd_font.battery(&battery(64.0, "NOT_CHARGING")),
            "\u{f007f}"
        );
        assert_eq!(nerd_font.battery(&battery(66.0, "CHARGING")), "\u{f089e}");
        assert_eq!(nerd_font.battery(&battery(3.0, "CHARGING")), "\u{f089f}");
        // Running out, unless it is charging.
        assert_eq!(
            nerd_font.battery(&battery(15.0, "NOT_CHARGING")),
            "\u{f0083}"
        );
        assert_eq!(nerd_font.battery(&battery(15.0, "CHARGING")), "\u{f0086}");

        let labels = Symbols::choose(false);
        assert_eq!(labels.battery(&battery(64.0, "NOT_CHARGING")), "🔋");
        assert_eq!(labels.battery(&battery(64.0, "CHARGING")), "⚡");
        assert_eq!(labels.battery(&battery(15.0, "NOT_CHARGING")), "🪫");
    }
}
