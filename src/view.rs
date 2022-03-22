use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tui::{
    backend::Backend,
    layout::{Alignment::{Center, Right}, Constraint, Direction::{Vertical, Horizontal}, Layout, Rect},
    style::{Color, Modifier, Style},
    symbols::line::VERTICAL,
    text::{Span, Spans},
    widgets::{Block, BorderType::Rounded, Borders, Gauge, List, ListItem, Paragraph, Tabs},
    Frame,
};

use crate::{sonos::SpeakerState, Action};

pub fn render_ui<B: Backend>(frame: &mut Frame<B>, state: &SpeakerState) {
    let chunks = Layout::default()
        .direction(Vertical)
        .constraints(vec![
            Constraint::Length(1),
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Min(1),
        ])
        .split(frame.size());

    // Title line
    render_title_bar(state, frame, chunks[0]);

    // Speaker tabs
    render_tabs(state, frame, chunks[1]);

    // playbar
    render_playbar(state, frame, chunks[2]);

    // queue
    render_queue(state, frame, chunks[3]);
}

pub fn handle_input(input: &KeyEvent, state: &SpeakerState) -> Action {
    match input.code {
        KeyCode::Char(' ') => {
            if state.is_playing {
                Action::Pause
            } else {
                Action::Play
            }
        }
        KeyCode::Char('n') => Action::Next,
        KeyCode::Char('p') => Action::Prev,
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

fn render_title_bar<B: Backend>(state: &SpeakerState, frame: &mut Frame<B>, area: Rect) {
    let chunks = Layout::default()
        .direction(Horizontal)
        .constraints(vec![Constraint::Min(1), Constraint::Length(8)])
        .split(area);

    let header = vec![Spans::from(vec![
        Span::styled(
            " Sinuous v0.1 ", // TODO don't hardcode version number
            Style::default()
                .fg(Color::Yellow)
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" -- Playing on ", Style::default()),
        Span::styled(state.speaker_name(), Style::default().fg(Color::Green)),
    ])];
    let title = Paragraph::new(header);
    frame.render_widget(title, chunks[0]);

    let vol_text = format!("🔊: {:2} ", state.current_volume);
    let vol = Paragraph::new(vol_text)
        .alignment(Right);
    frame.render_widget(vol, chunks[1]);
}

fn render_tabs<B: Backend>(state: &SpeakerState, frame: &mut Frame<B>, area: Rect) {
    let names = state
        .speaker_names
        .iter()
        .cloned()
        .map(Spans::from)
        .collect();
    let tabs = Tabs::new(names)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(Rounded)
                .title(" Speakers "),
        )
        .highlight_style(Style::default().fg(Color::Green))
        .select(state.selected_speaker)
        .divider(VERTICAL);

    frame.render_widget(tabs, area);
}

fn render_queue<B: Backend>(state: &SpeakerState, frame: &mut Frame<B>, area: Rect) {
    let items = state
        .queue
        .iter()
        .map(|t| {
            let s = format!(
                "{} - {} - {} ({})",
                t.creator().unwrap_or("Unknown"),
                t.album().unwrap_or("Unknown"),
                t.title(),
                format_duration(t.duration().unwrap_or(0))
            );
            ListItem::new(s)
        })
        .collect::<Vec<_>>();
    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Queue ")
            .border_type(Rounded),
    );

    frame.render_widget(list, area);
}

fn render_playbar<B: Backend>(state: &SpeakerState, frame: &mut Frame<B>, area: Rect) {
    let (np, label, ratio) = if let Some(track) = &state.now_playing {
        let percent = (track.elapsed() as f64) / (track.duration() as f64);
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
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(Rounded)
        .title(np);
    // The inner area is where the gauge and control buttons will be rendered
    let playbar_area = block.inner(area);

    // split the inner area into 2 columns for the buttons and the gauge
    let playbar_chunks = Layout::default()
        .direction(Horizontal)
        .constraints(vec![Constraint::Length(3), Constraint::Min(1)])
        .split(playbar_area);

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
    frame.render_widget(symbol, playbar_chunks[0]);
    frame.render_widget(playbar, playbar_chunks[1]);
}

fn format_duration(secs: u32) -> String {
    let minutes = secs / 60;
    let seconds = secs % 60;

    format!("{}:{:02}", minutes, seconds)
}
