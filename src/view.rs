use std::{collections::HashSet, time::Duration};

use clap::crate_version;
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Margin, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType::Rounded, Paragraph, Wrap},
};
use sinuous_client::{
    GroupId, PlayerId,
    history::{ContentType, HistoryItem},
};
use url::Url;

use crate::artwork::Artwork;
use crate::config::{UiConfig, UiSetting};
use crate::sonos::{
    Command, ErrorMessage, GroupAction, GroupQueue, GroupState, HouseholdState, QueueContents,
};
use crate::symbols::Symbols;
use crate::theme::{THEMES, Theme};

mod footer;
mod input;
mod now_playing;
mod panel;
mod popup;
mod system_info;

use footer::render_footer;
pub use input::handle_input;
use now_playing::{
    AlbumArt, PLAYER_BAR_HEIGHT, now_playing_width, render_now_playing, render_player_bar,
};
use panel::{render_browser, render_group_editor, render_group_panel};
use popup::{
    render_config_dialog, render_help, render_playback_menu, render_sleep_timer_picker,
    render_theme_picker, render_volume_mixer,
};
use system_info::render_system_info;

/// How long errors are displayed for.
const ERROR_DISPLAY_TIME: Duration = Duration::from_secs(5);

/// How wide the panel on the left (the groups, or the speakers of one) is: a third of the screen,
/// within these bounds.
const GROUP_PANEL_MIN_WIDTH: u16 = 34;
const GROUP_PANEL_MAX_WIDTH: u16 = 48;

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
    /// The queue, the history, the favorites or the playlists, in the panel on the left.
    Browser(Browser),
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
    /// The details of each speaker, in place of everything but the title bar and the footer.
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

/// The lists of things played, or to play, shown in the browser, each in its own tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    /// The queue of the selected group.
    Queue,
    /// What was played recently in the household.
    History,
    Favorites,
    /// The Sonos playlists of the household.
    Playlists,
}

impl Tab {
    const ALL: [Self; 4] = [Self::Queue, Self::History, Self::Favorites, Self::Playlists];

    fn title(self) -> &'static str {
        match self {
            Self::Queue => "Queue",
            Self::History => "History",
            Self::Favorites => "Favorites",
            Self::Playlists => "Playlists",
        }
    }

    /// The key opening the browser on this tab, switching to it, or closing it from it.
    fn key(self) -> char {
        match self {
            Self::Queue => 'l',
            Self::History => 'h',
            Self::Favorites => 'f',
            Self::Playlists => 'P',
        }
    }

    /// The tab `offset` tabs away from this one, wrapping around.
    fn cycle(self, offset: isize) -> Self {
        let index = Self::ALL.iter().position(|tab| *tab == self).unwrap_or(0);
        let len = Self::ALL.len().cast_signed();
        Self::ALL[(index.cast_signed() + offset)
            .rem_euclid(len)
            .cast_unsigned()]
    }

    /// How many items it lists for `group`.
    fn len(self, state: &HouseholdState, group: &GroupState) -> usize {
        match self {
            Self::Queue => match GroupQueue::of(state.queue.as_ref(), &group.id) {
                QueueContents::Loaded(queue) => queue.items.len(),
                QueueContents::Loading | QueueContents::Failed => 0,
            },
            Self::History => state.history.len(),
            Self::Favorites => state.favorites.len(),
            Self::Playlists => state.playlists.len(),
        }
    }

    /// What playing the item at `index` in `group` does, if it can be played.
    fn play(self, index: usize, state: &HouseholdState, group: &GroupState) -> Option<GroupAction> {
        let action = match self {
            // Skipping to a track only works while the group plays from its queue.
            Self::Queue => match GroupQueue::of(state.queue.as_ref(), &group.id) {
                QueueContents::Loaded(queue) if group.plays_from(queue) => {
                    GroupAction::SkipToTrack(queue.items.get(index)?.position)
                }
                QueueContents::Loaded(_) | QueueContents::Loading | QueueContents::Failed => {
                    return None;
                }
            },
            Self::History => {
                let item = state
                    .history
                    .get(index)
                    .filter(|item| can_play_again(item))?;
                GroupAction::LoadContent(item.id.clone(), item.r#type)
            }
            Self::Favorites => GroupAction::LoadFavorite(state.favorites.get(index)?.id.clone()),
            Self::Playlists => GroupAction::LoadPlaylist(state.playlists.get(index)?.id.clone()),
        };
        Some(action)
    }
}

/// Whether `item` of the history can be played again.
fn can_play_again(item: &HistoryItem) -> bool {
    // What the type of the content is needs to be given to play it.
    item.playable && item.r#type != ContentType::Unknown
}

/// The queue, the history, the favorites or the playlists, in tabs.
#[derive(Debug)]
struct Browser {
    tab: Tab,
    highlight: PanelHighlight,
    /// Whether to highlight the current track once the queue is read.
    find_current: bool,
}

impl Browser {
    /// The browser on `tab`, with its first item highlighted, or the current track for the queue.
    fn new(tab: Tab) -> Self {
        Self {
            tab,
            highlight: PanelHighlight::new(0),
            find_current: tab == Tab::Queue,
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
    /// How many items were shown last time, to move by pages.
    shown: usize,
}

impl PanelHighlight {
    fn new(item: usize) -> Self {
        Self {
            item,
            first: 0,
            shown: 0,
        }
    }

    /// Scroll as little as possible for the highlighted item to be among the `shown` ones.
    fn scroll(&mut self, shown: usize) {
        self.shown = shown;
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

        let current = self.current_group(state).map(|(_, group)| group);
        let mixer_rows = current.map(|group| state.members(group).count() + 1);
        match &mut self.mode {
            Mode::GroupPicker(_) if state.groups.is_empty() => self.mode = Mode::NowPlaying,
            Mode::GroupPicker(highlight) => clamp(&mut highlight.item, state.groups.len()),
            Mode::GroupEditor(editor) if state.group(&editor.group).is_none() => {
                self.mode = Mode::NowPlaying;
            }
            Mode::GroupEditor(editor) => clamp(&mut editor.highlight.item, state.players.len()),
            Mode::Browser(browser) => {
                let Some(group) = current else {
                    self.mode = Mode::NowPlaying;
                    return;
                };
                if browser.find_current
                    && let QueueContents::Loaded(queue) =
                        GroupQueue::of(state.queue.as_ref(), &group.id)
                {
                    browser.find_current = false;
                    let current = group
                        .queue_position
                        .filter(|_| group.plays_from(queue))
                        .and_then(|position| {
                            queue
                                .items
                                .iter()
                                .position(|item| item.position == position)
                        });
                    browser.highlight = PanelHighlight::new(current.unwrap_or(0));
                }
                clamp(&mut browser.highlight.item, browser.tab.len(state, group));
            }
            Mode::PlaybackMenu { highlighted, .. } => clamp(highlighted, PlaybackItem::ALL.len()),
            Mode::VolumeMixer(highlighted) => match mixer_rows {
                Some(rows) => clamp(highlighted, rows),
                None => self.mode = Mode::NowPlaying,
            },
            Mode::NowPlaying | Mode::Help | Mode::SystemInfo(_) | Mode::Config { .. } => {}
        }
    }

    /// Whether a popup is open in the middle of the screen. The panels on the left are beside
    /// what is playing, not on top of it, and the system info takes its place.
    fn popup_open(&self) -> bool {
        !matches!(
            self.mode,
            Mode::NowPlaying
                | Mode::GroupPicker(_)
                | Mode::GroupEditor(_)
                | Mode::Browser(_)
                | Mode::SystemInfo(_)
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

/// The group whose queue is shown, if any.
pub fn watched_queue(state: &HouseholdState, ui: &UiState) -> Option<GroupId> {
    match &ui.mode {
        Mode::Browser(browser) if browser.tab == Tab::Queue => {
            Some(ui.current_group(state)?.1.id.clone())
        }
        _ => None,
    }
}

/// Whether what is displayed changes over time, and so needs redrawing even when the state doesn't
/// change.
pub fn is_animated(state: &HouseholdState, ui: &UiState) -> bool {
    let showing_error = ui.shown_error(state).is_some();
    let group = ui.current_group(state).map(|(_, group)| group);
    let advancing = group
        .and_then(|group| group.playback)
        .is_some_and(|playback| playback.is_advancing());
    let counting_down = group.is_some_and(|group| group.sleep_timer.is_some());
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
    let [title, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    render_title_bar(state, ui, theme, frame, title);
    render_footer(state, ui, group, theme, frame, footer);
    if let Mode::SystemInfo(scroll) = &mut ui.mode {
        return render_system_info(state, theme, scroll, frame, body);
    }
    // The browser takes the whole width, above a bar showing what is playing.
    if let Mode::Browser(browser) = &mut ui.mode {
        let [panel, player] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(PLAYER_BAR_HEIGHT)])
                .areas(body);
        render_browser(state, group, symbols, theme, browser, frame, panel);
        return render_player_bar(group, symbols, theme, frame, player);
    }

    let now_playing = match ui.mode {
        Mode::GroupPicker(_) | Mode::GroupEditor(_) => {
            let width = (body.width / 3).clamp(GROUP_PANEL_MIN_WIDTH, GROUP_PANEL_MAX_WIDTH);
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
        _ => Some(body),
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

    // Popups, on top of everything else but the title bar and the footer, which shows the keys to
    // use in them
    match ui.mode {
        Mode::VolumeMixer(highlighted) => {
            render_volume_mixer(state, group, theme, highlighted, frame, body);
        }
        Mode::PlaybackMenu {
            highlighted,
            sleep_timer_picker,
        } => {
            render_playback_menu(group, symbols, theme, highlighted, frame, body);
            if let Some(highlighted) = sleep_timer_picker {
                render_sleep_timer_picker(theme, highlighted, frame, body);
            }
        }
        Mode::Help => render_help(theme, frame, body),
        Mode::Config {
            highlighted,
            theme_picker,
        } => {
            render_config_dialog(settings, symbols, theme, highlighted, frame, body);
            if let Some(highlighted) = theme_picker {
                render_theme_picker(&settings.theme, theme, highlighted, frame, body);
            }
        }
        Mode::NowPlaying
        | Mode::GroupPicker(_)
        | Mode::GroupEditor(_)
        | Mode::Browser(_)
        | Mode::SystemInfo(_) => {}
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

/// The name of the app, and the latest error if any.
fn render_title_bar(
    state: &HouseholdState,
    ui: &UiState,
    theme: &Theme,
    frame: &mut Frame,
    area: Rect,
) {
    // Muted text would be hard to read on the background of the header.
    let separator = Style::default().fg(theme.secondary);

    let mut header = vec![Span::styled(
        format!("Sinuous {}", crate_version!()),
        Style::default()
            .fg(theme.title)
            .add_modifier(Modifier::BOLD),
    )];
    if let Some(err) = ui.shown_error(state) {
        header.push(Span::styled(" │ ", separator));
        header.push(Span::styled(
            err.message.as_str(),
            Style::default().fg(theme.error),
        ));
    }

    frame.render_widget(
        Block::new().style(Style::default().bg(theme.title_background)),
        area,
    );
    frame.render_widget(Line::from(header), area.inner(Margin::new(1, 0)));
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
            battery: None,
            wifi: None,
        };
        HouseholdState {
            groups: vec![
                GroupState::new(
                    "bedroom".into(),
                    "Bedroom + 1".to_owned(),
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
            "Bedroom + 2".to_owned(),
            "b".into(),
            vec!["b".into(), "o".into(), "k".into()],
        )];
        ui.sync(&state);
        assert_eq!(ui.current_group(&state).unwrap().1.name, "Bedroom + 2");

        // The kitchen left again, getting a group with a new ID: the group shown stays.
        state.groups = vec![
            GroupState::new(
                "bedroom".into(),
                "Bedroom + 1".to_owned(),
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
        assert_eq!(ui.current_group(&state).unwrap().1.name, "Bedroom + 1");
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
            "SPACE toggle • ENTER apply",
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
    fn sleep_timer_shows_beside_the_controls_when_there_is_room() {
        use ratatui::{Terminal, backend::TestBackend};

        let controls_row = |state: &HouseholdState, width| {
            let mut ui = UiState::default();
            ui.sync(state);
            let mut terminal = Terminal::new(TestBackend::new(width, 30)).unwrap();
            let (mut artwork, _) = Artwork::new(ratatui_image::picker::Picker::halfblocks(), None);
            terminal
                .draw(|f| {
                    render_ui(
                        f,
                        state,
                        &mut ui,
                        &mut artwork,
                        &UiConfig::default(),
                        &Theme::TERMINAL,
                    );
                })
                .unwrap();
            let buffer = terminal.backend().buffer();
            (0..buffer.area.height)
                .map(|y| {
                    (0..buffer.area.width)
                        .map(|x| buffer[(x, y)].symbol())
                        .collect::<String>()
                })
                .find(|row| row.contains("shuffle"))
                .unwrap()
        };
        let without = household();
        let mut with = household();
        with.groups[0].sleep_timer =
            Some(std::time::Instant::now() + Duration::from_secs(20 * 60 + 30));

        let row = controls_row(&with, 80);
        let timer = row.find("🌙").expect(&row);
        assert!(row[timer..].contains("20:2"), "{row}");
        // The controls don't move to make room for it.
        let plain = controls_row(&without, 80);
        assert_eq!(row[..timer].trim_end(), plain.trim_end(), "{row}");

        // There is no room for it beside the controls.
        let narrow = controls_row(&with, 44);
        assert!(!narrow.contains("🌙"), "{narrow}");
    }

    /// Draw the UI, and return its rows.
    fn screen_rows(state: &HouseholdState, ui: &mut UiState, width: u16) -> Vec<String> {
        sized_screen_rows(state, ui, width, 30)
    }

    fn sized_screen_rows(
        state: &HouseholdState,
        ui: &mut UiState,
        width: u16,
        height: u16,
    ) -> Vec<String> {
        use ratatui::{Terminal, backend::TestBackend};

        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        let (mut artwork, _) = Artwork::new(ratatui_image::picker::Picker::halfblocks(), None);
        terminal
            .draw(|f| {
                render_ui(
                    f,
                    state,
                    ui,
                    &mut artwork,
                    &UiConfig::default(),
                    &Theme::TERMINAL,
                );
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect()
            })
            .collect()
    }

    /// The household, with a queue of three tracks read for the bedroom, the last one explicit.
    pub(super) fn household_with_queue() -> HouseholdState {
        use sinuous_client::queue::{Queue, QueueEntry};

        let entry = |position, name: &str| QueueEntry {
            position,
            name: Some(name.to_owned()),
            artist: Some("Boucle Infinie".to_owned()),
            album: Some("Summit".to_owned()),
            image_url: None,
            duration_millis: Some(324_000),
            uri: None,
            explicit: position == 3,
        };
        let mut state = household();
        state.queue = Some(GroupQueue {
            group: "bedroom".into(),
            contents: QueueContents::Loaded(std::sync::Arc::new(Queue {
                items: vec![
                    entry(1, "Escape"),
                    entry(2, "Contemplation"),
                    entry(3, "Ascension"),
                ],
                total: 3,
                version: "4".to_owned(),
            })),
        });
        state
    }

    #[test]
    fn queue_opens_on_the_current_track() {
        let mut state = household_with_queue();
        state.groups[0].queue_version = Some("4".to_owned());
        state.groups[0].queue_position = Some(2);
        let mut ui = UiState::default();
        ui.sync(&state);
        press(&[KeyCode::Char('l')], &state, &mut ui);
        assert_eq!(watched_queue(&state, &ui), Some("bedroom".into()));
        ui.sync(&state);

        let Mode::Browser(browser) = &ui.mode else {
            panic!("{:?}", ui.mode);
        };
        assert_eq!(browser.highlight.item, 1);
        let rows = screen_rows(&state, &mut ui, 120);
        let current = rows
            .iter()
            .find(|row| row.contains("Contemplation"))
            .unwrap();
        assert!(current.contains("▌ ⏵ Contemplation"), "{current}");
        // Its artist, album and duration in columns on the same row
        let columns: Vec<_> = current.split_whitespace().skip(3).collect();
        assert_eq!(
            columns,
            ["Boucle", "Infinie", "Summit", "5:24", "│"],
            "{current}"
        );
        assert!(
            !rows
                .iter()
                .any(|row| row.contains("Not playing from the queue"))
        );
        assert!(rows.iter().any(|row| row.contains("Nothing playing")));
        assert!(rows.last().unwrap().contains("ENTER play"));
    }

    #[test]
    fn queue_marks_explicit_tracks() {
        let state = household_with_queue();
        let mut ui = UiState::default();
        ui.sync(&state);
        press(&[KeyCode::Char('l')], &state, &mut ui);
        ui.sync(&state);

        let rows = screen_rows(&state, &mut ui, 120);
        let row = |name| rows.iter().find(|row| row.contains(name)).unwrap();
        assert!(
            row("Ascension").contains("Ascension  E "),
            "{}",
            row("Ascension")
        );
        assert!(!row("Escape").contains(" E "), "{}", row("Escape"));
    }

    #[test]
    fn queue_says_when_it_is_not_what_is_playing() {
        let mut state = household_with_queue();
        // Playing from another version of the queue
        state.groups[0].queue_version = Some("5".to_owned());
        state.groups[0].queue_position = Some(2);
        let mut ui = UiState::default();
        ui.sync(&state);
        press(&[KeyCode::Char('l')], &state, &mut ui);
        ui.sync(&state);

        let rows = screen_rows(&state, &mut ui, 120);
        assert!(
            rows.iter()
                .any(|row| row.contains("Not playing from the queue"))
        );
        assert!(!rows.iter().any(|row| row.contains("⏵ Contemplation")));
        assert!(rows.iter().any(|row| row.contains("▌ Escape")));
        // Its tracks can't be skipped to.
        assert!(!rows.last().unwrap().contains("ENTER play"));

        // The queue of another group is not shown.
        press(
            &[
                KeyCode::Esc,
                KeyCode::Char('g'),
                KeyCode::Char('j'),
                KeyCode::Enter,
            ],
            &state,
            &mut ui,
        );
        press(&[KeyCode::Char('l')], &state, &mut ui);
        assert_eq!(watched_queue(&state, &ui), Some("kitchen".into()));
        let rows = screen_rows(&state, &mut ui, 120);
        assert!(rows.iter().any(|row| row.contains("Reading the queue")));
        assert!(!rows.iter().any(|row| row.contains("Escape")));
    }

    #[test]
    fn footer_shows_the_keys_to_use_in_what_is_shown() {
        let state = household();
        let mut ui = UiState::default();
        ui.sync(&state);

        let rows = screen_rows(&state, &mut ui, 100);
        let footer = rows.last().unwrap();
        assert!(footer.starts_with(" SPACE play/pause • "), "{footer}");
        assert!(footer.ends_with(" ? help • , config • q quit "), "{footer}");
        assert!(!rows[0].contains("help"), "{}", rows[0]);

        // The keys opening something are shown first, in a terminal of the default size.
        let rows = sized_screen_rows(&state, &mut ui, 80, 24);
        let footer = rows.last().unwrap();
        assert!(
            footer.starts_with(" SPACE play/pause • l queue • g groups • . playback "),
            "{footer}"
        );
        assert!(footer.ends_with(" ? help • , config • q quit "), "{footer}");

        // Too narrow for all the global keys: the ones that fit are shown whole.
        let rows = screen_rows(&state, &mut ui, 20);
        assert_eq!(rows.last().unwrap(), "  ? help • , config ");
        let rows = screen_rows(&state, &mut ui, 14);
        assert_eq!(rows.last().unwrap(), "       ? help ");

        // In a popup, the keys to close it rather than the global ones
        press(&[KeyCode::Char('v')], &state, &mut ui);
        let rows = screen_rows(&state, &mut ui, 100);
        let footer = rows.last().unwrap();
        assert_eq!(footer.trim_end(), " ↑↓ navigate • ←→ adjust • ESC close");

        // Popups don't cover the footer, even when they don't fit above it.
        press(&[KeyCode::Esc, KeyCode::Char('?')], &state, &mut ui);
        let rows = sized_screen_rows(&state, &mut ui, 80, 24);
        assert_eq!(rows.last().unwrap().trim_end(), " ESC close");
    }

    #[test]
    fn footer_gives_all_its_room_to_the_keys_to_use_when_there_are_no_global_ones() {
        let state = household();
        let mut ui = UiState::default();
        ui.sync(&state);
        press(&[KeyCode::Char('G')], &state, &mut ui);

        // Exactly as wide as the keys, plus a column on each side
        let rows = screen_rows(&state, &mut ui, 55);
        assert_eq!(
            rows.last().unwrap(),
            " ↑↓ navigate • SPACE toggle • ENTER apply • ESC cancel "
        );
    }

    #[test]
    fn group_name_is_shown_with_what_is_playing_rather_than_in_the_header() {
        let state = household();
        let mut ui = UiState::default();
        ui.sync(&state);

        let rows = screen_rows(&state, &mut ui, 80);
        assert!(!rows[0].contains("Bedroom"), "{}", rows[0]);
        assert!(
            rows.iter()
                .any(|row| row.trim() == "Playing on Bedroom + 1"),
            "{rows:#?}"
        );
    }

    #[test]
    fn browser_shows_what_is_playing_in_a_bar_below_it() {
        use crate::sonos::{Source, TrackInfo};

        let mut state = household_with_queue();
        let group = &mut state.groups[0];
        group.volume = Some(13);
        group.now_playing = Some(TrackInfo {
            title: Some("Nocturne".to_owned()),
            artist: Some("TesseracT".to_owned()),
            explicit: false,
            duration_secs: Some(350),
            image_url: None,
            is_station: false,
        });
        group.next_track = Some(TrackInfo {
            title: Some("Exile".to_owned()),
            ..group.now_playing.clone().unwrap()
        });
        group.source = Some(Source {
            service: Some("Spotify".to_owned()),
            kind: Some("album".to_owned()),
            name: Some("Altered State".to_owned()),
        });
        let mut ui = UiState::default();
        ui.sync(&state);
        press(&[KeyCode::Char('l')], &state, &mut ui);

        let rows = screen_rows(&state, &mut ui, 120);
        let [.., info, progress, controls, footer] = rows.as_slice() else {
            panic!("{rows:?}");
        };
        assert!(info.starts_with(" Nocturne · TesseracT "), "{info}");
        assert!(
            info.ends_with(" Spotify · Album · Altered State   Playing on Bedroom + 1 "),
            "{info}"
        );
        assert!(progress.contains("-5:50"), "{progress}");
        assert!(controls.starts_with(" shuffle   prev"), "{controls}");
        assert!(controls.ends_with(" 13 "), "{controls}");
        assert!(!rows.iter().any(|row| row.contains("Exile")));
        assert!(footer.starts_with(" TAB switch"), "{footer}");

        // With room for the name of the group, but not for the source
        let rows = screen_rows(&state, &mut ui, 70);
        let [.., info, _, _, _] = rows.as_slice() else {
            panic!("{rows:?}");
        };
        assert!(info.ends_with("   Playing on Bedroom + 1 "), "{info}");
        assert!(!info.contains("Spotify"), "{info}");

        // Without room for the name of the group, nor for the play modes beside the volume
        let rows = screen_rows(&state, &mut ui, 40);
        let [.., info, _, controls, _] = rows.as_slice() else {
            panic!("{rows:?}");
        };
        assert!(!info.contains("Bedroom"), "{info}");
        assert!(controls.starts_with(" prev"), "{controls}");
        assert!(controls.ends_with(" 13 "), "{controls}");

        // With room for the source, but not for a longer name of the group
        state.groups[0].name = "Upstairs Master Bedroom by the Window + 3".to_owned();
        let rows = screen_rows(&state, &mut ui, 70);
        let [.., info, _, _, _] = rows.as_slice() else {
            panic!("{rows:?}");
        };
        assert!(
            info.ends_with(" Spotify · Album · Altered State "),
            "{info}"
        );
        assert!(!info.contains("Upstairs"), "{info}");
    }

    #[test]
    fn favorites_mark_explicit_ones() {
        let mut state = household();
        state.favorites = [("Summit", false), ("City Burials", true)]
            .map(|(name, explicit)| {
                serde_json::from_value(serde_json::json!({
                    "_objectType": "favorite",
                    "id": name.to_lowercase(),
                    "name": name,
                    "resource": {
                        "_objectType": "favoriteResource",
                        "type": "ALBUM",
                        "explicit": explicit,
                    },
                }))
                .unwrap()
            })
            .into();
        let mut ui = UiState::default();
        ui.sync(&state);
        press(&[KeyCode::Char('f')], &state, &mut ui);

        let rows = screen_rows(&state, &mut ui, 120);
        let row = |name| rows.iter().find(|row| row.contains(name)).unwrap();
        assert!(
            row("City Burials").contains("City Burials  E "),
            "{}",
            row("City Burials")
        );
        assert!(!row("Summit").contains(" E "), "{}", row("Summit"));
    }

    #[test]
    fn browser_tabs_show_the_history_the_favorites_and_the_playlists() {
        let mut state = household();
        state.history = vec![
            serde_json::from_value(serde_json::json!({
                "_objectType": "contentResource",
                "type": "album",
                "id": {"_objectType": "universalMusicObjectId", "objectId": "spotify:album:1"},
                "name": "Summit",
            }))
            .unwrap(),
        ]
        .into();
        state.playlists = vec![
            serde_json::from_value(serde_json::json!({
                "_objectType": "playlistSummary",
                "id": "3",
                "name": "Road trip",
                "trackCount": 12,
            }))
            .unwrap(),
        ]
        .into();
        let mut ui = UiState::default();
        ui.sync(&state);

        press(&[KeyCode::Char('h')], &state, &mut ui);
        assert_eq!(watched_queue(&state, &ui), None);
        let rows = screen_rows(&state, &mut ui, 120);
        assert!(
            rows.iter()
                .any(|row| row.contains(" Queue · History · Favorites · Playlists "))
        );
        assert!(rows.iter().any(|row| row.contains("▌ Album · Summit")));

        press(&[KeyCode::Tab], &state, &mut ui);
        let rows = screen_rows(&state, &mut ui, 120);
        assert!(rows.iter().any(|row| row.contains("No favorites")));
        // Nothing to play
        assert!(!rows.last().unwrap().contains("ENTER play"));

        press(&[KeyCode::Tab], &state, &mut ui);
        let rows = screen_rows(&state, &mut ui, 120);
        assert!(rows.iter().any(|row| row.contains("▌ Road trip")));
        assert!(rows.last().unwrap().contains("ENTER play"));
        assert!(
            rows.iter()
                .any(|row| row.contains("12 tracks") && !row.contains("Playlist"))
        );
    }

    #[test]
    fn idle_groups_show_as_stopped() {
        use sinuous_client::playback::{PlayModes, PlaybackActions, PlaybackState};

        use crate::sonos::Playback;

        let mut state = household();
        state.groups[0].playback = Some(Playback::in_state(
            PlaybackState::Idle,
            PlaybackActions::default(),
        ));
        let mut ui = UiState::default();
        ui.sync(&state);
        let rows = screen_rows(&state, &mut ui, 120);
        assert!(rows.iter().any(|row| row.contains("⏹")));
        press(&[KeyCode::Char('g')], &state, &mut ui);
        let rows = screen_rows(&state, &mut ui, 120);
        assert!(rows.iter().any(|row| row.contains("⏹ Stopped")));

        state.groups[0].playback = Some(Playback::paused(
            PlayModes::default(),
            PlaybackActions::default(),
        ));
        let rows = screen_rows(&state, &mut ui, 120);
        assert!(rows.iter().any(|row| row.contains("⏸ Paused")));
        assert!(!rows.iter().any(|row| row.contains("⏹")));
    }

    #[test]
    fn buffering_groups_show_it() {
        use sinuous_client::playback::{PlaybackActions, PlaybackState};

        use crate::sonos::Playback;

        let mut state = household();
        state.groups[0].playback = Some(Playback::in_state(
            PlaybackState::Buffering,
            PlaybackActions::default(),
        ));
        let mut ui = UiState::default();
        ui.sync(&state);
        let rows = screen_rows(&state, &mut ui, 120);
        assert!(rows.iter().any(|row| row.contains("Buffering…")));
        assert!(!rows.iter().any(|row| row.contains("0:00")));
        press(&[KeyCode::Char('g')], &state, &mut ui);
        let rows = screen_rows(&state, &mut ui, 120);
        assert!(rows.iter().any(|row| row.contains("⏵ Buffering")));
    }

    #[test]
    fn stations_show_no_elapsed_time() {
        use sinuous_client::playback::{PlaybackActions, PlaybackState};

        use crate::sonos::{Playback, TrackInfo};

        let mut state = household();
        state.groups[0].playback = Some(Playback::in_state(
            PlaybackState::Playing,
            PlaybackActions::default(),
        ));
        state.groups[0].now_playing = Some(TrackInfo {
            title: Some("franceinfo".to_owned()),
            artist: None,
            explicit: false,
            duration_secs: None,
            image_url: None,
            is_station: true,
        });
        let mut ui = UiState::default();
        ui.sync(&state);
        let rows = screen_rows(&state, &mut ui, 120);
        assert!(rows.iter().any(|row| row.contains("franceinfo")));
        assert!(!rows.iter().any(|row| row.contains("0:00")));
    }

    #[test]
    fn next_track_is_not_shown_when_it_is_the_one_playing() {
        use crate::sonos::TrackInfo;

        let track = |title: &str| TrackInfo {
            title: Some(title.to_owned()),
            artist: Some("Candlemass".to_owned()),
            explicit: false,
            duration_secs: None,
            image_url: None,
            is_station: false,
        };
        let mut state = household();
        state.groups[0].now_playing = Some(track("Bewitched"));
        state.groups[0].next_track = Some(track("Solitude"));
        let mut ui = UiState::default();
        ui.sync(&state);
        let rows = screen_rows(&state, &mut ui, 120);
        assert!(
            rows.iter()
                .any(|row| row.contains("Next: Solitude by Candlemass"))
        );

        // As radio stations report it
        state.groups[0].next_track = Some(track("Bewitched"));
        let rows = screen_rows(&state, &mut ui, 120);
        assert!(rows.iter().any(|row| row.contains("Bewitched")));
        assert!(!rows.iter().any(|row| row.contains("Next:")));
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
