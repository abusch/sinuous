use std::time::{Duration, Instant};

use clap::crate_version;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    Frame,
    layout::{
        Constraint,
        HorizontalAlignment::{Center, Right},
        Layout, Rect,
    },
    style::{Color, Modifier, Style},
    symbols::line::VERTICAL,
    text::{Line, Span},
    widgets::{
        Block, BorderType::Rounded, Clear, Gauge, List, ListItem, ListState, Padding, Paragraph,
        Tabs, Wrap,
    },
};
use sinuous_client::GroupId;

use crate::sonos::{
    Command, ConnectionStatus, ErrorMessage, GroupAction, GroupState, HouseholdState, TrackInfo,
};

/// How long errors are displayed for.
const ERROR_DISPLAY_TIME: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ViewMode {
    #[default]
    Queue,
    Favorites,
}

/// What the user is looking at, independently of the state of the speakers.
#[derive(Debug, Default)]
pub struct UiState {
    view: ViewMode,
    selected_group: Option<GroupId>,
    selected_favorite: usize,
    /// The group highlighted in the group picker, if it is open.
    group_picker: Option<usize>,
}

impl UiState {
    /// Keep the selection valid for the latest `state`, e.g. when the selected group is gone.
    pub fn sync(&mut self, state: &HouseholdState) {
        if !self
            .selected_group
            .as_ref()
            .is_some_and(|id| state.groups.iter().any(|g| &g.id == id))
        {
            self.selected_group = state.groups.first().map(|g| g.id.clone());
        }
        self.selected_favorite = self
            .selected_favorite
            .min(state.favorites.len().saturating_sub(1));
        if state.groups.is_empty() {
            self.group_picker = None;
        } else if let Some(highlighted) = &mut self.group_picker {
            *highlighted = (*highlighted).min(state.groups.len() - 1);
        }
    }

    /// The selected group and its index.
    fn current_group<'a>(&self, state: &'a HouseholdState) -> Option<(usize, &'a GroupState)> {
        let id = self.selected_group.as_ref()?;
        state.groups.iter().enumerate().find(|(_, g)| &g.id == id)
    }

    /// Open the group picker, with the selected group highlighted.
    fn open_group_picker(&mut self, state: &HouseholdState) {
        if let Some((index, _)) = self.current_group(state) {
            self.group_picker = Some(index);
        }
    }
}

/// Whether what is displayed changes over time, and so needs redrawing even when the state doesn't
/// change.
pub fn is_animated(state: &HouseholdState, ui: &UiState) -> bool {
    let showing_error = state.last_error.as_ref().is_some_and(is_shown);
    let advancing = ui
        .current_group(state)
        .and_then(|(_, group)| group.playback)
        .is_some_and(|playback| playback.is_advancing());
    showing_error || advancing
}

pub fn render_ui(frame: &mut Frame, state: &HouseholdState, ui: &UiState) {
    match &state.status {
        ConnectionStatus::Connecting => {
            return render_message(frame, "Connecting to your Sonos speakers...");
        }
        ConnectionStatus::Failed(err) => {
            return render_message(
                frame,
                &format!("Failed to connect to your Sonos speakers: {err}\n\nPress q to quit."),
            );
        }
        ConnectionStatus::Connected => {}
    }
    let Some((group_index, group)) = ui.current_group(state) else {
        return render_message(frame, "No speakers found.\n\nPress q to quit.");
    };
    let [title, playbar, view_tabs, content] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(3),
        Constraint::Length(1),
        Constraint::Min(1),
    ])
    .areas(frame.area());

    // Title line
    render_title_bar(state, group, frame, title);

    // playbar
    render_playbar(group, frame, playbar);

    // View tabs
    render_view_tabs(ui, frame, view_tabs);

    // Main content area (switches based on current view)
    match ui.view {
        ViewMode::Queue => render_queue(group, frame, content),
        ViewMode::Favorites => render_favorites(state, ui, frame, content),
    }

    // Group picker, on top of everything else
    if let Some(highlighted) = ui.group_picker {
        render_group_picker(state, group_index, highlighted, frame);
    }
}

/// Update `ui` according to the key pressed, and return the command to send to the speakers, if
/// any.
pub fn handle_input(input: &KeyEvent, state: &HouseholdState, ui: &mut UiState) -> Option<Command> {
    if let Some(highlighted) = ui.group_picker {
        handle_group_picker_input(input, state, ui, highlighted);
        return None;
    }
    match input.code {
        // View switching
        KeyCode::Char('1') => ui.view = ViewMode::Queue,
        KeyCode::Char('2') => ui.view = ViewMode::Favorites,

        // Favorites navigation (only when in Favorites view)
        KeyCode::Up | KeyCode::Char('k') if ui.view == ViewMode::Favorites => {
            ui.selected_favorite = ui.selected_favorite.saturating_sub(1);
        }
        KeyCode::Down | KeyCode::Char('j') if ui.view == ViewMode::Favorites => {
            if ui.selected_favorite + 1 < state.favorites.len() {
                ui.selected_favorite += 1;
            }
        }

        // Group switching
        KeyCode::Char('g') => ui.open_group_picker(state),

        _ => return group_command(input, state, ui),
    }
    None
}

/// Update `ui` according to the key pressed while the group picker is open, with the group at
/// index `highlighted` highlighted.
fn handle_group_picker_input(
    input: &KeyEvent,
    state: &HouseholdState,
    ui: &mut UiState,
    highlighted: usize,
) {
    match input.code {
        KeyCode::Up | KeyCode::Char('k') => ui.group_picker = Some(highlighted.saturating_sub(1)),
        KeyCode::Down | KeyCode::Char('j') => {
            if highlighted + 1 < state.groups.len() {
                ui.group_picker = Some(highlighted + 1);
            }
        }
        KeyCode::Enter => {
            if let Some(group) = state.groups.get(highlighted) {
                ui.selected_group = Some(group.id.clone());
            }
            ui.group_picker = None;
        }
        KeyCode::Esc | KeyCode::Char('g') => ui.group_picker = None,
        _ => {}
    }
}

/// The command for the selected group corresponding to the key pressed, if any.
fn group_command(input: &KeyEvent, state: &HouseholdState, ui: &UiState) -> Option<Command> {
    let (_, group) = ui.current_group(state)?;
    let action = match input.code {
        // Play favorite
        KeyCode::Enter if ui.view == ViewMode::Favorites => {
            let favorite = state.favorites.get(ui.selected_favorite)?;
            GroupAction::LoadFavorite(favorite.id.clone())
        }

        // Playback controls (work in any view)
        KeyCode::Char(' ') => {
            if group.playback.is_some_and(|p| p.is_playing) {
                GroupAction::Pause
            } else {
                GroupAction::Play
            }
        }
        KeyCode::Char('n') => GroupAction::Next,
        KeyCode::Char('p') => GroupAction::Previous,
        KeyCode::Char('[') => GroupAction::AdjustVolume(-2),
        KeyCode::Char(']') => GroupAction::AdjustVolume(2),

        _ => return None,
    };
    Some(Command {
        group: group.id.clone(),
        action,
    })
}

/// Display a message in place of the main UI.
fn render_message(frame: &mut Frame, message: &str) {
    let [_, area, _] = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(6),
        Constraint::Fill(1),
    ])
    .areas(frame.area());
    let paragraph = Paragraph::new(message)
        .centered()
        .wrap(Wrap { trim: true })
        .block(
            Block::bordered()
                .border_type(Rounded)
                .title(format!(" Sinuous {} ", crate_version!())),
        );
    frame.render_widget(paragraph, area);
}

fn render_title_bar(state: &HouseholdState, group: &GroupState, frame: &mut Frame, area: Rect) {
    let [title_area, volume_area] =
        Layout::horizontal([Constraint::Min(1), Constraint::Length(8)]).areas(area);

    let mut header = vec![Span::styled(
        format!("Sinuous {}", crate_version!()),
        Style::default()
            .fg(Color::Yellow)
            .bg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    )];
    match state.last_error.as_ref().filter(|err| is_shown(err)) {
        Some(err) => header.push(Span::styled(
            format!(" -- {}", err.message),
            Style::default().fg(Color::Red),
        )),
        None => {
            header.push(Span::styled(" -- Playing on ", Style::default()));
            header.push(Span::styled(&group.name, Style::default().fg(Color::Green)));
        }
    }
    let title = Paragraph::new(Line::from(header));
    frame.render_widget(title, title_area);

    let vol_text = match group.volume {
        Some(volume) => format!("🔊: {volume:2} "),
        None => "🔊: -- ".to_owned(),
    };
    let vol = Paragraph::new(vol_text).alignment(Right);
    frame.render_widget(vol, volume_area);
}

fn render_view_tabs(ui: &UiState, frame: &mut Frame, area: Rect) {
    let view_names = vec!["1 Queue", "2 Favorites"];
    let selected = match ui.view {
        ViewMode::Queue => 0,
        ViewMode::Favorites => 1,
    };

    let tabs = Tabs::new(view_names)
        .highlight_style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .select(selected)
        .divider(VERTICAL);

    frame.render_widget(tabs, area);
}

fn render_queue(group: &GroupState, frame: &mut Frame, area: Rect) {
    let now_playing = group.now_playing.as_ref();
    let next_track = group.next_track.as_ref();

    // Only the current and next tracks are known: select the current one (if any)
    let mut list_state = ListState::default();
    list_state.select(now_playing.map(|_| 0));

    let items = now_playing.into_iter().chain(next_track).map(|t| {
        let s = format!(
            "{} ({})",
            format_track(t),
            format_duration(t.duration_secs.unwrap_or(0))
        );
        ListItem::new(s)
    });
    let list = List::new(items)
        .highlight_style(Style::default().fg(Color::LightMagenta))
        .highlight_symbol("⏵")
        .block(
            Block::bordered()
                .title_top(" Queue ")
                .title_bottom(
                    Line::from(" SPACE play/pause • n next • p prev • [ ] volume • g groups ")
                        .centered()
                        .style(Style::default().fg(Color::DarkGray)),
                )
                .border_type(Rounded),
        );

    frame.render_stateful_widget(list, area, &mut list_state);
}

fn render_playbar(group: &GroupState, frame: &mut Frame, area: Rect) {
    let playback = group.playback;
    let (np, label, ratio) = if let Some(track) = &group.now_playing {
        let duration = track.duration_secs.unwrap_or(0);
        let elapsed = playback.map_or(0, |p| {
            let elapsed = u32::try_from(p.position(Instant::now()).as_secs()).unwrap_or(u32::MAX);
            // The position is worked out locally, and can run past the end of the track until the
            // speakers report the next one.
            if duration != 0 {
                elapsed.min(duration)
            } else {
                elapsed
            }
        });
        let percent = if duration != 0 {
            f64::clamp(f64::from(elapsed) / f64::from(duration), 0.0, 1.0)
        } else {
            0.0
        };
        let label = format!(
            "{} / {}",
            format_duration(elapsed),
            format_duration(duration)
        );
        let title = format!(" {} ", format_track(track));
        (title, label, percent)
    } else {
        (
            " Nothing currently playing ".to_owned(),
            "0:00 / 0:00".to_owned(),
            0.0,
        )
    };

    // Border around the whole playbar section
    let block = Block::bordered().border_type(Rounded).title(np);
    // The inner area is where the gauge and control buttons will be rendered
    let playbar_area = block.inner(area);

    // split the inner area into 2 columns for the buttons and the gauge
    let [symbol_area, bar_area] =
        Layout::horizontal([Constraint::Length(3), Constraint::Min(1)]).areas(playbar_area);

    let is_playing = playback.is_some_and(|p| p.is_playing);
    let media_symbol = if is_playing { "⏵" } else { "⏸" };
    let symbol = Paragraph::new(media_symbol).alignment(Center);

    let playbar = Gauge::default()
        .use_unicode(true)
        .gauge_style(
            Style::default()
                .fg(Color::LightGreen)
                .bg(Color::Black)
                .add_modifier(Modifier::ITALIC),
        )
        .label(label)
        .ratio(ratio);

    // render all the widgets
    frame.render_widget(block, area);
    frame.render_widget(symbol, symbol_area);
    frame.render_widget(playbar, bar_area);
}

fn render_favorites(state: &HouseholdState, ui: &UiState, frame: &mut Frame, area: Rect) {
    let mut list_state = ListState::default();
    list_state.select(Some(ui.selected_favorite));

    let items = state.favorites.iter().map(|fav| {
        let s = match &fav.description {
            Some(description) => format!("{} - {}", fav.name, description),
            None => fav.name.clone(),
        };
        ListItem::new(s)
    });

    let list = List::new(items)
        .highlight_style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("⏵ ")
        .block(
            Block::bordered()
                .title_top(" Favorites ")
                .title_bottom(
                    Line::from(" ↑↓ Navigate • ENTER to play • g groups ")
                        .centered()
                        .style(Style::default().fg(Color::DarkGray)),
                )
                .border_type(Rounded),
        );

    frame.render_stateful_widget(list, area, &mut list_state);
}

/// A popup listing the groups, with the selected group (at `selected`) in green and the one at
/// `highlighted` highlighted.
fn render_group_picker(
    state: &HouseholdState,
    selected: usize,
    highlighted: usize,
    frame: &mut Frame,
) {
    const HIGHLIGHT_SYMBOL: &str = "⏵ ";
    const PADDING: Padding = Padding::symmetric(2, 1);
    let help = Line::from(" ↑↓ Navigate • ENTER select • ESC close ")
        .centered()
        .style(Style::default().fg(Color::DarkGray));

    let lines: Vec<_> = state
        .groups
        .iter()
        .enumerate()
        .map(|(i, group)| {
            let name_style = if i == selected {
                Style::default().fg(Color::Green)
            } else {
                Style::default()
            };
            let mut spans = vec![Span::styled(group.name.as_str(), name_style)];
            if group.playback.is_some_and(|p| p.is_playing)
                && let Some(track) = &group.now_playing
            {
                spans.push(Span::styled(
                    format!("  ♪ {}", track.title.as_deref().unwrap_or("Unknown")),
                    Style::default().fg(Color::DarkGray),
                ));
            }
            Line::from(spans)
        })
        .collect();

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
    list_state.select(Some(highlighted));
    let list = List::new(lines)
        .highlight_style(Style::default().add_modifier(Modifier::BOLD))
        .highlight_symbol(HIGHLIGHT_SYMBOL)
        .block(
            Block::bordered()
                .border_type(Rounded)
                .padding(PADDING)
                .title_top(" Groups ")
                .title_bottom(help),
        );

    frame.render_widget(Clear, area);
    frame.render_stateful_widget(list, area, &mut list_state);
}

fn is_shown(error: &ErrorMessage) -> bool {
    error.at.elapsed() < ERROR_DISPLAY_TIME
}

fn format_track(track: &TrackInfo) -> String {
    format!(
        "{} - {} - {}",
        track.artist.as_deref().unwrap_or("Unknown"),
        track.album.as_deref().unwrap_or("Unknown"),
        track.title.as_deref().unwrap_or("Unknown"),
    )
}

fn format_duration(secs: u32) -> String {
    let minutes = secs / 60;
    let seconds = secs % 60;

    format!("{minutes}:{seconds:02}")
}
