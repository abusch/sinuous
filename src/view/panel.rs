//! The panel on the left, listing the groups, the speakers of one, or things played or to play.

use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType::Rounded, Padding, Paragraph},
};

use sinuous_client::{
    favorites::Favorite,
    history::{ContentType, HistoryItem},
    playback::PlaybackState,
    playlists::PlaylistSummary,
    queue::QueueEntry,
};

use crate::sonos::{GroupQueue, GroupState, HouseholdState, PlayerInfo, QueueContents};
use crate::symbols::Symbols;
use crate::theme::Theme;

use super::now_playing::format_duration;
use super::{
    BROWSER_HELP, BROWSER_VIEW_HELP, Browser, GROUP_EDITOR_HELP, GROUP_PANEL_HELP, GroupEditor,
    PanelHighlight, Tab, can_play_again,
};

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
        Line::from(" Groups "),
        GROUP_PANEL_HELP,
        None,
        items,
        highlight,
    );
}

/// A panel titled `title` in `area`, listing `items` a line apart, scrolled so that the one at
/// `highlight` is shown, and highlighted, with the keys to use (`help`) at the bottom, and `note`
/// above the items, if any.
#[expect(clippy::too_many_arguments)]
fn render_panel<const N: usize>(
    frame: &mut Frame,
    theme: &Theme,
    area: Rect,
    title: Line,
    help: &str,
    note: Option<Line>,
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
    let mut inner = block.inner(area);
    frame.render_widget(block, area);
    if let Some(note) = note {
        let [note_area, _, rest] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Fill(1),
        ])
        .areas(inner);
        // Lined up with the items, after the room for the bar
        let [_, note_area] =
            Layout::horizontal([Constraint::Length(2), Constraint::Fill(1)]).areas(note_area);
        frame.render_widget(note, note_area);
        inner = rest;
    }

    // Items are a line apart, and scrolled so that the highlighted one is shown.
    let item_height = u16::try_from(N).unwrap_or(u16::MAX);
    let shown = usize::from((inner.height + 1) / (item_height + 1)).max(1);
    highlight.scroll(shown);
    let PanelHighlight {
        item: highlighted,
        first,
        ..
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

    let not_playing = Style::default().fg(theme.secondary);
    let (icon, status, status_style) = match group.playback {
        Some(p) if p.is_playing => (
            symbols.group_playing,
            "Playing",
            Style::default().fg(theme.on),
        ),
        Some(p) if p.state == PlaybackState::Idle => {
            (symbols.group_stopped, "Stopped", not_playing)
        }
        Some(p) if p.state == PlaybackState::Paused => {
            (symbols.group_paused, "Paused", not_playing)
        }
        _ => (symbols.group_paused, "Not playing", not_playing),
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
        Some(track) if track.is_station => {
            track_line.push(Span::raw(track.title.as_deref().unwrap_or("Unknown title")));
        }
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
        Line::from(" Group speakers "),
        GROUP_EDITOR_HELP,
        None,
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

/// A panel in `area` listing what the browser's tab shows, with tabs as its title.
pub(super) fn render_browser(
    state: &HouseholdState,
    group: &GroupState,
    symbols: &Symbols,
    theme: &Theme,
    browser: &mut Browser,
    frame: &mut Frame,
    area: Rect,
) {
    let muted = Style::default().fg(theme.muted);
    let mut tabs = vec![Span::raw(" ")];
    for (i, tab) in Tab::ALL.into_iter().enumerate() {
        if i > 0 {
            tabs.push(Span::styled(" · ", muted));
        }
        let style = if tab == browser.tab {
            Style::default()
                .fg(theme.title)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.secondary)
        };
        tabs.push(Span::styled(tab.title(), style));
    }
    tabs.push(Span::raw(" "));
    let title = Line::from(tabs);

    let (help, note, items) = match browser.tab {
        Tab::Queue => {
            let queue = GroupQueue::of(state.queue.as_ref(), &group.id);
            let (note, items) = match queue {
                QueueContents::Loading => (Some("Reading the queue…"), Vec::new()),
                QueueContents::Failed => (Some("Failed to read the queue"), Vec::new()),
                QueueContents::Loaded(queue) if queue.items.is_empty() => {
                    (Some("The queue is empty"), Vec::new())
                }
                QueueContents::Loaded(queue) => {
                    let current = group.queue_position.filter(|_| group.plays_from(queue));
                    let items = queue
                        .items
                        .iter()
                        .map(|entry| queue_entry_lines(entry, current, symbols, theme))
                        .collect();
                    // The queue keeps what was last played from it.
                    let note = current.is_none().then_some("Not playing from the queue");
                    (note, items)
                }
            };
            // Its tracks can only be skipped to while the group plays from it.
            let help = match queue {
                QueueContents::Loaded(queue) if group.plays_from(queue) => BROWSER_HELP,
                QueueContents::Loaded(_) | QueueContents::Loading | QueueContents::Failed => {
                    BROWSER_VIEW_HELP
                }
            };
            (help, note, items)
        }
        // Its items are a line each, unlike the other tabs'.
        Tab::History => {
            let items: Vec<_> = state
                .history
                .iter()
                .map(|item| [history_line(item, symbols, theme)])
                .collect();
            let note = items.is_empty().then_some("Nothing played recently");
            return render_panel(
                frame,
                theme,
                area,
                title,
                BROWSER_HELP,
                note.map(|note| Line::styled(note, muted)),
                items,
                &mut browser.highlight,
            );
        }
        Tab::Favorites => {
            let items: Vec<_> = state
                .favorites
                .iter()
                .map(|favorite| favorite_lines(favorite, symbols, theme))
                .collect();
            let note = items.is_empty().then_some("No favorites");
            (BROWSER_HELP, note, items)
        }
        Tab::Playlists => {
            let items: Vec<_> = state
                .playlists
                .iter()
                .map(|playlist| playlist_lines(playlist, theme))
                .collect();
            let note = items.is_empty().then_some("No playlists");
            (BROWSER_HELP, note, items)
        }
    };
    render_panel(
        frame,
        theme,
        area,
        title,
        help,
        note.map(|note| Line::styled(note, muted)),
        items,
        &mut browser.highlight,
    );
}

/// The lines showing `entry` of a queue: its title, then its artist, album and duration. The
/// track at position `current`, if any, is marked as playing.
fn queue_entry_lines<'a>(
    entry: &'a QueueEntry,
    current: Option<u32>,
    symbols: &Symbols,
    theme: &Theme,
) -> [Line<'a>; 2] {
    let muted = Style::default().fg(theme.muted);
    let mut title_style = Style::default().add_modifier(Modifier::BOLD);
    let mut title_line = Vec::new();
    if current == Some(entry.position) {
        title_style = title_style.fg(theme.name);
        title_line.push(Span::styled(
            format!("{} ", symbols.playing),
            Style::default().fg(theme.on),
        ));
    }
    title_line.push(Span::styled(
        entry.name.as_deref().unwrap_or("Unknown title"),
        title_style,
    ));

    let mut details = vec![Span::styled(
        entry.artist.as_deref().unwrap_or("Unknown artist"),
        Style::default().fg(theme.artist),
    )];
    if let Some(album) = &entry.album {
        details.push(Span::styled(" · ", muted));
        details.push(Span::styled(
            album.as_str(),
            Style::default().fg(theme.secondary),
        ));
    }
    if let Some(millis) = entry.duration_millis {
        let secs = u32::try_from(millis / 1000).unwrap_or(u32::MAX);
        details.push(Span::styled(" · ", muted));
        details.push(Span::styled(format_duration(secs), muted));
    }

    [Line::from(title_line), Line::from(details)]
}

/// The line showing `item` of the history: what it is, then its name, muted if it can't be
/// played again.
fn history_line<'a>(item: &'a HistoryItem, symbols: &Symbols, theme: &Theme) -> Line<'a> {
    let mut name_style = Style::default().add_modifier(Modifier::BOLD);
    if !can_play_again(item) {
        name_style = name_style.fg(theme.muted);
    }
    let kind = match item.r#type {
        ContentType::Album => Some(symbols.album),
        ContentType::Playlist => Some(symbols.playlist),
        ContentType::Program => Some(symbols.radio),
        ContentType::Other | ContentType::Unknown => None,
    };
    let mut line = kind_spans(kind, symbols, theme);
    line.push(Span::styled(item.name.as_str(), name_style));
    if item.explicit {
        line.push(Span::raw(" "));
        line.push(Span::styled(" E ", theme.badge));
    }
    Line::from(line)
}

/// The spans showing `kind` before the name of something, if it is known.
fn kind_spans(kind: Option<&'static str>, symbols: &Symbols, theme: &Theme) -> Vec<Span<'static>> {
    kind.map(|kind| {
        vec![
            Span::styled(kind, Style::default().fg(theme.secondary)),
            Span::styled(
                symbols.source_kind_separator,
                Style::default().fg(theme.muted),
            ),
        ]
    })
    .unwrap_or_default()
}

/// The lines showing `playlist`: its name, then how many tracks it has.
fn playlist_lines<'a>(playlist: &'a PlaylistSummary, theme: &Theme) -> [Line<'a>; 2] {
    let tracks = playlist.track_count.map(|count| {
        let tracks = if count == 1 { "track" } else { "tracks" };
        Line::styled(
            format!("{count} {tracks}"),
            Style::default().fg(theme.muted),
        )
    });
    [
        Line::styled(
            playlist.name.as_str(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        tracks.unwrap_or_default(),
    ]
}

/// The lines showing `favorite`: what it is and its name, then its music service and its
/// description.
fn favorite_lines<'a>(favorite: &'a Favorite, symbols: &Symbols, theme: &Theme) -> [Line<'a>; 2] {
    let muted = Style::default().fg(theme.muted);
    let service = favorite
        .service
        .as_ref()
        .map(|service| service.name.as_str());
    let kind = favorite
        .resource
        .as_ref()
        .and_then(|resource| resource.r#type.as_deref())
        .and_then(|kind| match kind.to_lowercase().as_str() {
            "album" => Some(symbols.album),
            "artist" => Some(symbols.artist),
            "playlist" => Some(symbols.playlist),
            "station" | "program" => Some(symbols.radio),
            "show" | "podcast" => Some(symbols.podcast),
            _ => None,
        });
    // Descriptions are often the name of the service.
    let description = favorite
        .description
        .as_deref()
        .filter(|description| Some(*description) != service);

    let mut details = Vec::new();
    if let Some(service) = service {
        details.push(Span::styled(
            symbols.service(service),
            Style::default().fg(theme.secondary),
        ));
    }
    if let Some(description) = description {
        if !details.is_empty() {
            details.push(Span::styled(" · ", muted));
        }
        details.push(Span::styled(description, muted));
    }
    let mut name_line = kind_spans(kind, symbols, theme);
    name_line.push(Span::styled(
        favorite.name.as_str(),
        Style::default().add_modifier(Modifier::BOLD),
    ));
    [Line::from(name_line), Line::from(details)]
}
