//! The bar at the bottom of the screen, with the keys to use in what is shown.

use ratatui::{
    Frame,
    layout::{Constraint, Layout, Margin, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Block,
};

use crate::sonos::{GroupQueue, GroupState, HouseholdState, QueueContents};
use crate::theme::Theme;

use super::{Mode, Tab, UiState};

/// A key, and what it does.
type Shortcut = (&'static str, &'static str);

/// The most common keys on the main screen; the help lists them all.
const NOW_PLAYING: &[Shortcut] = &[
    ("SPACE", "play/pause"),
    ("p/n", "prev/next"),
    ("[/]", "volume"),
    ("l", "queue"),
    ("g", "groups"),
    (".", "playback"),
];
const GROUP_PICKER: &[Shortcut] = &[("↑↓", "navigate"), ("ENTER", "select"), ("ESC", "close")];
const GROUP_EDITOR: &[Shortcut] = &[
    ("↑↓", "navigate"),
    ("SPACE", "toggle"),
    ("ENTER", "apply"),
    ("ESC", "cancel"),
];
const BROWSER: &[Shortcut] = &[
    ("TAB", "switch"),
    ("↑↓", "navigate"),
    ("ENTER", "play"),
    ("ESC", "close"),
];
/// On the queue tab while the group doesn't play from its queue, which has nothing to play.
const BROWSER_VIEW: &[Shortcut] = &[("TAB", "switch"), ("↑↓", "navigate"), ("ESC", "close")];
const SYSTEM_INFO: &[Shortcut] = &[("↑↓", "scroll"), ("PGUP/PGDN", "page"), ("ESC", "close")];
const VOLUME_MIXER: &[Shortcut] = &[("↑↓", "navigate"), ("←→", "adjust"), ("ESC", "close")];
const MENU: &[Shortcut] = &[("↑↓", "navigate"), ("SPACE", "change"), ("ESC", "close")];
const SLEEP_TIMER_PICKER: &[Shortcut] =
    &[("↑↓", "navigate"), ("ENTER", "choose"), ("ESC", "cancel")];
const THEME_PICKER: &[Shortcut] = &[("↑↓", "preview"), ("ENTER", "choose"), ("ESC", "cancel")];
const HELP: &[Shortcut] = &[("ESC", "close")];
/// Only on the main screen: elsewhere, these keys do something else, or nothing.
const GLOBAL: &[Shortcut] = &[("?", "help"), (",", "config"), ("q", "quit")];

/// The keys to use in what is shown, on the left, and on the main screen, the keys for the help,
/// the configuration dialog and quitting, on the right.
pub(super) fn render_footer(
    state: &HouseholdState,
    ui: &UiState,
    group: &GroupState,
    theme: &Theme,
    frame: &mut Frame,
    area: Rect,
) {
    frame.render_widget(
        Block::new().style(Style::default().bg(theme.title_background)),
        area,
    );
    let global = match ui.mode {
        Mode::NowPlaying => shortcuts_line(GLOBAL, usize::MAX, theme),
        _ => Line::default(),
    };
    let global_width = u16::try_from(global.width()).unwrap_or(u16::MAX);
    let [context_area, global_area] =
        Layout::horizontal([Constraint::Min(1), Constraint::Length(global_width)])
            .spacing(3)
            .areas(area.inner(Margin::new(1, 0)));
    let context = shortcuts_line(
        shortcuts(state, ui, group),
        usize::from(context_area.width),
        theme,
    );
    frame.render_widget(context, context_area);
    frame.render_widget(global, global_area);
}

/// The keys to use in what is shown, in `ui`.
fn shortcuts(state: &HouseholdState, ui: &UiState, group: &GroupState) -> &'static [Shortcut] {
    match &ui.mode {
        Mode::NowPlaying => NOW_PLAYING,
        Mode::GroupPicker(_) => GROUP_PICKER,
        Mode::GroupEditor(_) => GROUP_EDITOR,
        // Its tracks can only be skipped to while the group plays from it.
        Mode::Browser(browser) if browser.tab == Tab::Queue => {
            match GroupQueue::of(state.queue.as_ref(), &group.id) {
                QueueContents::Loaded(queue) if group.plays_from(queue) => BROWSER,
                QueueContents::Loaded(_) | QueueContents::Loading | QueueContents::Failed => {
                    BROWSER_VIEW
                }
            }
        }
        Mode::Browser(_) => BROWSER,
        Mode::SystemInfo(_) => SYSTEM_INFO,
        Mode::VolumeMixer(_) => VOLUME_MIXER,
        Mode::PlaybackMenu {
            sleep_timer_picker: Some(_),
            ..
        } => SLEEP_TIMER_PICKER,
        Mode::Config {
            theme_picker: Some(_),
            ..
        } => THEME_PICKER,
        Mode::PlaybackMenu { .. } | Mode::Config { .. } => MENU,
        Mode::Help => HELP,
    }
}

/// `shortcuts`, as many as fit in `max_width` columns.
fn shortcuts_line(shortcuts: &[Shortcut], max_width: usize, theme: &Theme) -> Line<'static> {
    let hint = Style::default().fg(theme.hint);
    let mut line = Line::default();
    for (i, &(key, label)) in shortcuts.iter().enumerate() {
        let mut spans = Vec::new();
        if i > 0 {
            spans.push(Span::styled(" • ", hint));
        }
        spans.push(Span::styled(
            key,
            Style::default().fg(theme.key).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(format!(" {label}"), hint));
        let width: usize = spans.iter().map(Span::width).sum();
        if line.width() + width > max_width {
            break;
        }
        line.spans.extend(spans);
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_shortcuts_that_fit_are_shown() {
        let line = |width| shortcuts_line(MENU, width, &Theme::TERMINAL).to_string();
        assert_eq!(line(usize::MAX), "↑↓ navigate • SPACE change • ESC close");
        assert_eq!(line(30), "↑↓ navigate • SPACE change");
        assert_eq!(line(5), "");
    }
}
