use clap::crate_version;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    layout::{
        Alignment::{Center, Right},
        Constraint, Layout, Rect,
    },
    style::{Color, Modifier, Style},
    symbols::line::VERTICAL,
    text::{Line, Span},
    widgets::{Block, BorderType::Rounded, Gauge, List, ListItem, ListState, Paragraph, Tabs},
    Frame,
};

use crate::{sonos::SpeakerState, Action, Direction, ViewMode};

pub fn render_ui(frame: &mut Frame, state: &SpeakerState) {
    let [title, tabs, playbar, view_tabs, content] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(3),
        Constraint::Length(3),
        Constraint::Length(1),
        Constraint::Min(1),
    ])
    .areas(frame.area());

    // Title line
    render_title_bar(state, frame, title);

    // Group tabs
    render_tabs(state, frame, tabs);

    // playbar
    render_playbar(state, frame, playbar);

    // View tabs
    render_view_tabs(state, frame, view_tabs);

    // Main content area (switches based on current view)
    match state.current_view {
        ViewMode::Queue => render_queue(state, frame, content),
        ViewMode::Favorites => render_favorites(state, frame, content),
    }
}

pub fn handle_input(input: &KeyEvent, state: &SpeakerState) -> Action {
    match input.code {
        // View switching
        KeyCode::Char('1') => Action::SwitchView(ViewMode::Queue),
        KeyCode::Char('2') => Action::SwitchView(ViewMode::Favorites),

        // Favorites navigation (only when in Favorites view)
        KeyCode::Up | KeyCode::Char('k') if matches!(state.current_view, ViewMode::Favorites) => {
            Action::NavigateFavorites(Direction::Up)
        }
        KeyCode::Down | KeyCode::Char('j') if matches!(state.current_view, ViewMode::Favorites) => {
            Action::NavigateFavorites(Direction::Down)
        }
        KeyCode::Enter if matches!(state.current_view, ViewMode::Favorites) => {
            Action::PlayFavorite(state.selected_favorite)
        }

        // Playback controls (work in any view)
        KeyCode::Char(' ') => {
            if state.is_playing {
                Action::Pause
            } else {
                Action::Play
            }
        }
        KeyCode::Char('n') => Action::Next,
        KeyCode::Char('p') => Action::Prev,
        KeyCode::Char('[') => Action::VolAdjust(-2),
        KeyCode::Char(']') => Action::VolAdjust(2),

        // Group switching
        KeyCode::Tab => {
            if input.modifiers.contains(KeyModifiers::SHIFT) {
                Action::PrevSpeaker
            } else {
                Action::NextSpeaker
            }
        }

        _ => Action::Nop,
    }
}

fn render_title_bar(state: &SpeakerState, frame: &mut Frame, area: Rect) {
    let [title_area, volume_area] =
        Layout::horizontal([Constraint::Min(1), Constraint::Length(8)]).areas(area);

    let header = vec![Line::from(vec![
        Span::styled(
            format!("Sinuous {}", crate_version!()),
            Style::default()
                .fg(Color::Yellow)
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" -- Playing on ", Style::default()),
        Span::styled(state.group_name(), Style::default().fg(Color::Green)),
    ])];
    let title = Paragraph::new(header);
    frame.render_widget(title, title_area);

    let vol_text = format!("🔊: {:2} ", state.current_volume);
    let vol = Paragraph::new(vol_text).alignment(Right);
    frame.render_widget(vol, volume_area);
}

fn render_tabs(state: &SpeakerState, frame: &mut Frame, area: Rect) {
    let tabs = Tabs::new(state.group_names.iter().cloned())
        .block(Block::bordered().border_type(Rounded).title(" Groups "))
        .highlight_style(Style::default().fg(Color::Green))
        .select(state.selected_group)
        .divider(VERTICAL);

    frame.render_widget(tabs, area);
}

fn render_view_tabs(state: &SpeakerState, frame: &mut Frame, area: Rect) {
    let view_names = vec!["1 Queue", "2 Favorites"];
    let selected = match state.current_view {
        ViewMode::Queue => 0,
        ViewMode::Favorites => 1,
    };

    let tabs = Tabs::new(view_names)
        .highlight_style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
        .select(selected)
        .divider(VERTICAL);

    frame.render_widget(tabs, area);
}

fn render_queue(state: &SpeakerState, frame: &mut Frame, area: Rect) {
    // Select the currently playing track in the queue (if any)
    let mut list_state = ListState::default();
    let selection = state.now_playing.as_ref().and_then(|track| {
        state
            .queue
            .iter()
            .position(|t| t.uri() == track.track().uri())
    });
    list_state.select(selection);

    let items = state.queue.iter().map(|t| {
        let s = format!(
            "{} - {} - {} ({})",
            t.creator().unwrap_or("Unknown"),
            t.album().unwrap_or("Unknown"),
            t.title(),
            format_duration(t.duration().unwrap_or(0))
        );
        ListItem::new(s)
    });
    let list = List::new(items)
        .highlight_style(Style::default().fg(Color::LightMagenta))
        .highlight_symbol("⏵")
        .block(Block::bordered().title(" Queue ").border_type(Rounded));

    frame.render_stateful_widget(list, area, &mut list_state);

    // Show help text at the bottom if there are tracks in the queue
    if !state.queue.is_empty() {
        render_help_in_border(frame, area, " SPACE play/pause • n next • p prev • [ ] volume ");
    }
}

fn render_playbar(state: &SpeakerState, frame: &mut Frame, area: Rect) {
    let (np, label, ratio) = if let Some(track) = &state.now_playing {
        let percent = if track.duration() != 0 {
            f64::clamp(
                f64::from(track.elapsed()) / f64::from(track.duration()),
                0.0,
                1.0,
            )
        } else {
            0.0
        };
        let label = format!(
            "{} / {}",
            format_duration(track.elapsed()),
            format_duration(track.duration())
        );
        let title = format!(
            " {} - {} - {} ",
            track.track().creator().unwrap_or("Unknown"),
            track.track().album().unwrap_or("Unknown"),
            track.track().title()
        );
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

    let media_symbol = if state.is_playing { "⏵" } else { "⏸" };
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

fn render_favorites(state: &SpeakerState, frame: &mut Frame, area: Rect) {
    let mut list_state = ListState::default();
    list_state.select(Some(state.selected_favorite));

    let items = state.favorites.iter().map(|fav| {
        let s = format!(
            "{} - {}",
            fav.title,
            fav.description
        );
        ListItem::new(s)
    });

    let list = List::new(items)
        .highlight_style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
        .highlight_symbol("⏵ ")
        .block(
            Block::bordered()
                .title(" Favorite Playlists ")
                .border_type(Rounded)
        );

    frame.render_stateful_widget(list, area, &mut list_state);

    // Show help text at the bottom if there are favorites
    if !state.favorites.is_empty() {
        render_help_in_border(frame, area, " ↑↓ Navigate • ENTER to play ");
    }
}

fn render_help_in_border(frame: &mut Frame, area: Rect, help_text: &str) {
    // Calculate how much space the help text takes
    let text_width = help_text.len();
    // Available width is total width minus the 2 border characters (left and right)
    let available_width = area.width.saturating_sub(2) as usize;

    // Calculate padding for centering
    let padding = (available_width.saturating_sub(text_width)) / 2;

    // Create the bottom border content (without corner characters - they're drawn by the Block)
    let left_border = "─".repeat(padding);
    let right_border = "─".repeat(available_width.saturating_sub(padding + text_width));

    let border_line = Line::from(vec![
        Span::raw(left_border),
        Span::styled(help_text, Style::default().fg(Color::DarkGray)),
        Span::raw(right_border),
    ]);

    let help = Paragraph::new(border_line);
    // Position the help text just after the left border corner character
    let help_area = Rect {
        x: area.x + 1,  // Skip the corner character
        y: area.y + area.height.saturating_sub(1),
        width: area.width.saturating_sub(2),  // Exclude both corner characters
        height: 1,
    };
    frame.render_widget(help, help_area);
}

fn format_duration(secs: u32) -> String {
    let minutes = secs / 60;
    let seconds = secs % 60;

    format!("{minutes}:{seconds:02}")
}
