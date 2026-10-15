//! The panel on the left, listing the groups or the speakers of one.

use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType::Rounded, Padding, Paragraph},
};

use crate::sonos::{GroupState, HouseholdState, PlayerInfo};
use crate::symbols::Symbols;
use crate::theme::Theme;

use super::{GROUP_EDITOR_HELP, GROUP_PANEL_HELP, GroupEditor, PanelHighlight};

/// How many lines each group takes in the group panel.
const GROUP_HEIGHT: u16 = 3;

/// A panel in `area` listing the groups and what they are playing, with the selected group (at
/// `selected`) in colour and the one at `highlight` highlighted.
pub(super) fn render_group_panel(
    state: &HouseholdState,
    symbols: &Symbols,
    theme: &Theme,
    selected: usize,
    highlight: &mut PanelHighlight,
    frame: &mut Frame,
    area: Rect,
) {
    let items = state
        .groups
        .iter()
        .enumerate()
        .map(|(i, group)| group_lines(group, i == selected, symbols, theme))
        .collect();
    render_panel(
        frame,
        theme,
        area,
        " Groups ",
        GROUP_PANEL_HELP,
        items,
        highlight,
    );
}

/// A panel titled `title` in `area`, listing `items` a line apart, scrolled so that the one at
/// `highlight` is shown, and highlighted, with the keys to use (`help`) at the bottom.
fn render_panel<const N: usize>(
    frame: &mut Frame,
    theme: &Theme,
    area: Rect,
    title: &str,
    help: &str,
    items: Vec<[Line; N]>,
    highlight: &mut PanelHighlight,
) {
    let help = Line::from(help)
        .centered()
        .style(Style::default().fg(theme.muted));
    let block = Block::bordered()
        .border_type(Rounded)
        .border_style(Style::default().fg(theme.border))
        .style(theme.popup())
        .padding(Padding::vertical(1))
        .title_top(title)
        .title_bottom(help);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Items are a line apart, and scrolled so that the highlighted one is shown.
    let item_height = u16::try_from(N).unwrap_or(u16::MAX);
    let shown = usize::from((inner.height + 1) / (item_height + 1)).max(1);
    highlight.scroll(shown);
    let PanelHighlight {
        item: highlighted,
        first,
    } = *highlight;
    let rows = (inner.y..).step_by(usize::from(item_height + 1));
    for ((i, lines), y) in items.into_iter().enumerate().skip(first).zip(rows) {
        let item_area = Rect {
            y,
            height: item_height,
            ..inner
        }
        .intersection(inner);
        if item_area.is_empty() {
            break;
        }
        // A bar on the left of the highlighted item, and room for it on the others.
        let [bar_area, _, lines_area] = Layout::horizontal([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Fill(1),
        ])
        .areas(item_area);
        if i == highlighted {
            if let Some(selection) = theme.selection {
                frame.render_widget(
                    Block::new().style(Style::default().bg(selection)),
                    item_area,
                );
            }
            let bar = vec![Line::from("▌"); usize::from(item_area.height)];
            frame.render_widget(
                Paragraph::new(bar).style(Style::default().fg(theme.title)),
                bar_area,
            );
        }
        frame.render_widget(Paragraph::new(Vec::from(lines)), lines_area);
    }
}

/// The lines showing `group` in the group panel: its name, whether it is playing, and what.
fn group_lines<'a>(
    group: &'a GroupState,
    selected: bool,
    symbols: &Symbols,
    theme: &Theme,
) -> [Line<'a>; GROUP_HEIGHT as usize] {
    let muted = Style::default().fg(theme.muted);
    let mut name_style = Style::default().add_modifier(Modifier::BOLD);
    if selected {
        name_style = name_style.fg(theme.name);
    }

    let (icon, status, status_style) = if group.playback.is_some_and(|p| p.is_playing) {
        (
            symbols.group_playing,
            "Playing",
            Style::default().fg(theme.on),
        )
    } else {
        (
            symbols.group_not_playing,
            "Not playing",
            Style::default().fg(theme.secondary),
        )
    };
    let mut status_line = vec![Span::styled(format!("{icon} {status}"), status_style)];
    if let Some(service) = group.source.as_ref().and_then(|s| s.service.as_deref()) {
        status_line.push(Span::styled(" · ", muted));
        status_line.push(Span::styled(
            symbols.service(service),
            Style::default().fg(theme.secondary),
        ));
    }

    // The track, lined up with the status after the icon
    let mut track_line = vec![Span::raw(" ".repeat(Span::raw(icon).width() + 1))];
    match &group.now_playing {
        Some(track) => track_line.extend([
            Span::raw(track.title.as_deref().unwrap_or("Unknown title")),
            Span::styled(" · ", muted),
            Span::styled(
                track.artist.as_deref().unwrap_or("Unknown artist"),
                Style::default().fg(theme.artist),
            ),
        ]),
        None => track_line.push(Span::styled("No content", muted)),
    }

    [
        Line::styled(group.name.as_str(), name_style),
        Line::from(status_line),
        Line::from(track_line),
    ]
}

/// A panel listing all the speakers in `area`, with the ones to have in the group being edited
/// checked.
pub(super) fn render_group_editor(
    state: &HouseholdState,
    symbols: &Symbols,
    theme: &Theme,
    editor: &mut GroupEditor,
    frame: &mut Frame,
    area: Rect,
) {
    let Some(group) = state.group(&editor.group) else {
        return;
    };
    let items = state
        .players
        .iter()
        .map(|player| speaker_lines(state, player, group, editor, symbols, theme))
        .collect();
    render_panel(
        frame,
        theme,
        area,
        " Group speakers ",
        GROUP_EDITOR_HELP,
        items,
        &mut editor.highlight,
    );
}

/// The lines showing `player` in the editor of `group`: its name and model, whether it is to be
/// in the group, and which group it is in now.
fn speaker_lines<'a>(
    state: &HouseholdState,
    player: &'a PlayerInfo,
    group: &GroupState,
    editor: &GroupEditor,
    symbols: &Symbols,
    theme: &Theme,
) -> [Line<'a>; 2] {
    let muted = Style::default().fg(theme.muted);
    let secondary = Style::default().fg(theme.secondary);
    let mut name_style = Style::default().add_modifier(Modifier::BOLD);
    let (checkbox, checkbox_style) = if editor.members.contains(&player.id) {
        name_style = name_style.fg(theme.name);
        (symbols.checked, Style::default().fg(theme.on))
    } else {
        (symbols.unchecked, secondary)
    };

    let current = state
        .groups
        .iter()
        .find(|g| g.player_ids.contains(&player.id));
    let location = match current {
        Some(current) if current.id == group.id => "In this group".to_owned(),
        Some(current) if current.player_ids.len() > 1 => format!("In {}", current.name),
        _ => "On its own".to_owned(),
    };
    let mut status_line = vec![
        Span::styled(checkbox, checkbox_style),
        Span::raw(" "),
        Span::styled(location, secondary),
    ];
    if player.id == group.coordinator_id {
        status_line.push(Span::styled(" · ", muted));
        status_line.push(Span::styled("coordinator", muted));
    }

    [
        Line::from(speaker_name(player, name_style, theme)),
        Line::from(status_line),
    ]
}

/// The name of `player` in `style`, followed by its model, if it is known.
pub(super) fn speaker_name<'a>(
    player: &'a PlayerInfo,
    style: Style,
    theme: &Theme,
) -> Vec<Span<'a>> {
    let mut spans = vec![Span::styled(player.name.as_str(), style)];
    if let Some(model) = &player.model {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            model.as_str(),
            Style::default().fg(theme.muted),
        ));
    }
    spans
}
