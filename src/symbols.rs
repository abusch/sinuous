//! What actions and statuses are shown as: text labels, or icons for terminals using a
//! [Nerd Font](https://www.nerdfonts.com).

#[derive(Debug)]
pub struct Symbols {
    // Playback status
    pub playing: &'static str,
    pub paused: &'static str,
    /// Shown before the volume of the current group.
    pub volume_level: &'static str,

    // Play modes
    pub shuffle: &'static str,
    pub repeat: &'static str,
    pub repeat_one: &'static str,
    pub crossfade: &'static str,

    // Actions
    pub next: &'static str,
    pub previous: &'static str,
    pub volume: &'static str,
    pub mixer: &'static str,
    pub favorites: &'static str,
    pub groups: &'static str,
    pub edit_group: &'static str,
}

impl Symbols {
    const LABELS: Self = Self {
        playing: "⏵",
        paused: "⏸",
        volume_level: "🔊:",
        shuffle: "shuffle",
        repeat: "repeat",
        repeat_one: "repeat one",
        crossfade: "crossfade",
        next: "next",
        previous: "prev",
        volume: "volume",
        mixer: "mixer",
        favorites: "favorites",
        groups: "groups",
        edit_group: "edit group",
    };

    /// Material Design icons, see https://www.nerdfonts.com/cheat-sheet.
    const NERD_FONT: Self = Self {
        playing: "\u{f040a}",      // nf-md-play
        paused: "\u{f03e4}",       // nf-md-pause
        volume_level: "\u{f057e}", // nf-md-volume_high
        shuffle: "\u{f049f}",      // nf-md-shuffle_variant
        repeat: "\u{f0456}",       // nf-md-repeat
        repeat_one: "\u{f0458}",   // nf-md-repeat_once
        crossfade: "\u{f0915}",    // nf-md-transition
        next: "\u{f04ad}",         // nf-md-skip_next
        previous: "\u{f04ae}",     // nf-md-skip_previous
        volume: "\u{f057e}",       // nf-md-volume_high
        mixer: "\u{f066a}",        // nf-md-tune_vertical
        favorites: "\u{f02d1}",    // nf-md-heart
        groups: "\u{f04c3}",       // nf-md-speaker
        edit_group: "\u{f0d38}",   // nf-md-speaker_multiple
    };

    /// Nerd Font icons if `nerd_font`, text labels otherwise.
    pub fn choose(nerd_font: bool) -> &'static Self {
        if nerd_font {
            &Self::NERD_FONT
        } else {
            &Self::LABELS
        }
    }
}
