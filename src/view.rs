use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tui::{
    backend::Backend,
    layout::{Constraint, Direction::Vertical, Layout},
    style::{Color, Modifier, Style},
    symbols::line::VERTICAL,
    text::{Span, Spans},
    widgets::{
        Block, BorderType::Rounded, Borders, Gauge, List, ListItem, Paragraph, Tabs, Widget,
    },
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
    let title = render_title_bar(state);
    frame.render_widget(title, chunks[0]);

    // Speaker tabs
    let tabs = render_tabs(state);
    frame.render_widget(tabs, chunks[1]);

    // playbar
    let playbar = render_playbar(state);
    frame.render_widget(playbar, chunks[2]);

    // queue
    let list = render_queue(state);
    frame.render_widget(list, chunks[3]);
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

fn render_title_bar(state: &SpeakerState) -> impl Widget + '_ {
    let header = vec![Spans::from(vec![
        Span::styled(
            " Sinuous v0.1 ",
            Style::default()
                .fg(Color::Yellow)
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" -- Playing on ", Style::default()),
        Span::styled(state.speaker_name(), Style::default().fg(Color::Green)),
    ])];
    Paragraph::new(header)
}

fn render_tabs(state: &SpeakerState) -> impl Widget {
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

    tabs
}

fn render_queue(state: &SpeakerState) -> impl Widget {
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

    list
}

fn render_playbar(state: &SpeakerState) -> impl Widget {
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
    let playbar = Gauge::default()
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(Rounded)
                .title(np),
        )
        .use_unicode(true)
        .gauge_style(
            Style::default()
                .fg(Color::LightGreen)
                .bg(Color::Black)
                .add_modifier(Modifier::ITALIC),
        )
        .label(label)
        .ratio(ratio);

    playbar
}

fn format_duration(secs: u32) -> String {
    let minutes = secs / 60;
    let seconds = secs % 60;

    format!("{}:{:02}", minutes, seconds)
}
