use std::{
    collections::HashSet,
    time::{Duration, Instant},
};

use clap::crate_version;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    Frame,
    buffer::Buffer,
    layout::{Constraint, Layout, Margin, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, BorderType::Rounded, Clear, Gauge, HighlightSpacing, List, ListState, Padding,
        Paragraph, Widget, Wrap,
    },
};
use ratatui_image::{FilterType, Resize, StatefulImage};
use sinuous_client::{
    GroupId, PlayerId,
    playback::{PlayModes, PlayModesUpdate, PlaybackActions},
};
use url::Url;

use crate::artwork::Artwork;
use crate::config::{UiConfig, UiSetting};
use crate::sonos::{
    Command, ErrorMessage, GroupAction, GroupState, HouseholdState, PlayerAction, PlayerInfo,
    Source, TrackInfo,
};
use crate::symbols::Symbols;
use crate::theme::{self, THEMES, Theme};

/// How long errors are displayed for.
const ERROR_DISPLAY_TIME: Duration = Duration::from_secs(5);
/// How much the volume changes with each key press.
const VOLUME_STEP: i8 = 1;
/// How wide the panel on the left (the groups, or the speakers of one) is: a third of the screen,
/// within these bounds, unless the keys to use need more room.
const GROUP_PANEL_MIN_WIDTH: u16 = 34;
const GROUP_PANEL_MAX_WIDTH: u16 = 48;
/// How many lines each group takes in the group panel.
const GROUP_HEIGHT: u16 = 3;
/// The keys to use in the group panel, and in the group editor.
const GROUP_PANEL_HELP: &str = " ↑↓ • ENTER select • ESC close ";
const GROUP_EDITOR_HELP: &str = " ↑↓ • SPACE toggle • ENTER apply • ESC cancel ";
/// How narrow the volume slider can be, with its icon and value.
const MIN_VOLUME_WIDTH: u16 = 44;
/// How wide the icon and the value on each side of the volume slider are: as wide as each other,
/// so that the slider is centred.
const VOLUME_SIDE_WIDTH: u16 = 3;
/// How wide the times on each side of the progress bar can be, e.g. `59:59`.
const TIME_WIDTH: u16 = 5;

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
    /// The keyboard shortcuts.
    Help,
    /// The configuration dialog, and the theme picker on top of it, if open.
    Config {
        /// The setting highlighted: one of [`UiSetting::ALL`], or the theme after them.
        highlighted: usize,
        /// The theme highlighted in the theme picker, as an index in [`THEMES`].
        theme_picker: Option<usize>,
    },
}

/// What a key press asks for, besides changing what is shown.
#[derive(Debug)]
pub enum Request {
    /// Send a command to the speakers.
    Speakers(Command),
    /// Turn a setting on or off, and save it.
    Toggle(UiSetting),
    /// Use the theme with this name, and save it.
    SetTheme(String),
}

/// The group the user is looking at.
#[derive(Debug)]
struct Selection {
    group: GroupId,
    /// Its coordinator when it was last seen, to follow it if the group gets a new ID, e.g. when
    /// its members change.
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
            Mode::VolumeMixer(highlighted) => match mixer_rows {
                Some(rows) => clamp(highlighted, rows),
                None => self.mode = Mode::NowPlaying,
            },
            Mode::NowPlaying | Mode::Help | Mode::Config { .. } => {}
        }
    }

    /// Whether a popup is open in the middle of the screen. The group panel and the group editor
    /// are beside what is playing, not on top of it.
    fn popup_open(&self) -> bool {
        !matches!(
            self.mode,
            Mode::NowPlaying | Mode::GroupPicker(_) | Mode::GroupEditor(_)
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

/// The row to highlight in a list of `len` rows when `key` moves the highlight from the row at
/// `highlighted`, if it does. The highlight stays on the first and last rows.
fn navigate(key: KeyCode, highlighted: usize, len: usize) -> Option<usize> {
    match key {
        KeyCode::Up | KeyCode::Char('k') => Some(highlighted.saturating_sub(1)),
        KeyCode::Down | KeyCode::Char('j') => Some((highlighted + 1).min(len.saturating_sub(1))),
        _ => None,
    }
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
    let advancing = ui
        .current_group(state)
        .and_then(|(_, group)| group.playback)
        .is_some_and(|playback| playback.is_advancing());
    showing_error || advancing
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
        // Images can be drawn on top of the text, so hide the album art behind popups.
        render_now_playing(
            group,
            symbols,
            theme,
            frame,
            area,
            artwork,
            !ui.popup_open(),
        );
    }

    // Popups, on top of everything else
    match ui.mode {
        Mode::FavoritesPicker(highlighted) => {
            render_favorites_picker(state, theme, highlighted, frame);
        }
        Mode::VolumeMixer(highlighted) => {
            render_volume_mixer(state, group, theme, highlighted, frame);
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
        Mode::NowPlaying | Mode::GroupPicker(_) | Mode::GroupEditor(_) => {}
    }
}

/// Update `ui` according to the key pressed, and return what else it asks for, if anything.
pub fn handle_input(
    input: &KeyEvent,
    state: &HouseholdState,
    settings: &UiConfig,
    ui: &mut UiState,
) -> Option<Request> {
    let command = match ui.mode {
        Mode::NowPlaying => handle_now_playing_input(input, state, ui),
        Mode::GroupPicker(highlight) => {
            handle_group_picker_input(input, state, ui, highlight);
            None
        }
        Mode::GroupEditor(_) => handle_group_editor_input(input, state, ui),
        Mode::FavoritesPicker(highlighted) => {
            handle_favorites_picker_input(input, state, ui, highlighted)
        }
        Mode::VolumeMixer(highlighted) => handle_volume_mixer_input(input, state, ui, highlighted),
        Mode::Help => {
            if matches!(input.code, KeyCode::Esc | KeyCode::Char('?' | 'h')) {
                ui.mode = Mode::NowPlaying;
            }
            None
        }
        Mode::Config {
            highlighted,
            theme_picker: None,
        } => return handle_config_dialog_input(input, settings, ui, highlighted),
        Mode::Config {
            highlighted,
            theme_picker: Some(theme),
        } => return handle_theme_picker_input(input, ui, highlighted, theme),
    };
    command.map(Request::Speakers)
}

/// Update `ui` according to the key pressed while nothing is open, and return the command to send
/// to the speakers, if any.
fn handle_now_playing_input(
    input: &KeyEvent,
    state: &HouseholdState,
    ui: &mut UiState,
) -> Option<Command> {
    ui.mode = match input.code {
        KeyCode::Char('f') => Mode::FavoritesPicker(0),

        // Group switching
        KeyCode::Char('g') => Mode::GroupPicker(PanelHighlight::new(ui.current_group(state)?.0)),
        KeyCode::Char('G') => {
            Mode::GroupEditor(GroupEditor::new(ui.current_group(state)?.1, state))
        }

        KeyCode::Char('v') => Mode::VolumeMixer(0),

        KeyCode::Char('?' | 'h') => Mode::Help,
        KeyCode::Char(',') => Mode::Config {
            highlighted: 0,
            theme_picker: None,
        },

        _ => return group_command(input, state, ui),
    };
    None
}

/// Update `ui` according to the key pressed while the configuration dialog is open, with the
/// setting at `highlighted` highlighted, and return the setting to toggle, if any.
fn handle_config_dialog_input(
    input: &KeyEvent,
    settings: &UiConfig,
    ui: &mut UiState,
    highlighted: usize,
) -> Option<Request> {
    // The settings that can be toggled, then the theme
    let rows = UiSetting::ALL.len() + 1;
    if let Some(highlighted) = navigate(input.code, highlighted, rows) {
        ui.mode = Mode::Config {
            highlighted,
            theme_picker: None,
        };
        return None;
    }
    match input.code {
        KeyCode::Char(' ') | KeyCode::Enter => match UiSetting::ALL.get(highlighted) {
            Some(setting) => return Some(Request::Toggle(*setting)),
            None => {
                let current = THEMES
                    .iter()
                    .position(|entry| entry.name == settings.theme)
                    .unwrap_or(0);
                ui.mode = Mode::Config {
                    highlighted,
                    theme_picker: Some(current),
                };
            }
        },
        KeyCode::Esc | KeyCode::Char(',') => ui.mode = Mode::NowPlaying,
        _ => {}
    }
    None
}

/// Update `ui` according to the key pressed while the theme picker is open, over the
/// configuration dialog with the setting at `setting` highlighted, with the theme at `highlighted`
/// highlighted, and return the theme chosen, if any.
fn handle_theme_picker_input(
    input: &KeyEvent,
    ui: &mut UiState,
    setting: usize,
    highlighted: usize,
) -> Option<Request> {
    let config = |theme_picker| Mode::Config {
        highlighted: setting,
        theme_picker,
    };
    if let Some(highlighted) = navigate(input.code, highlighted, THEMES.len()) {
        ui.mode = config(Some(highlighted));
        return None;
    }
    match input.code {
        KeyCode::Enter => {
            ui.mode = config(None);
            let entry = THEMES.get(highlighted)?;
            return Some(Request::SetTheme(entry.name.clone()));
        }
        KeyCode::Esc => ui.mode = config(None),
        _ => {}
    }
    None
}

/// Update `ui` according to the key pressed while the group picker is open, with the group at
/// `highlight` highlighted.
fn handle_group_picker_input(
    input: &KeyEvent,
    state: &HouseholdState,
    ui: &mut UiState,
    highlight: PanelHighlight,
) {
    if let Some(item) = navigate(input.code, highlight.item, state.groups.len()) {
        ui.mode = Mode::GroupPicker(PanelHighlight { item, ..highlight });
        return;
    }
    match input.code {
        KeyCode::Enter => {
            if let Some(group) = state.groups.get(highlight.item) {
                ui.selected_group = Some(Selection::of(group));
            }
            ui.mode = Mode::NowPlaying;
        }
        KeyCode::Esc | KeyCode::Char('g') => ui.mode = Mode::NowPlaying,
        _ => {}
    }
}

/// Update `ui` according to the key pressed while the group editor is open, and return the
/// command changing the members of the group, if any.
fn handle_group_editor_input(
    input: &KeyEvent,
    state: &HouseholdState,
    ui: &mut UiState,
) -> Option<Command> {
    let Mode::GroupEditor(editor) = &mut ui.mode else {
        return None;
    };
    let group = state.group(&editor.group)?;
    if let Some(item) = navigate(input.code, editor.highlight.item, state.players.len()) {
        editor.highlight.item = item;
        return None;
    }
    match input.code {
        KeyCode::Char(' ') => {
            let player = state.players.get(editor.highlight.item)?;
            // The coordinator stays, so that the group keeps playing.
            if player.id != group.coordinator_id && !editor.members.remove(&player.id) {
                editor.members.insert(player.id.clone());
            }
        }
        KeyCode::Enter => {
            let command = set_members(state, group, &editor.members);
            ui.mode = Mode::NowPlaying;
            return command;
        }
        KeyCode::Esc | KeyCode::Char('G') => ui.mode = Mode::NowPlaying,
        _ => {}
    }
    None
}

/// The command making `members` the speakers of `group`, unless they already are.
fn set_members(
    state: &HouseholdState,
    group: &GroupState,
    members: &HashSet<PlayerId>,
) -> Option<Command> {
    if group.player_ids.iter().cloned().collect::<HashSet<_>>() == *members {
        return None;
    }
    let others = state
        .players
        .iter()
        .filter(|p| p.id != group.coordinator_id && members.contains(&p.id))
        .map(|p| p.id.clone());
    let players = std::iter::once(group.coordinator_id.clone())
        .chain(others)
        .collect();
    Some(Command::Group {
        group: group.id.clone(),
        action: GroupAction::SetMembers(players),
    })
}

/// Update `ui` according to the key pressed while the volume mixer is open, with the row at
/// `highlighted` highlighted, and return the command changing the volume, if any.
fn handle_volume_mixer_input(
    input: &KeyEvent,
    state: &HouseholdState,
    ui: &mut UiState,
    highlighted: usize,
) -> Option<Command> {
    let (_, group) = ui.current_group(state)?;
    // The group, then its members
    let rows = state.members(group).count() + 1;
    if let Some(highlighted) = navigate(input.code, highlighted, rows) {
        ui.mode = Mode::VolumeMixer(highlighted);
        return None;
    }
    let delta = match input.code {
        KeyCode::Esc | KeyCode::Char('v') => {
            ui.mode = Mode::NowPlaying;
            return None;
        }
        KeyCode::Left | KeyCode::Char('[') => -VOLUME_STEP,
        KeyCode::Right | KeyCode::Char(']') => VOLUME_STEP,
        _ => return None,
    };
    match highlighted.checked_sub(1) {
        None => Some(Command::Group {
            group: group.id.clone(),
            action: GroupAction::AdjustVolume(delta),
        }),
        Some(member) => Some(Command::Player {
            player: state.members(group).nth(member)?.id.clone(),
            action: PlayerAction::AdjustVolume(delta),
        }),
    }
}

/// Update `ui` according to the key pressed while the favorites picker is open, with the favorite
/// at index `highlighted` highlighted, and return the command playing the favorite, if any.
fn handle_favorites_picker_input(
    input: &KeyEvent,
    state: &HouseholdState,
    ui: &mut UiState,
    highlighted: usize,
) -> Option<Command> {
    if let Some(highlighted) = navigate(input.code, highlighted, state.favorites.len()) {
        ui.mode = Mode::FavoritesPicker(highlighted);
        return None;
    }
    match input.code {
        KeyCode::Enter => {
            ui.mode = Mode::NowPlaying;
            let favorite = state.favorites.get(highlighted)?;
            let (_, group) = ui.current_group(state)?;
            return Some(Command::Group {
                group: group.id.clone(),
                action: GroupAction::LoadFavorite(favorite.id.clone()),
            });
        }
        KeyCode::Esc | KeyCode::Char('f') => ui.mode = Mode::NowPlaying,
        _ => {}
    }
    None
}

/// The command for the selected group corresponding to the key pressed, if any.
fn group_command(input: &KeyEvent, state: &HouseholdState, ui: &UiState) -> Option<Command> {
    let (_, group) = ui.current_group(state)?;
    // The action, and what the current source needs to support for it, if anything.
    let (action, required) = match input.code {
        KeyCode::Char(' ') if group.playback.is_some_and(|p| p.is_playing) => {
            (GroupAction::Pause, Some(Action::PlayPause))
        }
        KeyCode::Char(' ') => (GroupAction::Play, Some(Action::PlayPause)),
        KeyCode::Char('n') => (GroupAction::Next, Some(Action::Next)),
        KeyCode::Char('p') => (GroupAction::Previous, Some(Action::Previous)),
        KeyCode::Char('[') => (GroupAction::AdjustVolume(-VOLUME_STEP), None),
        KeyCode::Char(']') => (GroupAction::AdjustVolume(VOLUME_STEP), None),

        // Play modes
        KeyCode::Char('s') => {
            let update = PlayModesUpdate {
                shuffle: Some(!group.playback?.play_modes.shuffle),
                ..Default::default()
            };
            (GroupAction::SetPlayModes(update), Some(Action::Shuffle))
        }
        KeyCode::Char('r') => {
            let playback = group.playback?;
            let update = next_repeat(playback.play_modes, playback.actions);
            (GroupAction::SetPlayModes(update), Some(Action::Repeat))
        }
        KeyCode::Char('c') => {
            let update = PlayModesUpdate {
                crossfade: Some(!group.playback?.play_modes.crossfade),
                ..Default::default()
            };
            (GroupAction::SetPlayModes(update), Some(Action::Crossfade))
        }

        _ => return None,
    };
    if required.is_some_and(|required| !supports(group, required)) {
        return None;
    }
    Some(Command::Group {
        group: group.id.clone(),
        action,
    })
}

/// Something the user can do that the current source may not support.
#[derive(Debug, Clone, Copy)]
enum Action {
    PlayPause,
    Next,
    Previous,
    Shuffle,
    Repeat,
    Crossfade,
}

/// Whether the source `group` is playing from supports `action`. Everything is supported until
/// the group has said otherwise.
fn supports(group: &GroupState, action: Action) -> bool {
    let Some(playback) = group.playback else {
        return true;
    };
    let actions = playback.actions;
    match action {
        Action::PlayPause if playback.is_playing => actions.can_pause,
        Action::PlayPause => actions.can_play,
        Action::Next => actions.can_skip,
        Action::Previous => actions.can_skip_back || actions.can_skip_to_previous,
        Action::Shuffle => actions.can_shuffle,
        Action::Repeat => actions.can_repeat || actions.can_repeat_one,
        Action::Crossfade => actions.can_crossfade,
    }
}

/// Cycle through no repeat, repeat all and repeat one, skipping the ones `actions` doesn't
/// support.
fn next_repeat(modes: PlayModes, actions: PlaybackActions) -> PlayModesUpdate {
    let (repeat, repeat_one) = match (modes.repeat, modes.repeat_one) {
        (false, false) if actions.can_repeat => (true, false),
        (false, false) | (true, false) if actions.can_repeat_one => (false, true),
        _ => (false, false),
    };
    PlayModesUpdate {
        repeat: Some(repeat),
        repeat_one: Some(repeat_one),
        ..Default::default()
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

/// The track playing on `group`, how far into it playback is, its volume, and the next track.
fn render_now_playing(
    group: &GroupState,
    symbols: &Symbols,
    theme: &Theme,
    frame: &mut Frame,
    area: Rect,
    artwork: &mut Artwork,
    draw_art: bool,
) {
    // Everything but the album art: source, track information, progress, controls, volume and
    // next track, with the gaps between them.
    const DETAILS_HEIGHT: u16 = 13;
    const MAX_ART_HEIGHT: u16 = 16;
    const MIN_ART_HEIGHT: u16 = 4;

    // Make room for the art even before it is loaded, so that the rest doesn't move when it is.
    // Keep a gap below it, and at least one empty line above and below everything.
    let art_height = if artwork.is_wanted() {
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
        Constraint::Length(3),
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

    if draw_art
        && art_height > 0
        && let Some(image) = artwork.image()
    {
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
    }

    if let Some(source) = &group.source {
        frame.render_widget(
            Paragraph::new(source_line(source, theme)).centered(),
            source_area,
        );
    }

    // Track information
    let info = match &group.now_playing {
        Some(track) => vec![
            Line::styled(
                track.title.as_deref().unwrap_or("Unknown title"),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Line::styled(
                track.artist.as_deref().unwrap_or("Unknown artist"),
                Style::default().fg(theme.artist),
            ),
            album_line(track, theme),
        ],
        None => vec![
            Line::default(),
            Line::styled("Nothing playing", Style::default().fg(theme.muted)),
        ],
    };
    frame.render_widget(Paragraph::new(info).centered(), info_area);

    // The volume slider lines up with the controls above it, unless they are too narrow (with
    // icons), and the progress bar with the slider.
    let controls = controls_line(group, symbols, theme);
    let volume_width = volume_width(&controls);
    frame.render_widget(Paragraph::new(controls).centered(), controls_area);
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

    // Next track
    if let Some(next) = &group.next_track {
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

/// How wide what `group` is playing needs to be for the controls, the volume and the progress bar
/// to fit.
fn now_playing_width(group: &GroupState, symbols: &Symbols, theme: &Theme) -> u16 {
    let volume = volume_width(&controls_line(group, symbols, theme));
    // The progress bar is as wide as the volume slider, with the times on each side of it.
    let bar = volume.saturating_sub(2 * (VOLUME_SIDE_WIDTH + 1));
    volume.max(bar + 2 * (TIME_WIDTH + 1))
}

/// Where the music comes from, e.g. `Spotify · Album · Altered State`.
fn source_line<'a>(source: &'a Source, theme: &Theme) -> Line<'a> {
    let kind = source.kind.as_deref().and_then(|kind| match kind {
        "album" => Some("Album"),
        "artist" => Some("Artist"),
        "playlist" => Some("Playlist"),
        "station" | "program" => Some("Radio"),
        "show" | "podcast" => Some("Podcast"),
        kind if kind.starts_with("linein") => Some("Line-in"),
        _ => None,
    });
    let parts = [source.service.as_deref(), kind, source.name.as_deref()];
    let mut spans = Vec::new();
    for (i, part) in parts.into_iter().flatten().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" · ", Style::default().fg(theme.muted)));
        }
        let style = if i == 0 {
            Style::default().fg(theme.name)
        } else {
            Style::default()
        };
        spans.push(Span::styled(part, style));
    }
    Line::from(spans)
}

/// The album of `track`, with a badge if it is explicit.
fn album_line<'a>(track: &'a TrackInfo, theme: &Theme) -> Line<'a> {
    let mut spans = vec![Span::styled(
        track.album.as_deref().unwrap_or("Unknown album"),
        Style::default()
            .fg(theme.muted)
            .add_modifier(Modifier::ITALIC),
    )];
    if track.album_explicit {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(" E ", theme.badge));
    }
    Line::from(spans)
}

/// The playback controls and play modes, highlighting the playback status and the play modes that
/// are on.
fn controls_line(group: &GroupState, symbols: &Symbols, theme: &Theme) -> Line<'static> {
    let is_playing = group.playback.is_some_and(|p| p.is_playing);
    let status = if is_playing {
        symbols.playing
    } else {
        symbols.paused
    };
    let controls = [
        (symbols.previous, false, Action::Previous),
        (status, is_playing, Action::PlayPause),
        (symbols.next, false, Action::Next),
    ];

    let modes = group.playback.map(|p| p.play_modes).unwrap_or_default();
    let repeat = if modes.repeat_one {
        symbols.repeat_one
    } else {
        symbols.repeat
    };
    let modes = [
        (symbols.shuffle, modes.shuffle, Action::Shuffle),
        (repeat, modes.repeat || modes.repeat_one, Action::Repeat),
        (symbols.crossfade, modes.crossfade, Action::Crossfade),
    ];

    let mut spans = Vec::new();
    for (i, (label, on, action)) in controls.into_iter().chain(modes).enumerate() {
        if i == controls.len() {
            spans.push(Span::styled("   │   ", Style::default().fg(theme.muted)));
        } else if i > 0 {
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
/// total times, in the rest of `area`.
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
    let duration_x = bar_area.right() + 1;
    let duration_area = Rect {
        x: duration_x,
        width: area.right().saturating_sub(duration_x),
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
    frame.render_widget(Paragraph::new(format_duration(duration)), duration_area);
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
fn volume_slider(volume: Option<u8>, theme: &Theme) -> VolumeSlider<'_> {
    VolumeSlider { volume, theme }
}

/// A horizontal slider showing a volume, with a handle if it is known.
struct VolumeSlider<'a> {
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
fn volume_text(volume: Option<u8>) -> String {
    volume.map_or_else(|| " --".to_owned(), |v| format!("{v:>3}"))
}

/// A popup listing the favorites, with the one at `highlighted` highlighted.
fn render_favorites_picker(
    state: &HouseholdState,
    theme: &Theme,
    highlighted: usize,
    frame: &mut Frame,
) {
    let lines: Vec<_> = state
        .favorites
        .iter()
        .map(|fav| {
            let mut spans = vec![Span::raw(fav.name.as_str())];
            if let Some(description) = &fav.description {
                spans.push(Span::styled(
                    format!("  {description}"),
                    Style::default().fg(theme.muted),
                ));
            }
            Line::from(spans)
        })
        .collect();
    let (lines, highlighted) = if lines.is_empty() {
        let none = Line::styled("No favorites", Style::default().fg(theme.muted));
        (vec![none], None)
    } else {
        (lines, Some(highlighted))
    };

    render_popup(
        frame,
        theme,
        " Favorites ",
        lines,
        highlighted,
        " ↑↓ Navigate • ENTER play • ESC close ",
    );
}

/// A panel in `area` listing the groups and what they are playing, with the selected group (at
/// `selected`) in colour and the one at `highlight` highlighted.
fn render_group_panel(
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
        status_line.push(Span::styled(service, Style::default().fg(theme.secondary)));
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
fn render_group_editor(
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
fn speaker_name<'a>(player: &'a PlayerInfo, style: Style, theme: &Theme) -> Vec<Span<'a>> {
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

/// A popup with the volume of `group` and of each of its members, with the row at `highlighted`
/// highlighted.
fn render_volume_mixer(
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

/// A popup listing all the keyboard shortcuts.
fn render_help(theme: &Theme, frame: &mut Frame) {
    // The keys to use in the other popups are shown at their bottom.
    const SECTIONS: [(&str, &[(&str, &str)]); 2] = [
        (
            "Playback",
            &[
                ("SPACE", "Play / pause"),
                ("p / n", "Previous / next track"),
                ("s", "Toggle shuffle"),
                ("r", "Cycle repeat: all, one, off"),
                ("c", "Toggle crossfade"),
                ("[ / ]", "Volume down / up"),
            ],
        ),
        (
            "General",
            &[
                ("v", "Volume of the group and of each speaker"),
                ("f", "Play a favorite"),
                ("g", "Switch to another group"),
                ("G", "Add or remove speakers from the group"),
                (",", "Configuration"),
                ("? / h", "This help"),
                ("q", "Quit"),
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
fn render_config_dialog(
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
fn render_theme_picker(current: &str, theme: &Theme, highlighted: usize, frame: &mut Frame) {
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

fn is_shown(error: &ErrorMessage) -> bool {
    error.at.elapsed() < ERROR_DISPLAY_TIME
}

fn format_duration(secs: u32) -> String {
    let minutes = secs / 60;
    let seconds = secs % 60;

    format!("{minutes}:{seconds:02}")
}

#[cfg(test)]
mod tests {
    use crossterm::event::KeyModifiers;

    use super::*;
    use crate::sonos::PlayerAction;

    fn household() -> HouseholdState {
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

    fn press(keys: &[KeyCode], state: &HouseholdState, ui: &mut UiState) -> Option<Request> {
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
    fn group_editor_sets_members_with_coordinator_first() {
        let state = household();
        let mut ui = UiState::default();
        ui.sync(&state);

        let command = press(
            &[
                KeyCode::Char('G'),
                // The coordinator can't be removed
                KeyCode::Char(' '),
                // Add the kitchen
                KeyCode::Char('j'),
                KeyCode::Char(' '),
                // Remove the office
                KeyCode::Char('j'),
                KeyCode::Char(' '),
                KeyCode::Enter,
            ],
            &state,
            &mut ui,
        );

        let Some(Request::Speakers(Command::Group {
            group,
            action: GroupAction::SetMembers(players),
        })) = command
        else {
            panic!("Unexpected command: {command:?}");
        };
        assert_eq!(group, GroupId::from("bedroom"));
        assert_eq!(players, [PlayerId::from("b"), PlayerId::from("k")]);
        assert!(matches!(ui.mode, Mode::NowPlaying));
    }

    #[test]
    fn selection_follows_the_coordinator_when_the_group_gets_a_new_id() {
        let mut state = household();
        let mut ui = UiState::default();
        ui.sync(&state);
        press(
            &[KeyCode::Char('g'), KeyCode::Char('j'), KeyCode::Enter],
            &state,
            &mut ui,
        );
        assert_eq!(ui.current_group(&state).unwrap().1.name, "Kitchen");

        // The kitchen was grouped with the office, under a new ID.
        state.groups = vec![
            GroupState::new(
                "bedroom".into(),
                "Bedroom".to_owned(),
                "b".into(),
                vec!["b".into()],
            ),
            GroupState::new(
                "kitchen:2".into(),
                "Kitchen + Office".to_owned(),
                "k".into(),
                vec!["k".into(), "o".into()],
            ),
        ];
        ui.sync(&state);
        assert_eq!(ui.current_group(&state).unwrap().1.name, "Kitchen + Office");

        // The kitchen joined the bedroom.
        state.groups = vec![GroupState::new(
            "bedroom:2".into(),
            "Bedroom + Kitchen".to_owned(),
            "b".into(),
            vec!["b".into(), "k".into()],
        )];
        ui.sync(&state);
        assert_eq!(
            ui.current_group(&state).unwrap().1.name,
            "Bedroom + Kitchen"
        );
    }

    #[test]
    fn group_editor_does_nothing_unless_members_change() {
        let state = household();
        let mut ui = UiState::default();
        ui.sync(&state);

        let keys = [
            KeyCode::Char('G'),
            KeyCode::Char('j'),
            KeyCode::Char(' '),
            KeyCode::Char(' '),
            KeyCode::Enter,
        ];
        assert!(press(&keys, &state, &mut ui).is_none());
        assert!(matches!(ui.mode, Mode::NowPlaying));

        let keys = [KeyCode::Char('G'), KeyCode::Char('j'), KeyCode::Char(' ')];
        assert!(press(&keys, &state, &mut ui).is_none());
        assert!(press(&[KeyCode::Esc], &state, &mut ui).is_none());
        assert!(matches!(ui.mode, Mode::NowPlaying));
    }

    #[test]
    fn volume_mixer_adjusts_group_then_members() {
        let state = household();
        let mut ui = UiState::default();
        ui.sync(&state);

        let command = press(&[KeyCode::Char('v'), KeyCode::Left], &state, &mut ui);
        let Some(Request::Speakers(Command::Group {
            group,
            action: GroupAction::AdjustVolume(delta),
        })) = command
        else {
            panic!("Unexpected command: {command:?}");
        };
        assert_eq!(group, GroupId::from("bedroom"));
        assert_eq!(delta, -VOLUME_STEP);

        // Rows past the last member stay on it: the office
        let keys = [
            KeyCode::Char('j'),
            KeyCode::Char('j'),
            KeyCode::Char('j'),
            KeyCode::Char(']'),
        ];
        let command = press(&keys, &state, &mut ui);
        let Some(Request::Speakers(Command::Player {
            player,
            action: PlayerAction::AdjustVolume(delta),
        })) = command
        else {
            panic!("Unexpected command: {command:?}");
        };
        assert_eq!(player, PlayerId::from("o"));
        assert_eq!(delta, VOLUME_STEP);

        assert!(press(&[KeyCode::Esc], &state, &mut ui).is_none());
        assert!(matches!(ui.mode, Mode::NowPlaying));
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
            let (mut artwork, _) = Artwork::new(ratatui_image::picker::Picker::halfblocks());
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
    fn group_editor_is_a_panel_beside_what_is_playing() {
        use ratatui::{Terminal, backend::TestBackend};

        let state = household();
        let mut ui = UiState::default();
        ui.sync(&state);
        press(&[KeyCode::Char('G')], &state, &mut ui);

        let settings = UiConfig::default();
        let mut terminal = Terminal::new(TestBackend::new(120, 20)).unwrap();
        let (mut artwork, _) = Artwork::new(ratatui_image::picker::Picker::halfblocks());
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
        let (mut artwork, _) = Artwork::new(ratatui_image::picker::Picker::halfblocks());
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

    fn repeat_cycle(actions: PlaybackActions) -> Vec<(bool, bool)> {
        let mut modes = PlayModes::default();
        let mut seen = Vec::new();
        for _ in 0..3 {
            let update = next_repeat(modes, actions);
            modes.repeat = update.repeat.unwrap();
            modes.repeat_one = update.repeat_one.unwrap();
            seen.push((modes.repeat, modes.repeat_one));
        }
        seen
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

    #[test]
    fn repeat_cycles_through_off_all_and_one() {
        let actions = PlaybackActions {
            can_repeat: true,
            can_repeat_one: true,
            ..Default::default()
        };
        assert_eq!(
            repeat_cycle(actions),
            [(true, false), (false, true), (false, false)]
        );
    }

    #[test]
    fn repeat_cycle_skips_unsupported_modes() {
        let actions = PlaybackActions {
            can_repeat: true,
            ..Default::default()
        };
        assert_eq!(
            repeat_cycle(actions),
            [(true, false), (false, false), (true, false)]
        );
        let actions = PlaybackActions {
            can_repeat_one: true,
            ..Default::default()
        };
        assert_eq!(
            repeat_cycle(actions),
            [(false, true), (false, false), (false, true)]
        );
    }

    #[test]
    fn config_dialog_toggles_highlighted_setting() {
        let state = household();
        let mut ui = UiState::default();
        ui.sync(&state);

        let request = press(
            &[KeyCode::Char(','), KeyCode::Char('j'), KeyCode::Char(' ')],
            &state,
            &mut ui,
        );
        assert!(matches!(
            request,
            Some(Request::Toggle(UiSetting::NerdFont))
        ));
        // Keys don't reach the main screen while it is open.
        assert!(press(&[KeyCode::Char('c')], &state, &mut ui).is_none());
        assert!(press(&[KeyCode::Esc], &state, &mut ui).is_none());
        assert!(matches!(ui.mode, Mode::NowPlaying));
    }

    #[test]
    fn help_closes_with_the_keys_opening_it() {
        let state = household();
        let mut ui = UiState::default();
        ui.sync(&state);

        assert!(press(&[KeyCode::Char('?'), KeyCode::Char(' ')], &state, &mut ui).is_none());
        assert!(matches!(ui.mode, Mode::Help));
        press(&[KeyCode::Char('h')], &state, &mut ui);
        assert!(matches!(ui.mode, Mode::NowPlaying));
    }

    #[test]
    fn theme_picker_previews_highlighted_theme() {
        let state = household();
        let mut ui = UiState::default();
        ui.sync(&state);

        // The theme comes after the settings that can be toggled.
        let mut keys = vec![KeyCode::Char(',')];
        keys.extend(UiSetting::ALL.map(|_| KeyCode::Char('j')));
        keys.extend([KeyCode::Enter, KeyCode::Char('j')]);
        assert!(press(&keys, &state, &mut ui).is_none());
        assert_eq!(ui.previewed_theme(), Some(THEMES[1].name.as_str()));

        let Some(Request::SetTheme(name)) = press(&[KeyCode::Enter], &state, &mut ui) else {
            panic!("No theme chosen");
        };
        assert_eq!(name, THEMES[1].name);
        assert!(ui.previewed_theme().is_none());
        assert!(matches!(ui.mode, Mode::Config { .. }));

        // Cancelling stops previewing.
        press(
            &[KeyCode::Enter, KeyCode::Char('j'), KeyCode::Esc],
            &state,
            &mut ui,
        );
        assert!(ui.previewed_theme().is_none());
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
