use std::{collections::HashSet, time::Duration};

use clap::crate_version;
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Margin, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType::Rounded, Paragraph, Wrap},
};
use sinuous_client::{GroupId, PlayerId};
use url::Url;

use crate::artwork::Artwork;
use crate::config::{UiConfig, UiSetting};
use crate::sonos::{Command, ErrorMessage, GroupState, HouseholdState};
use crate::symbols::Symbols;
use crate::theme::{THEMES, Theme};

mod input;
mod now_playing;
mod panel;
mod popup;
mod system_info;

pub use input::handle_input;
use now_playing::{AlbumArt, now_playing_width, render_now_playing};
use panel::{render_group_editor, render_group_panel};
use popup::{
    render_config_dialog, render_favorites_picker, render_help, render_playback_menu,
    render_sleep_timer_picker, render_theme_picker, render_volume_mixer,
};
use system_info::render_system_info;

/// How long errors are displayed for.
const ERROR_DISPLAY_TIME: Duration = Duration::from_secs(5);

/// How wide the panel on the left (the groups, or the speakers of one) is: a third of the screen,
/// within these bounds, unless the keys to use need more room.
const GROUP_PANEL_MIN_WIDTH: u16 = 34;
const GROUP_PANEL_MAX_WIDTH: u16 = 48;

/// The keys to use in the group panel, and in the group editor.
const GROUP_PANEL_HELP: &str = " ↑↓ • ENTER select • ESC close ";
const GROUP_EDITOR_HELP: &str = " ↑↓ • SPACE toggle • ENTER apply • ESC cancel ";

/// What the user is looking at, independently of the state of the speakers.
#[derive(Debug, Default)]
pub struct UiState {
    selected_group: Option<Selection>,
    mode: Mode,
    /// The last error that wasn't about the speakers.
    error: Option<ErrorMessage>,
}

/// What is open beside, or on top of, what the selected group is playing.
#[derive(Debug, Default)]
enum Mode {
    #[default]
    NowPlaying,
    /// The group panel, on the left, with the group at this index highlighted.
    GroupPicker(PanelHighlight),
    /// The group editor, in the panel on the left.
    GroupEditor(GroupEditor),
    /// The favorites picker, with the favorite at this index highlighted.
    FavoritesPicker(usize),
    /// The volume mixer, with this row highlighted: 0 for the selected group, then its members.
    VolumeMixer(usize),
    /// The playback menu, and the sleep timer picker on top of it, if open.
    PlaybackMenu {
        /// The item highlighted, as an index in [`PlaybackItem::ALL`].
        highlighted: usize,
        /// The choice highlighted in the sleep timer picker, as an index in
        /// [`SLEEP_TIMER_MINUTES`].
        sleep_timer_picker: Option<usize>,
    },
    /// The keyboard shortcuts.
    Help,
    /// The details of each speaker, in place of everything but the title bar.
    SystemInfo(Scroll),
    /// The configuration dialog, and the theme picker on top of it, if open.
    Config {
        /// The setting highlighted: one of [`UiSetting::ALL`], or the theme after them.
        highlighted: usize,
        /// The theme highlighted in the theme picker, as an index in [`THEMES`].
        theme_picker: Option<usize>,
    },
}

/// What the playback menu lists.
#[derive(Debug, Clone, Copy)]
enum PlaybackItem {
    Crossfade,
    SleepTimer,
}

impl PlaybackItem {
    const ALL: [Self; 2] = [Self::Crossfade, Self::SleepTimer];
}

/// What the sleep timer can be set to, in minutes, or off.
const SLEEP_TIMER_MINUTES: [Option<u64>; 7] = [
    None,
    Some(15),
    Some(30),
    Some(45),
    Some(60),
    Some(90),
    Some(120),
];

/// What a key press asks for, besides changing what is shown.
#[derive(Debug)]
pub enum Request {
    /// Send a command to the speakers.
    Speakers(Command),
    /// Turn a setting on or off, and save it.
    Toggle(UiSetting),
    /// Use the theme with this name, and save it.
    SetTheme(String),
    /// Exit the app.
    Quit,
}

/// The group the user is looking at.
#[derive(Debug)]
struct Selection {
    group: GroupId,
    /// Its coordinator when it was last seen, to follow it into the group it joins if this one is
    /// gone. A group keeps its ID when its members change, but a speaker leaving a group gets a
    /// new one.
    coordinator: PlayerId,
}

impl Selection {
    fn of(group: &GroupState) -> Self {
        Self {
            group: group.id.clone(),
            coordinator: group.coordinator_id.clone(),
        }
    }
}

/// Speakers being added to or removed from a group.
#[derive(Debug)]
struct GroupEditor {
    group: GroupId,
    /// The highlighted speaker, as an index in [`HouseholdState::players`].
    highlight: PanelHighlight,
    /// The speakers to have in the group.
    members: HashSet<PlayerId>,
}

impl GroupEditor {
    /// An editor for the speakers of `group`, with its coordinator highlighted.
    fn new(group: &GroupState, state: &HouseholdState) -> Self {
        let highlighted = state
            .players
            .iter()
            .position(|p| p.id == group.coordinator_id)
            .unwrap_or(0);
        Self {
            group: group.id.clone(),
            highlight: PanelHighlight::new(highlighted),
            members: group.player_ids.iter().cloned().collect(),
        }
    }
}

/// How far a fullscreen view is scrolled.
#[derive(Debug, Clone, Copy, Default)]
struct Scroll {
    /// The first line shown.
    offset: usize,
    /// How many lines were shown last time, to scroll by pages.
    page: usize,
}

/// The highlighted item of a list in the panel on the left, and the first one shown.
#[derive(Debug, Clone, Copy)]
struct PanelHighlight {
    item: usize,
    first: usize,
}

impl PanelHighlight {
    fn new(item: usize) -> Self {
        Self { item, first: 0 }
    }

    /// Scroll as little as possible for the highlighted item to be among the `shown` ones.
    fn scroll(&mut self, shown: usize) {
        self.first = self
            .first
            .min(self.item)
            .max((self.item + 1).saturating_sub(shown));
    }
}

impl UiState {
    /// Keep the selection valid for the latest `state`, e.g. when the selected group is gone.
    pub fn sync(&mut self, state: &HouseholdState) {
        // When the selected group is gone, follow its coordinator to the group it is in now.
        let selected = self
            .selected_group
            .as_ref()
            .and_then(|selected| {
                state.group(&selected.group).or_else(|| {
                    state
                        .groups
                        .iter()
                        .find(|g| g.player_ids.contains(&selected.coordinator))
                })
            })
            .or_else(|| state.groups.first());
        self.selected_group = selected.map(Selection::of);

        let mixer_rows = self
            .current_group(state)
            .map(|(_, group)| state.members(group).count() + 1);
        match &mut self.mode {
            Mode::GroupPicker(_) if state.groups.is_empty() => self.mode = Mode::NowPlaying,
            Mode::GroupPicker(highlight) => clamp(&mut highlight.item, state.groups.len()),
            Mode::GroupEditor(editor) if state.group(&editor.group).is_none() => {
                self.mode = Mode::NowPlaying;
            }
            Mode::GroupEditor(editor) => clamp(&mut editor.highlight.item, state.players.len()),
            Mode::FavoritesPicker(highlighted) => clamp(highlighted, state.favorites.len()),
            Mode::PlaybackMenu { highlighted, .. } => clamp(highlighted, PlaybackItem::ALL.len()),
            Mode::VolumeMixer(highlighted) => match mixer_rows {
                Some(rows) => clamp(highlighted, rows),
                None => self.mode = Mode::NowPlaying,
            },
            Mode::NowPlaying | Mode::Help | Mode::SystemInfo(_) | Mode::Config { .. } => {}
        }
    }

    /// Whether a popup is open in the middle of the screen. The group panel and the group editor
    /// are beside what is playing, not on top of it, and the system info takes its place.
    fn popup_open(&self) -> bool {
        !matches!(
            self.mode,
            Mode::NowPlaying | Mode::GroupPicker(_) | Mode::GroupEditor(_) | Mode::SystemInfo(_)
        )
    }

    /// The name of the theme to show instead of the configured one, while choosing one.
    pub fn previewed_theme(&self) -> Option<&'static str> {
        let Mode::Config {
            theme_picker: Some(highlighted),
            ..
        } = self.mode
        else {
            return None;
        };
        Some(&THEMES.get(highlighted)?.name)
    }

    /// Show `message` in the header for a while.
    pub fn show_error(&mut self, message: String) {
        self.error = Some(ErrorMessage::new(message));
    }

    /// The error to show in the header, if any: the most recent one, if it is recent enough.
    fn shown_error<'a>(&'a self, state: &'a HouseholdState) -> Option<&'a ErrorMessage> {
        [state.last_error.as_ref(), self.error.as_ref()]
            .into_iter()
            .flatten()
            .filter(|err| is_shown(err))
            .max_by_key(|err| err.at)
    }

    /// The selected group and its index.
    fn current_group<'a>(&self, state: &'a HouseholdState) -> Option<(usize, &'a GroupState)> {
        let id = &self.selected_group.as_ref()?.group;
        state.groups.iter().enumerate().find(|(_, g)| &g.id == id)
    }
}

/// Keep `highlighted` within a list of `len` rows.
fn clamp(highlighted: &mut usize, len: usize) {
    *highlighted = (*highlighted).min(len.saturating_sub(1));
}

/// The album art to show.
pub fn artwork_url<'a>(state: &'a HouseholdState, ui: &UiState) -> Option<&'a Url> {
    let (_, group) = ui.current_group(state)?;
    group.now_playing.as_ref()?.image_url.as_ref()
}

/// Whether what is displayed changes over time, and so needs redrawing even when the state doesn't
/// change.
pub fn is_animated(state: &HouseholdState, ui: &UiState) -> bool {
    let showing_error = ui.shown_error(state).is_some();
    let group = ui.current_group(state).map(|(_, group)| group);
    let advancing = group
        .and_then(|group| group.playback)
        .is_some_and(|playback| playback.is_advancing());
    // The playback menu shows the time left on the sleep timer.
    let counting_down = matches!(ui.mode, Mode::PlaybackMenu { .. })
        && group.is_some_and(|group| group.sleep_timer.is_some());
    showing_error || advancing || counting_down
}

pub fn render_ui(
    frame: &mut Frame,
    state: &HouseholdState,
    ui: &mut UiState,
    artwork: &mut Artwork,
    settings: &UiConfig,
    theme: &Theme,
) {
    let symbols = Symbols::choose(settings.nerd_font);
    frame.render_widget(Block::new().style(theme.base()), frame.area());
    let Some((group_index, group)) = ui.current_group(state) else {
        return render_message(frame, theme, "No speakers found.\n\nPress q to quit.");
    };
    let [title, body] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(frame.area());

    render_title_bar(state, ui, group, symbols, theme, frame, title);
    if let Mode::SystemInfo(scroll) = &mut ui.mode {
        return render_system_info(state, theme, scroll, frame, body);
    }
    let panel_help = match ui.mode {
        Mode::GroupPicker(_) => Some(GROUP_PANEL_HELP),
        Mode::GroupEditor(_) => Some(GROUP_EDITOR_HELP),
        _ => None,
    };
    let now_playing = match panel_help {
        Some(help) => {
            let help_width = u16::try_from(Span::raw(help).width() + 2).unwrap_or(u16::MAX);
            let width = (body.width / 3)
                .clamp(GROUP_PANEL_MIN_WIDTH, GROUP_PANEL_MAX_WIDTH)
                .max(help_width);
            let [panel, rest] =
                Layout::horizontal([Constraint::Length(width), Constraint::Fill(1)]).areas(body);
            // When what is playing doesn't fit beside the panel, the panel takes its place rather
            // than covering part of it.
            let fits = rest.width >= now_playing_width(group, symbols, theme);
            let panel = if fits { panel } else { body };
            match &mut ui.mode {
                Mode::GroupPicker(highlight) => {
                    render_group_panel(state, symbols, theme, group_index, highlight, frame, panel)
                }
                Mode::GroupEditor(editor) => {
                    render_group_editor(state, symbols, theme, editor, frame, panel);
                }
                _ => {}
            }
            fits.then_some(rest)
        }
        None => Some(body),
    };
    if let Some(area) = now_playing {
        let album_art = match (settings.album_art, ui.popup_open()) {
            (false, _) => AlbumArt::Off,
            // Images can be drawn on top of the text, so hide the album art behind popups.
            (true, true) => AlbumArt::Placeholder,
            (true, false) => AlbumArt::Shown,
        };
        render_now_playing(group, symbols, theme, frame, area, artwork, album_art);
    }

    // Popups, on top of everything else
    match ui.mode {
        Mode::FavoritesPicker(highlighted) => {
            render_favorites_picker(state, theme, highlighted, frame);
        }
        Mode::VolumeMixer(highlighted) => {
            render_volume_mixer(state, group, theme, highlighted, frame);
        }
        Mode::PlaybackMenu {
            highlighted,
            sleep_timer_picker,
        } => {
            render_playback_menu(group, symbols, theme, highlighted, frame);
            if let Some(highlighted) = sleep_timer_picker {
                render_sleep_timer_picker(theme, highlighted, frame);
            }
        }
        Mode::Help => render_help(theme, frame),
        Mode::Config {
            highlighted,
            theme_picker,
        } => {
            render_config_dialog(settings, symbols, theme, highlighted, frame);
            if let Some(highlighted) = theme_picker {
                render_theme_picker(&settings.theme, theme, highlighted, frame);
            }
        }
        Mode::NowPlaying | Mode::GroupPicker(_) | Mode::GroupEditor(_) | Mode::SystemInfo(_) => {}
    }
}

/// Display a message in place of the main UI.
fn render_message(frame: &mut Frame, theme: &Theme, message: &str) {
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
                .border_style(Style::new().fg(theme.border))
                .title(format!(" Sinuous {} ", crate_version!())),
        );
    frame.render_widget(paragraph, area);
}

/// The name of the app and of the current group, or the latest error, and the main shortcuts.
fn render_title_bar(
    state: &HouseholdState,
    ui: &UiState,
    group: &GroupState,
    symbols: &Symbols,
    theme: &Theme,
    frame: &mut Frame,
    area: Rect,
) {
    // Muted text would be hard to read on the background of the header.
    let separator = Style::default().fg(theme.secondary);
    let shortcuts = shortcuts_line(symbols, theme);
    let shortcuts_width = u16::try_from(shortcuts.width()).unwrap_or(u16::MAX);
    let [title_area, shortcuts_area] =
        Layout::horizontal([Constraint::Min(1), Constraint::Length(shortcuts_width)])
            .spacing(1)
            .areas(area.inner(Margin::new(1, 0)));

    let mut header = vec![
        Span::styled(
            format!("Sinuous {}", crate_version!()),
            Style::default()
                .fg(theme.title)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" │ ", separator),
    ];
    match ui.shown_error(state) {
        Some(err) => header.push(Span::styled(
            err.message.as_str(),
            Style::default().fg(theme.error),
        )),
        None => {
            header.push(Span::raw(format!("{} ", symbols.playing_on)));
            header.push(Span::styled(&group.name, Style::default().fg(theme.name)));
        }
    }

    frame.render_widget(
        Block::new().style(Style::default().bg(theme.title_background)),
        area,
    );
    frame.render_widget(Line::from(header), title_area);
    frame.render_widget(shortcuts, shortcuts_area);
}

/// The keys to press for the help, the configuration dialog and quitting.
fn shortcuts_line(symbols: &Symbols, theme: &Theme) -> Line<'static> {
    let entries = [
        ("?", symbols.help),
        (",", symbols.config),
        ("q", symbols.quit),
    ];
    // Muted text would be hard to read on the background of the header.
    let separator = Style::default().fg(theme.secondary);
    let mut spans = Vec::new();
    for (i, (key, label)) in entries.into_iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" • ", separator));
        }
        spans.push(Span::raw(key));
        spans.push(Span::styled(
            format!(" {label}"),
            Style::default().fg(theme.secondary),
        ));
    }
    Line::from(spans)
}

fn is_shown(error: &ErrorMessage) -> bool {
    error.at.elapsed() < ERROR_DISPLAY_TIME
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::*;
    use crate::sonos::PlayerInfo;

    pub(super) fn household() -> HouseholdState {
        let player = |id: &str, name: &str| PlayerInfo {
            id: id.into(),
            name: name.to_owned(),
            model: Some("Move".to_owned()),
            volume: Some(20),
        };
        HouseholdState {
            groups: vec![
                GroupState::new(
                    "bedroom".into(),
                    "Bedroom + Office".to_owned(),
                    "b".into(),
                    vec!["b".into(), "o".into()],
                ),
                GroupState::new(
                    "kitchen".into(),
                    "Kitchen".to_owned(),
                    "k".into(),
                    vec!["k".into()],
                ),
            ],
            players: vec![
                player("b", "Bedroom"),
                player("k", "Kitchen"),
                player("o", "Office"),
            ],
            ..Default::default()
        }
    }

    pub(super) fn press(
        keys: &[KeyCode],
        state: &HouseholdState,
        ui: &mut UiState,
    ) -> Option<Request> {
        keys.iter().fold(None, |_, code| {
            handle_input(
                &KeyEvent::new(*code, KeyModifiers::NONE),
                state,
                &UiConfig::default(),
                ui,
            )
        })
    }

    #[test]
    fn selection_follows_its_coordinator_into_the_group_it_joins() {
        let mut state = household();
        let mut ui = UiState::default();
        ui.sync(&state);
        press(
            &[KeyCode::Char('g'), KeyCode::Char('j'), KeyCode::Enter],
            &state,
            &mut ui,
        );
        assert_eq!(ui.current_group(&state).unwrap().1.name, "Kitchen");

        // The kitchen joined the bedroom, e.g. from the Sonos app: its group is gone, and the group
        // it joined keeps its ID.
        state.groups = vec![GroupState::new(
            "bedroom".into(),
            "Bedroom + Office + Kitchen".to_owned(),
            "b".into(),
            vec!["b".into(), "o".into(), "k".into()],
        )];
        ui.sync(&state);
        assert_eq!(
            ui.current_group(&state).unwrap().1.name,
            "Bedroom + Office + Kitchen"
        );

        // The kitchen left again, getting a group with a new ID: the group shown stays.
        state.groups = vec![
            GroupState::new(
                "bedroom".into(),
                "Bedroom + Office".to_owned(),
                "b".into(),
                vec!["b".into(), "o".into()],
            ),
            GroupState::new(
                "kitchen:2".into(),
                "Kitchen".to_owned(),
                "k".into(),
                vec!["k".into()],
            ),
        ];
        ui.sync(&state);
        assert_eq!(ui.current_group(&state).unwrap().1.name, "Bedroom + Office");
    }

    #[test]
    fn group_panel_takes_the_place_of_what_is_playing_when_both_dont_fit() {
        use ratatui::{Terminal, backend::TestBackend};

        let state = household();
        let mut ui = UiState::default();
        ui.sync(&state);
        press(&[KeyCode::Char('g')], &state, &mut ui);

        let settings = UiConfig::default();
        let group = &state.groups[0];
        let needed =
            now_playing_width(group, Symbols::choose(settings.nerd_font), &Theme::TERMINAL);
        for (width, shown) in [
            (GROUP_PANEL_MIN_WIDTH + needed, true),
            (GROUP_PANEL_MIN_WIDTH + needed - 1, false),
        ] {
            let mut terminal = Terminal::new(TestBackend::new(width, 20)).unwrap();
            let (mut artwork, _) = Artwork::new(ratatui_image::picker::Picker::halfblocks(), None);
            terminal
                .draw(|f| {
                    render_ui(
                        f,
                        &state,
                        &mut ui,
                        &mut artwork,
                        &settings,
                        &Theme::TERMINAL,
                    );
                })
                .unwrap();
            let screen: String = terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|cell| cell.symbol())
                .collect();
            assert!(screen.contains("Groups"), "{width}");
            assert_eq!(screen.contains("Nothing playing"), shown, "{width}");
        }
    }

    #[test]
    fn placeholder_shows_where_the_album_art_goes_when_there_is_none() {
        use ratatui::{Terminal, backend::TestBackend};

        let state = household();
        let mut ui = UiState::default();
        ui.sync(&state);
        for album_art in [true, false] {
            let settings = UiConfig {
                album_art,
                ..UiConfig::default()
            };
            let mut terminal = Terminal::new(TestBackend::new(100, 40)).unwrap();
            let (mut artwork, _) = Artwork::new(ratatui_image::picker::Picker::halfblocks(), None);
            terminal
                .draw(|f| {
                    render_ui(
                        f,
                        &state,
                        &mut ui,
                        &mut artwork,
                        &settings,
                        &Theme::TERMINAL,
                    );
                })
                .unwrap();
            let shown = terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .any(|cell| cell.symbol() == "♪");
            assert_eq!(shown, album_art);
        }
    }

    #[test]
    fn group_editor_is_a_panel_beside_what_is_playing() {
        use ratatui::{Terminal, backend::TestBackend};

        let state = household();
        let mut ui = UiState::default();
        ui.sync(&state);
        press(&[KeyCode::Char('G')], &state, &mut ui);

        let settings = UiConfig::default();
        let mut terminal = Terminal::new(TestBackend::new(120, 20)).unwrap();
        let (mut artwork, _) = Artwork::new(ratatui_image::picker::Picker::halfblocks(), None);
        terminal
            .draw(|f| {
                render_ui(
                    f,
                    &state,
                    &mut ui,
                    &mut artwork,
                    &settings,
                    &Theme::TERMINAL,
                );
            })
            .unwrap();
        let screen: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        for shown in [
            "Group speakers",
            GROUP_EDITOR_HELP,
            "Bedroom Move",
            "In this group · coordinator",
            "On its own",
            "Nothing playing",
        ] {
            assert!(screen.contains(shown), "{shown}");
        }
    }

    #[test]
    fn volume_mixer_shows_models_with_sliders_lined_up() {
        use ratatui::{Terminal, backend::TestBackend};

        let mut state = household();
        state.players[0].model = Some("Arc Ultra".to_owned());
        let mut ui = UiState::default();
        ui.sync(&state);
        press(&[KeyCode::Char('v')], &state, &mut ui);

        let mut terminal = Terminal::new(TestBackend::new(100, 20)).unwrap();
        let (mut artwork, _) = Artwork::new(ratatui_image::picker::Picker::halfblocks(), None);
        terminal
            .draw(|f| {
                let settings = UiConfig::default();
                render_ui(
                    f,
                    &state,
                    &mut ui,
                    &mut artwork,
                    &settings,
                    &Theme::TERMINAL,
                );
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let rows: Vec<String> = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect()
            })
            .collect();
        let slider_start = |label: &str| {
            let row = rows
                .iter()
                .find(|row| row.contains(label))
                .unwrap_or_else(|| panic!("{label}"));
            row.char_indices()
                .position(|(_, c)| matches!(c, '━' | '●' | '─'))
                .unwrap()
        };
        let group = slider_start("Group");
        assert_eq!(slider_start("Bedroom Arc Ultra"), group);
        assert_eq!(slider_start("Office Move"), group);
    }

    #[test]
    fn panel_scrolls_as_little_as_possible() {
        let mut highlight = PanelHighlight::new(0);
        let mut firsts = Vec::new();
        for item in [0, 1, 2, 3, 4, 3, 2, 1, 0] {
            highlight.item = item;
            highlight.scroll(3);
            firsts.push(highlight.first);
        }
        assert_eq!(firsts, [0, 0, 0, 1, 2, 2, 2, 1, 0]);

        // Opening it on an item further down shows that item at the bottom.
        let mut highlight = PanelHighlight::new(7);
        highlight.scroll(3);
        assert_eq!(highlight.first, 5);
    }
}
