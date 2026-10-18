//! What the selected group is playing, with its controls, progress and volume.

use std::time::Instant;

use ratatui::{
    Frame,
    buffer::Buffer,
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Gauge, Paragraph, Widget},
};
use ratatui_image::{FilterType, FontSize, Resize, StatefulImage};

use crate::artwork::Artwork;
use crate::sonos::{GroupState, Source, TrackInfo};
use crate::symbols::Symbols;
use crate::theme::Theme;

use super::input::{Action, supports};

/// How narrow the volume slider can be, with its icon and value.
const MIN_VOLUME_WIDTH: u16 = 44;

/// How wide the icon and the value on each side of the volume slider are: as wide as each other,
/// so that the slider is centred.
const VOLUME_SIDE_WIDTH: u16 = 3;

/// How wide the times on each side of the progress bar can be, e.g. `-59:59`.
const TIME_WIDTH: u16 = 6;

/// What to show where the album art goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AlbumArt {
    /// Nothing, and leave no room for it.
    Off,
    /// A placeholder, even if the art is loaded.
    Placeholder,
    /// The art, or a placeholder until it is loaded.
    Shown,
}

/// The track playing on `group`, how far into it playback is, its volume, and the next track.
pub(super) fn render_now_playing(
    group: &GroupState,
    symbols: &Symbols,
    theme: &Theme,
    frame: &mut Frame,
    area: Rect,
    artwork: &mut Artwork,
    album_art: AlbumArt,
) {
    // Everything but the album art: source, track information, progress, controls, volume and
    // next track, with the gaps between them.
    const DETAILS_HEIGHT: u16 = 12;
    const MAX_ART_HEIGHT: u16 = 16;
    const MIN_ART_HEIGHT: u16 = 4;

    // Make room for the art even when there is none, so that the rest doesn't move when there is.
    // Keep a gap below it, and at least one empty line above and below everything.
    let art_height = if album_art != AlbumArt::Off {
        let height = area
            .height
            .saturating_sub(DETAILS_HEIGHT + 3)
            .min(MAX_ART_HEIGHT);
        if height >= MIN_ART_HEIGHT { height } else { 0 }
    } else {
        0
    };
    let [
        _,
        art_area,
        _,
        source_area,
        _,
        info_area,
        _,
        progress_area,
        _,
        controls_area,
        _,
        volume_area,
        _,
        next_area,
        _,
    ] = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(art_height),
        Constraint::Length(if art_height > 0 { 1 } else { 0 }),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(2),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Fill(1),
    ])
    .areas(area);

    let font_size = artwork.font_size();
    if art_height == 0 {
        // No room for it
    } else if let Some(image) = artwork.image().filter(|_| album_art == AlbumArt::Shown) {
        // Images are drawn from the top left corner of their area: centre it on the image's size.
        let resize = Resize::Scale(Some(FilterType::Triangle));
        let size = image.size_for(resize.clone(), art_area.as_size());
        frame.render_stateful_widget(
            StatefulImage::default().resize(resize),
            art_area.centered(
                Constraint::Length(size.width),
                Constraint::Length(size.height),
            ),
            image,
        );
    } else {
        render_art_placeholder(font_size, symbols, theme, frame, art_area);
    }

    if let Some(source) = &group.source {
        frame.render_widget(
            Paragraph::new(source_line(source, symbols, theme)).centered(),
            source_area,
        );
    }

    // Track information
    let info = match &group.now_playing {
        Some(track) => vec![
            title_line(track, theme),
            Line::styled(
                track.artist.as_deref().unwrap_or("Unknown artist"),
                Style::default().fg(theme.artist),
            ),
        ],
        None => vec![Line::styled(
            "Nothing playing",
            Style::default().fg(theme.muted),
        )],
    };
    frame.render_widget(Paragraph::new(info).centered(), info_area);

    // The volume slider lines up with the controls above it, unless they are too narrow (with
    // icons), and the progress bar with the slider.
    let controls = controls_line(group, symbols, theme);
    let volume_width = volume_width(&controls);
    let controls_width = u16::try_from(controls.width()).unwrap_or(u16::MAX);
    frame.render_widget(Paragraph::new(controls).centered(), controls_area);
    if let Some(end) = group.sleep_timer {
        render_sleep_timer(end, symbols, theme, frame, controls_area, controls_width);
    }
    let slider_area = render_volume(
        group,
        symbols,
        theme,
        frame,
        volume_area.centered_horizontally(Constraint::Length(volume_width)),
    );
    render_progress(
        group,
        theme,
        frame,
        progress_area,
        Rect {
            y: progress_area.y,
            ..slider_area
        },
    );

    // Next track, unless it is the one playing: radio stations report the track playing as the
    // next one too.
    let next = group.next_track.as_ref().filter(|next| {
        group
            .now_playing
            .as_ref()
            .is_none_or(|now| (&now.title, &now.artist) != (&next.title, &next.artist))
    });
    if let Some(next) = next {
        let dim = Style::default().fg(theme.muted);
        let next = Line::from(vec![
            Span::styled("Next: ", dim),
            Span::raw(next.title.as_deref().unwrap_or("Unknown title")),
            Span::styled(" by ", dim),
            Span::styled(
                next.artist.as_deref().unwrap_or("Unknown artist"),
                Style::default().fg(theme.artist),
            ),
        ]);
        frame.render_widget(Paragraph::new(next).centered(), next_area);
    }
}

/// How wide the volume is, with the slider between its icon and value: as wide as the
/// `controls` above it, unless they are too narrow.
fn volume_width(controls: &Line) -> u16 {
    u16::try_from(controls.width())
        .unwrap_or(u16::MAX)
        .max(MIN_VOLUME_WIDTH)
}

/// A square in the middle of `area`, with an icon in it, where the album art goes when there is
/// none to show: it is loading, or failed to, there is none, or it is hidden behind a popup.
fn render_art_placeholder(
    font_size: FontSize,
    symbols: &Symbols,
    theme: &Theme,
    frame: &mut Frame,
    area: Rect,
) {
    // Cells are taller than they are wide.
    let width =
        u32::from(area.height) * u32::from(font_size.height) / u32::from(font_size.width.max(1));
    let width = u16::try_from(width).unwrap_or(u16::MAX).min(area.width);
    let square = area.centered_horizontally(Constraint::Length(width));
    frame.render_widget(Block::new().style(theme.art_placeholder), square);
    frame.render_widget(
        Paragraph::new(symbols.no_art).centered(),
        square.centered_vertically(Constraint::Length(1)),
    );
}

/// How wide what `group` is playing needs to be for the controls, the volume and the progress bar
/// to fit.
pub(super) fn now_playing_width(group: &GroupState, symbols: &Symbols, theme: &Theme) -> u16 {
    let volume = volume_width(&controls_line(group, symbols, theme));
    // The progress bar is as wide as the volume slider, with the times on each side of it.
    let bar = volume.saturating_sub(2 * (VOLUME_SIDE_WIDTH + 1));
    volume.max(bar + 2 * (TIME_WIDTH + 1))
}

/// Where the music comes from, e.g. `Spotify · Album · Altered State`, or `Spotify · 󰀥 Altered
/// State` with icons.
fn source_line<'a>(source: &'a Source, symbols: &Symbols, theme: &Theme) -> Line<'a> {
    let kind = source.kind.as_deref().and_then(|kind| match kind {
        "album" => Some(symbols.album),
        "artist" => Some(symbols.artist),
        "playlist" => Some(symbols.playlist),
        "station" | "program" => Some(symbols.radio),
        "show" | "podcast" => Some(symbols.podcast),
        kind if kind.starts_with("linein") => Some(symbols.line_in),
        _ => None,
    });
    let separator = Span::styled(" · ", Style::default().fg(theme.muted));
    let mut spans = Vec::new();
    if let Some(service) = &source.service {
        spans.push(Span::styled(
            symbols.service(service),
            Style::default().fg(theme.name),
        ));
    }
    if let Some(kind) = kind {
        if !spans.is_empty() {
            spans.push(separator.clone());
        }
        spans.push(Span::raw(kind));
    }
    if let Some(name) = &source.name {
        match kind {
            Some(_) => spans.push(Span::styled(symbols.source_kind_separator, separator.style)),
            None if !spans.is_empty() => spans.push(separator),
            None => {}
        }
        spans.push(Span::raw(name.as_str()));
    }
    Line::from(spans)
}

/// The title of `track`, with a badge if it is explicit.
fn title_line<'a>(track: &'a TrackInfo, theme: &Theme) -> Line<'a> {
    let mut spans = vec![Span::styled(
        track.title.as_deref().unwrap_or("Unknown title"),
        Style::default().add_modifier(Modifier::BOLD),
    )];
    if track.album_explicit {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(" E ", theme.badge));
    }
    Line::from(spans)
}

/// The playback controls between the play modes, shuffle on the left and repeat on the right,
/// highlighting the playback status and the play modes that are on.
fn controls_line(group: &GroupState, symbols: &Symbols, theme: &Theme) -> Line<'static> {
    let is_playing = group.playback.is_some_and(|p| p.is_playing);
    let status = if is_playing {
        symbols.playing
    } else {
        symbols.paused
    };
    let modes = group.playback.map(|p| p.play_modes).unwrap_or_default();
    let repeat = if modes.repeat_one {
        symbols.repeat_one
    } else {
        symbols.repeat
    };
    let items = [
        (symbols.shuffle, modes.shuffle, Action::Shuffle),
        (symbols.previous, false, Action::Previous),
        (status, is_playing, Action::PlayPause),
        (symbols.next, false, Action::Next),
        (repeat, modes.repeat || modes.repeat_one, Action::Repeat),
    ];

    let mut spans = Vec::new();
    for (i, (label, on, action)) in items.into_iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("   "));
        }
        let style = match (supports(group, action), on) {
            (false, _) => Style::default().fg(theme.muted),
            (true, true) => Style::default().fg(theme.on).add_modifier(Modifier::BOLD),
            (true, false) => Style::default().fg(theme.secondary),
        };
        spans.push(Span::styled(label, style));
    }
    Line::from(spans)
}

/// A bar showing how far into the current track `group` is in `bar_area`, between the elapsed and
/// remaining times, in the rest of `area`.
fn render_progress(
    group: &GroupState,
    theme: &Theme,
    frame: &mut Frame,
    area: Rect,
    bar_area: Rect,
) {
    let playback = group.playback;
    let duration = group
        .now_playing
        .as_ref()
        .map_or(0, |track| track.duration_secs.unwrap_or(0));
    let elapsed = match (&group.now_playing, playback) {
        (Some(_), Some(p)) => {
            let elapsed = u32::try_from(p.position(Instant::now()).as_secs()).unwrap_or(u32::MAX);
            // The position is worked out locally, and can run past the end of the track until the
            // speakers report the next one.
            if duration != 0 {
                elapsed.min(duration)
            } else {
                elapsed
            }
        }
        _ => 0,
    };
    let ratio = if duration != 0 {
        f64::clamp(f64::from(elapsed) / f64::from(duration), 0.0, 1.0)
    } else {
        0.0
    };

    // Leave a space on each side of the bar.
    let elapsed_area = Rect {
        width: bar_area.x.saturating_sub(area.x + 1),
        ..area
    };
    let remaining_x = bar_area.right() + 1;
    let remaining_area = Rect {
        x: remaining_x,
        width: area.right().saturating_sub(remaining_x),
        ..area
    };

    let bar = Gauge::default()
        .use_unicode(true)
        .gauge_style(Style::default().fg(theme.on).bg(theme.track))
        // Without a label, the gauge shows the percentage.
        .label("")
        .ratio(ratio);

    frame.render_widget(
        Paragraph::new(format_duration(elapsed)).right_aligned(),
        elapsed_area,
    );
    frame.render_widget(bar, bar_area);
    frame.render_widget(
        Paragraph::new(remaining_time(elapsed, duration)),
        remaining_area,
    );
}

/// The time left in a track lasting `duration` seconds, `elapsed` seconds into it, e.g. `-3:07`,
/// or nothing if its duration isn't known, e.g. for radio.
fn remaining_time(elapsed: u32, duration: u32) -> String {
    if duration == 0 {
        return String::new();
    }
    format!("-{}", format_duration(duration.saturating_sub(elapsed)))
}

/// The volume of `group`, as a slider between an icon showing its level and its value.
///
/// Returns where the slider is.
fn render_volume(
    group: &GroupState,
    symbols: &Symbols,
    theme: &Theme,
    frame: &mut Frame,
    area: Rect,
) -> Rect {
    let [icon_area, slider_area, value_area] = Layout::horizontal([
        Constraint::Length(VOLUME_SIDE_WIDTH),
        Constraint::Min(1),
        Constraint::Length(VOLUME_SIDE_WIDTH),
    ])
    .spacing(1)
    .areas(area);

    frame.render_widget(
        Line::from(symbols.volume_level(group.volume)).right_aligned(),
        icon_area,
    );
    frame.render_widget(volume_slider(group.volume, theme), slider_area);
    frame.render_widget(volume_text(group.volume), value_area);
    slider_area
}

/// A slider showing `volume`, if it is known.
pub(super) fn volume_slider(volume: Option<u8>, theme: &Theme) -> VolumeSlider<'_> {
    VolumeSlider { volume, theme }
}

/// A horizontal slider showing a volume, with a handle if it is known.
pub(super) struct VolumeSlider<'a> {
    volume: Option<u8>,
    theme: &'a Theme,
}

impl Widget for VolumeSlider<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }
        let y = area.y + area.height / 2;
        // The handle goes from the first column to the last, and the bar is filled up to it, so
        // that there is never a gap between them.
        let volume = u32::from(self.volume.unwrap_or(0).min(100));
        let handle = u16::try_from((u32::from(area.width - 1) * volume + 50) / 100).unwrap_or(0);
        for x in 0..area.width {
            let (symbol, color) = if x < handle {
                ("━", self.theme.slider)
            } else {
                ("─", self.theme.track)
            };
            buf[(area.x + x, y)].set_symbol(symbol).set_fg(color);
        }
        if self.volume.is_some() {
            buf[(area.x + handle, y)]
                .set_symbol("●")
                .set_fg(self.theme.slider_handle);
        }
    }
}

/// `volume`, right-aligned on 3 columns, or `--` if it isn't known.
pub(super) fn volume_text(volume: Option<u8>) -> String {
    volume.map_or_else(|| " --".to_owned(), |v| format!("{v:>3}"))
}

/// The time left on a sleep timer running out at `end`, to the right of the controls, which are
/// `controls_width` wide in the middle of `area`, if there is room for it.
fn render_sleep_timer(
    end: Instant,
    symbols: &Symbols,
    theme: &Theme,
    frame: &mut Frame,
    area: Rect,
    controls_width: u16,
) {
    // As far from the controls as they are from each other
    const GAP: u16 = 3;
    let timer = Line::styled(
        format!("{} {}", symbols.sleep_timer, sleep_timer_text(end)),
        Style::default().fg(theme.on),
    );
    let width = u16::try_from(timer.width()).unwrap_or(u16::MAX);
    // Where `Paragraph::centered` puts the controls
    let x = area.x + area.width.saturating_sub(controls_width) / 2 + controls_width + GAP;
    let timer_area = Rect { x, width, ..area }.intersection(area);
    if timer_area.width == width {
        frame.render_widget(timer, timer_area);
    }
}

/// The time left on a sleep timer running out at `end`: hours, minutes and seconds, or minutes
/// and seconds if under an hour.
pub(super) fn sleep_timer_text(end: Instant) -> String {
    let secs = end.saturating_duration_since(Instant::now()).as_secs();
    let (hours, minutes, seconds) = (secs / 3600, secs / 60 % 60, secs % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

pub(super) fn format_duration(secs: u32) -> String {
    let minutes = secs / 60;
    let seconds = secs % 60;

    format!("{minutes}:{seconds:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn art_placeholder_is_a_square_with_an_icon_in_the_middle() {
        use ratatui::{Terminal, backend::TestBackend};

        let mut terminal = Terminal::new(TestBackend::new(50, 9)).unwrap();
        terminal
            .draw(|f| {
                let font_size = FontSize::new(10, 20);
                let symbols = Symbols::choose(false);
                render_art_placeholder(font_size, symbols, &Theme::TERMINAL, f, f.area());
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let background = Theme::TERMINAL.art_placeholder.bg;
        let rows: Vec<String> = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| {
                        let cell = &buffer[(x, y)];
                        match (cell.symbol(), cell.bg == background.unwrap()) {
                            ("♪", true) => '♪',
                            (_, true) => '#',
                            _ => '.',
                        }
                    })
                    .collect()
            })
            .collect();
        // Cells are twice as tall as they are wide.
        let square = format!("{}{}{}", ".".repeat(16), "#".repeat(18), ".".repeat(16));
        let middle = format!(
            "{}{}♪{}{}",
            ".".repeat(16),
            "#".repeat(9),
            "#".repeat(8),
            ".".repeat(16)
        );
        for (y, row) in rows.iter().enumerate() {
            assert_eq!(row, if y == 4 { &middle } else { &square }, "{y}");
        }
    }

    #[test]
    fn source_kind_is_a_label_or_an_icon() {
        let source = Source {
            service: Some("Spotify".to_owned()),
            kind: Some("playlist".to_owned()),
            name: Some("Chill".to_owned()),
        };
        let line = |nerd_font| {
            source_line(&source, Symbols::choose(nerd_font), &Theme::TERMINAL).to_string()
        };
        assert_eq!(line(false), "Spotify · Playlist · Chill");
        assert_eq!(line(true), "\u{f04c7} Spotify · \u{f0cb8} Chill");

        let unknown_kind = Source {
            kind: Some("mystery".to_owned()),
            ..source.clone()
        };
        let line = source_line(&unknown_kind, Symbols::choose(true), &Theme::TERMINAL);
        assert_eq!(line.to_string(), "\u{f04c7} Spotify · Chill");
    }

    #[test]
    fn remaining_time_counts_down_to_the_end_of_the_track() {
        assert_eq!(remaining_time(0, 432), "-7:12");
        assert_eq!(remaining_time(425, 432), "-0:07");
        assert_eq!(remaining_time(432, 432), "-0:00");
        // Unknown duration, e.g. radio
        assert_eq!(remaining_time(65, 0), "");
    }

    #[test]
    fn volume_slider_handle_follows_bar() {
        for width in 1..=40 {
            let area = Rect::new(0, 0, width, 1);
            for volume in 0..=100 {
                let mut buf = Buffer::empty(area);
                volume_slider(Some(volume), &Theme::TERMINAL).render(area, &mut buf);
                let line: String = (0..width).map(|x| buf[(x, 0)].symbol()).collect();
                let handle = line.find('●').expect("the handle is shown");
                assert!(
                    line[..handle].chars().all(|c| c == '━'),
                    "{volume}% on {width} columns: {line}"
                );
            }
        }
    }
}
