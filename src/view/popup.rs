//! The popups shown in the middle of the screen.

use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType::Rounded, Clear, HighlightSpacing, List, ListState, Padding},
};

use crate::config::{UiConfig, UiSetting};
use crate::sonos::{GroupState, HouseholdState};
use crate::symbols::Symbols;
use crate::theme::{self, THEMES, Theme};

use super::input::{Action, supports};
use super::now_playing::{sleep_timer_text, volume_slider, volume_text};
use super::panel::speaker_name;
use super::{PlaybackItem, SLEEP_TIMER_MINUTES};

/// A popup with the volume of `group` and of each of its members, with the row at `highlighted`
/// highlighted.
pub(super) fn render_volume_mixer(
    state: &HouseholdState,
    group: &GroupState,
    theme: &Theme,
    highlighted: usize,
    frame: &mut Frame,
) {
    const BAR_WIDTH: usize = 20;

    let rows: Vec<_> = std::iter::once((Line::from("Group"), group.volume))
        .chain(state.members(group).map(|player| {
            let label = speaker_name(player, Style::default(), theme);
            (Line::from(label), player.volume)
        }))
        .collect();
    let label_width = rows
        .iter()
        .map(|(label, _)| label.width())
        .max()
        .unwrap_or(0);

    // Leave room for the sliders, which are drawn over the lines afterwards.
    let lines = rows
        .iter()
        .map(|(label, volume)| {
            let padding = " ".repeat(label_width - label.width() + 2);
            let mut spans = label.spans.clone();
            spans.extend([
                Span::raw(padding),
                Span::raw(" ".repeat(BAR_WIDTH)),
                Span::raw(format!(" {}", volume_text(*volume))),
            ]);
            Line::from(spans)
        })
        .collect();

    let (area, offset) = render_popup(
        frame,
        theme,
        " Volume ",
        lines,
        Some(highlighted),
        " ↑↓ Navigate • ←→ adjust • ESC close ",
    );

    let bar_x = area.x + u16::try_from(label_width + 2).unwrap_or(u16::MAX);
    let bar_width = u16::try_from(BAR_WIDTH).unwrap_or(u16::MAX);
    for (y, (_, volume)) in (area.top()..area.bottom()).zip(rows.into_iter().skip(offset)) {
        let bar_area = Rect::new(bar_x, y, bar_width, 1).intersection(area);
        frame.render_widget(volume_slider(volume, theme), bar_area);
    }
}

/// A popup listing what can be changed about the playback of `group`, with the item at
/// `highlighted` highlighted. What its source doesn't support is dimmed.
pub(super) fn render_playback_menu(
    group: &GroupState,
    symbols: &Symbols,
    theme: &Theme,
    highlighted: usize,
    frame: &mut Frame,
) {
    let modes = group.playback.map(|p| p.play_modes).unwrap_or_default();
    let lines = PlaybackItem::ALL
        .into_iter()
        .map(|item| {
            let (on, action, description) = match item {
                PlaybackItem::Crossfade => (modes.crossfade, Action::Crossfade, "Crossfade"),
                PlaybackItem::SleepTimer => return sleep_timer_line(group, symbols, theme),
            };
            let checkbox = if on {
                symbols.checked
            } else {
                symbols.unchecked
            };
            let (checkbox_style, style) = match (supports(group, action), on) {
                (false, _) => (
                    Style::default().fg(theme.muted),
                    Style::default().fg(theme.muted),
                ),
                (true, true) => (Style::default().fg(theme.on), Style::default()),
                (true, false) => (Style::default().fg(theme.secondary), Style::default()),
            };
            Line::from(vec![
                Span::styled(checkbox, checkbox_style),
                Span::raw(" "),
                Span::styled(description, style),
            ])
        })
        .collect();

    render_popup(
        frame,
        theme,
        " Playback ",
        lines,
        Some(highlighted),
        " ↑↓ Navigate • SPACE change • ESC close ",
    );
}

/// The time left on the sleep timer of `group`, if it is set, lined up with the checkboxes above
/// it.
fn sleep_timer_line(group: &GroupState, symbols: &Symbols, theme: &Theme) -> Line<'static> {
    let padding = Span::raw(symbols.checked)
        .width()
        .saturating_sub(Span::raw(symbols.sleep_timer).width());
    let value = match group.sleep_timer {
        Some(end) => Span::styled(
            format!("{} left", sleep_timer_text(end)),
            Style::default().fg(theme.name),
        ),
        None => Span::styled("off", Style::default().fg(theme.secondary)),
    };
    Line::from(vec![
        Span::styled(symbols.sleep_timer, Style::default().fg(theme.secondary)),
        Span::raw(" ".repeat(padding + 1)),
        Span::raw("Sleep timer: "),
        value,
    ])
}

/// A popup listing what the sleep timer can be set to, with the choice at `highlighted`
/// highlighted.
pub(super) fn render_sleep_timer_picker(theme: &Theme, highlighted: usize, frame: &mut Frame) {
    let lines = SLEEP_TIMER_MINUTES
        .into_iter()
        .map(|minutes| match minutes {
            None => Line::from("Off"),
            Some(60) => Line::from("1 hour"),
            Some(minutes) if minutes % 60 == 0 => Line::from(format!("{} hours", minutes / 60)),
            Some(minutes) => Line::from(format!("{minutes} minutes")),
        })
        .collect();

    render_popup(
        frame,
        theme,
        " Sleep timer ",
        lines,
        Some(highlighted),
        " ↑↓ Navigate • ENTER choose • ESC cancel ",
    );
}

/// A popup listing all the keyboard shortcuts.
pub(super) fn render_help(theme: &Theme, frame: &mut Frame) {
    // The keys to use in the other popups are shown at their bottom.
    const SECTIONS: [(&str, &[(&str, &str)]); 2] = [
        (
            "Playback",
            &[
                ("SPACE", "Play / pause"),
                ("p / n", "Previous / next track"),
                ("s", "Toggle shuffle"),
                ("r", "Cycle repeat: all, one, off"),
                ("[ / ]", "Volume down / up"),
                (".", "Playback menu: crossfade, sleep timer"),
            ],
        ),
        (
            "General",
            &[
                ("v", "Volume of the group and of each speaker"),
                ("l", "Queue of the group"),
                ("h", "Recently played"),
                ("f", "Play a favorite"),
                ("P", "Play a playlist"),
                ("g", "Switch to another group"),
                ("G", "Add or remove speakers from the group"),
                ("i", "System info, for troubleshooting"),
                (",", "Configuration"),
                ("?", "This help"),
                ("q", "Close what is open, or quit"),
            ],
        ),
    ];

    let key_width = SECTIONS
        .iter()
        .flat_map(|(_, shortcuts)| shortcuts.iter())
        .map(|(key, _)| Span::raw(*key).width())
        .max()
        .unwrap_or(0);
    let mut lines = Vec::new();
    for (title, shortcuts) in SECTIONS {
        if !lines.is_empty() {
            lines.push(Line::default());
        }
        lines.push(Line::styled(
            title,
            Style::default()
                .fg(theme.title)
                .add_modifier(Modifier::BOLD),
        ));
        for (key, description) in shortcuts {
            let padding = " ".repeat(key_width - Span::raw(*key).width() + 2);
            lines.push(Line::from(vec![
                Span::raw(*key),
                Span::raw(padding),
                Span::styled(*description, Style::default().fg(theme.secondary)),
            ]));
        }
    }

    render_popup(
        frame,
        theme,
        " Keyboard shortcuts ",
        lines,
        None,
        " ESC close ",
    );
}

/// A popup listing the settings, with the one at `highlighted` highlighted: the ones that can be
/// toggled, then the theme.
pub(super) fn render_config_dialog(
    settings: &UiConfig,
    symbols: &Symbols,
    theme: &Theme,
    highlighted: usize,
    frame: &mut Frame,
) {
    let mut lines: Vec<_> = UiSetting::ALL
        .into_iter()
        .map(|setting| {
            let (checkbox, style) = if settings.get(setting) {
                (symbols.checked, Style::default().fg(theme.on))
            } else {
                (symbols.unchecked, Style::default().fg(theme.secondary))
            };
            Line::from(vec![
                Span::styled(checkbox, style),
                Span::raw(" "),
                Span::raw(setting.description()),
            ])
        })
        .collect();
    // Line the theme up with the settings above it.
    let padding = Span::raw(symbols.checked)
        .width()
        .saturating_sub(Span::raw(symbols.theme).width());
    lines.push(Line::from(vec![
        Span::styled(symbols.theme, Style::default().fg(theme.secondary)),
        Span::raw(" ".repeat(padding + 1)),
        Span::raw("Theme: "),
        Span::styled(
            theme::display_name(&settings.theme),
            Style::default().fg(theme.name),
        ),
    ]));

    render_popup(
        frame,
        theme,
        " Configuration ",
        lines,
        Some(highlighted),
        " ↑↓ Navigate • SPACE change • ESC close ",
    );
}

/// A popup listing the themes, with the current one (`current`) in colour, and the one at
/// `highlighted` highlighted, and shown.
pub(super) fn render_theme_picker(
    current: &str,
    theme: &Theme,
    highlighted: usize,
    frame: &mut Frame,
) {
    let lines = THEMES
        .iter()
        .map(|entry| {
            let style = if entry.name == current {
                Style::default().fg(theme.name)
            } else {
                Style::default()
            };
            Line::styled(entry.display_name.as_str(), style)
        })
        .collect();

    render_popup(
        frame,
        theme,
        " Theme ",
        lines,
        Some(highlighted),
        " ↑↓ Preview • ENTER choose • ESC cancel ",
    );
}

/// A popup in the middle of the screen, listing `lines` with the one at `highlighted`
/// highlighted, if any.
///
/// Returns where the lines are drawn, and the index of the first one shown, which may not be 0 if
/// they don't all fit.
fn render_popup(
    frame: &mut Frame,
    theme: &Theme,
    title: &str,
    lines: Vec<Line>,
    highlighted: Option<usize>,
    help: &str,
) -> (Rect, usize) {
    const HIGHLIGHT_SYMBOL: &str = "⏵ ";
    const PADDING: Padding = Padding::symmetric(2, 1);
    let help = Line::from(help)
        .centered()
        .style(Style::default().fg(theme.muted));

    // Fit the content, plus the padding and the borders
    let content_width = lines.iter().map(Line::width).max().unwrap_or(0)
        + Span::raw(HIGHLIGHT_SYMBOL).width()
        + usize::from(PADDING.left + PADDING.right);
    let width = u16::try_from(content_width.max(help.width()) + 2).unwrap_or(u16::MAX);
    let height = u16::try_from(lines.len() + usize::from(PADDING.top + PADDING.bottom) + 2)
        .unwrap_or(u16::MAX);
    let area = frame
        .area()
        .centered(Constraint::Length(width), Constraint::Length(height));

    let mut list_state = ListState::default();
    list_state.select(highlighted);
    let block = Block::bordered()
        .border_type(Rounded)
        .border_style(Style::default().fg(theme.border))
        .style(theme.popup())
        .padding(PADDING)
        .title_top(title)
        .title_bottom(help);
    let [_, lines_area] = Layout::horizontal([
        Constraint::Length(u16::try_from(Span::raw(HIGHLIGHT_SYMBOL).width()).unwrap_or(0)),
        Constraint::Fill(1),
    ])
    .areas(block.inner(area));
    let mut highlight_style = Style::default().add_modifier(Modifier::BOLD);
    if let Some(selection) = theme.selection {
        highlight_style = highlight_style.bg(selection);
    }
    let list = List::new(lines)
        .highlight_style(highlight_style)
        .highlight_symbol(HIGHLIGHT_SYMBOL)
        .highlight_spacing(HighlightSpacing::Always)
        .block(block);

    frame.render_widget(Clear, area);
    frame.render_stateful_widget(list, area, &mut list_state);
    (lines_area, list_state.offset())
}
