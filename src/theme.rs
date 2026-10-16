//! The colours of the UI: the terminal's own, or those of an [opaline](https://docs.rs/opaline)
//! theme.

use std::sync::LazyLock;

use opaline::names::tokens;
use ratatui::style::{Color, Style};

/// The name of the theme using the terminal's colours.
pub const TERMINAL: &str = "terminal";

/// A theme that can be chosen.
#[derive(Debug)]
pub struct ThemeEntry {
    /// The name to use in the configuration file.
    pub name: String,
    pub display_name: String,
}

/// All the themes, the terminal's colours first.
pub static THEMES: LazyLock<Vec<ThemeEntry>> = LazyLock::new(|| {
    let mut themes: Vec<_> = opaline::list_available_themes()
        .into_iter()
        .map(|info| ThemeEntry {
            name: info.name,
            display_name: info.display_name,
        })
        .collect();
    themes.sort_by(|a, b| a.display_name.cmp(&b.display_name));
    themes.insert(
        0,
        ThemeEntry {
            name: TERMINAL.to_owned(),
            display_name: "Terminal colours".to_owned(),
        },
    );
    themes
});

/// The display name of the theme called `name`, or `name` itself if there is no such theme.
pub fn display_name(name: &str) -> &str {
    THEMES
        .iter()
        .find(|theme| theme.name == name)
        .map_or(name, |theme| &theme.display_name)
}

/// The colours of the UI, by what they are used for.
#[derive(Debug, Clone)]
pub struct Theme {
    /// Behind everything, if not the terminal's.
    pub background: Option<Color>,
    /// Behind popups, if not the terminal's.
    pub popup_background: Option<Color>,
    /// Behind the highlighted line of popups, if anything.
    pub selection: Option<Color>,
    pub text: Color,
    /// Less important text, e.g. labels.
    pub secondary: Color,
    /// Labels standing out from their values, where both matter, e.g. in the system info.
    pub label: Color,
    /// Unimportant text, e.g. separators, and actions that aren't supported.
    pub muted: Color,
    /// The name of the app, and titles.
    pub title: Color,
    pub title_background: Color,
    /// Names: of the current group, and of the service playing.
    pub name: Color,
    pub artist: Color,
    /// What is on, e.g. play modes, and the progress bar.
    pub on: Color,
    pub error: Color,
    /// The volume sliders.
    pub slider: Color,
    pub slider_handle: Color,
    /// The empty part of the progress bar and of sliders.
    pub track: Color,
    /// The explicit badge.
    pub badge: Style,
    /// Where the album art goes when there is none to show.
    pub art_placeholder: Style,
    pub border: Color,
}

impl Theme {
    /// The terminal's own colours.
    pub const TERMINAL: Self = Self {
        background: None,
        popup_background: None,
        selection: None,
        text: Color::Reset,
        secondary: Color::Gray,
        label: Color::Cyan,
        muted: Color::DarkGray,
        title: Color::Yellow,
        title_background: Color::DarkGray,
        name: Color::Green,
        artist: Color::Cyan,
        on: Color::LightGreen,
        error: Color::Red,
        slider: Color::Cyan,
        slider_handle: Color::White,
        track: Color::DarkGray,
        badge: Style::new().fg(Color::Black).bg(Color::Gray),
        art_placeholder: Style::new().fg(Color::Gray).bg(Color::DarkGray),
        border: Color::Reset,
    };

    /// The theme called `name`, if there is one.
    pub fn named(name: &str) -> Option<Self> {
        if name == TERMINAL {
            return Some(Self::TERMINAL);
        }
        let theme = opaline::load_by_name(name)?;
        let color = |token| Color::from(theme.color(token));
        Some(Self {
            background: Some(color(tokens::BG_BASE)),
            popup_background: Some(color(tokens::BG_PANEL)),
            selection: Some(color(tokens::BG_SELECTION)),
            text: color(tokens::TEXT_PRIMARY),
            secondary: color(tokens::TEXT_SECONDARY),
            label: color(tokens::ACCENT_SECONDARY),
            muted: color(tokens::TEXT_DIM),
            title: color(tokens::ACCENT_PRIMARY),
            title_background: color(tokens::BG_HIGHLIGHT),
            name: color(tokens::ACCENT_TERTIARY),
            artist: color(tokens::ACCENT_SECONDARY),
            on: color(tokens::SUCCESS),
            error: color(tokens::ERROR),
            slider: color(tokens::ACCENT_SECONDARY),
            slider_handle: color(tokens::TEXT_PRIMARY),
            track: color(tokens::BG_HIGHLIGHT),
            badge: Style::new()
                .fg(color(tokens::BG_BASE))
                .bg(color(tokens::TEXT_SECONDARY)),
            art_placeholder: Style::new()
                .fg(color(tokens::TEXT_DIM))
                .bg(color(tokens::BG_HIGHLIGHT)),
            border: color(tokens::BORDER_FOCUSED),
        })
    }

    /// The style of the whole screen.
    pub fn base(&self) -> Style {
        with_background(Style::new().fg(self.text), self.background)
    }

    /// The style of popups.
    pub fn popup(&self) -> Style {
        with_background(Style::new().fg(self.text), self.popup_background)
    }
}

fn with_background(style: Style, background: Option<Color>) -> Style {
    match background {
        Some(color) => style.bg(color),
        None => style,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_themes_load() {
        for theme in THEMES.iter() {
            assert!(Theme::named(&theme.name).is_some(), "{}", theme.name);
        }
    }
}
