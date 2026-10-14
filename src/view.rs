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
    Command, ConnectionStatus, ErrorMessage, GroupAction, GroupState, HouseholdState, PlayerAction,
    Source, TrackInfo,
};
use crate::symbols::Symbols;
use crate::theme::{self, THEMES, Theme};

/// How long errors are displayed for.
const ERROR_DISPLAY_TIME: Duration = Duration::from_secs(5);
/// How much the volume changes with each key press.
const VOLUME_STEP: i8 = 2;

/// What the user is looking at, independently of the state of the speakers.
#[derive(Debug, Default)]
pub struct UiState {
    selected_group: Option<GroupId>,
    /// The favorite highlighted in the favorites picker, if it is open.
    favorites_picker: Option<usize>,
    /// The group highlighted in the group picker, if it is open.
    group_picker: Option<usize>,
    group_editor: Option<GroupEditor>,
    /// The row highlighted in the volume mixer, if it is open: 0 for the selected group, then its
    /// members.
    volume_mixer: Option<usize>,
    /// Whether the keyboard shortcuts are shown.
    help: bool,
    /// The setting highlighted in the configuration dialog, if it is open: one of
    /// [`UiSetting::ALL`], or the theme after them.
    config_dialog: Option<usize>,
    /// The theme highlighted in the theme picker, if it is open, as an index in [`THEMES`].
    theme_picker: Option<usize>,
    /// The last error that wasn't about the speakers.
    error: Option<ErrorMessage>,
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

/// Speakers being added to or removed from a group.
#[derive(Debug)]
struct GroupEditor {
    group: GroupId,
    /// The index of the highlighted speaker in [`HouseholdState::players`].
    highlighted: usize,
    /// The speakers to have in the group.
    members: HashSet<PlayerId>,
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
        if let Some(highlighted) = &mut self.favorites_picker {
            *highlighted = (*highlighted).min(state.favorites.len().saturating_sub(1));
        }
        if state.groups.is_empty() {
            self.group_picker = None;
        } else if let Some(highlighted) = &mut self.group_picker {
            *highlighted = (*highlighted).min(state.groups.len() - 1);
        }
        if self
            .group_editor
            .as_ref()
            .is_some_and(|editor| state.group(&editor.group).is_none())
        {
            self.group_editor = None;
        }
        if let Some(editor) = &mut self.group_editor {
            editor.highlighted = editor
                .highlighted
                .min(state.players.len().saturating_sub(1));
        }
        let mixer_rows = self
            .current_group(state)
            .map(|(_, group)| state.members(group).count() + 1);
        self.volume_mixer = self
            .volume_mixer
            .zip(mixer_rows)
            .map(|(highlighted, rows)| highlighted.min(rows - 1));
    }

    fn popup_open(&self) -> bool {
        self.group_picker.is_some()
            || self.group_editor.is_some()
            || self.volume_mixer.is_some()
            || self.favorites_picker.is_some()
            || self.help
            || self.config_dialog.is_some()
            || self.theme_picker.is_some()
    }

    /// The name of the theme to show instead of the configured one, while choosing one.
    pub fn previewed_theme(&self) -> Option<&'static str> {
        let entry = THEMES.get(self.theme_picker?)?;
        Some(&entry.name)
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
        let id = self.selected_group.as_ref()?;
        state.groups.iter().enumerate().find(|(_, g)| &g.id == id)
    }

    /// Open the group picker, with the selected group highlighted.
    fn open_group_picker(&mut self, state: &HouseholdState) {
        if let Some((index, _)) = self.current_group(state) {
            self.group_picker = Some(index);
        }
    }

    /// Open the group editor for the selected group, with its coordinator highlighted.
    fn open_group_editor(&mut self, state: &HouseholdState) {
        let Some((_, group)) = self.current_group(state) else {
            return;
        };
        let highlighted = state
            .players
            .iter()
            .position(|p| p.id == group.coordinator_id)
            .unwrap_or(0);
        self.group_editor = Some(GroupEditor {
            group: group.id.clone(),
            highlighted,
            members: group.player_ids.iter().cloned().collect(),
        });
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
    ui: &UiState,
    artwork: &mut Artwork,
    settings: &UiConfig,
    theme: &Theme,
) {
    let symbols = Symbols::choose(settings.nerd_font);
    frame.render_widget(Block::new().style(theme.base()), frame.area());
    match &state.status {
        ConnectionStatus::Connecting => {
            return render_message(frame, theme, "Connecting to your Sonos speakers...");
        }
        ConnectionStatus::Failed(err) => {
            return render_message(
                frame,
                theme,
                &format!("Failed to connect to your Sonos speakers: {err}\n\nPress q to quit."),
            );
        }
        ConnectionStatus::Connected => {}
    }
    let Some((group_index, group)) = ui.current_group(state) else {
        return render_message(frame, theme, "No speakers found.\n\nPress q to quit.");
    };
    let [title, now_playing] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(frame.area());

    render_title_bar(state, ui, group, symbols, theme, frame, title);
    // Images can be drawn on top of the text, so hide the album art behind popups.
    render_now_playing(
        group,
        symbols,
        theme,
        frame,
        now_playing,
        artwork,
        !ui.popup_open(),
    );

    // Popups, on top of everything else
    if let Some(highlighted) = ui.favorites_picker {
        render_favorites_picker(state, theme, highlighted, frame);
    }
    if let Some(highlighted) = ui.volume_mixer {
        render_volume_mixer(state, group, theme, highlighted, frame);
    }
    if let Some(highlighted) = ui.group_picker {
        render_group_picker(state, theme, group_index, highlighted, frame);
    }
    if let Some(editor) = &ui.group_editor {
        render_group_editor(state, theme, editor, frame);
    }
    if let Some(highlighted) = ui.config_dialog {
        render_config_dialog(settings, symbols, theme, highlighted, frame);
    }
    if let Some(highlighted) = ui.theme_picker {
        render_theme_picker(&settings.theme, theme, highlighted, frame);
    }
    if ui.help {
        render_help(theme, frame);
    }
}

/// Update `ui` according to the key pressed, and return what else it asks for, if anything.
pub fn handle_input(
    input: &KeyEvent,
    state: &HouseholdState,
    settings: &UiConfig,
    ui: &mut UiState,
) -> Option<Request> {
    if ui.help {
        if matches!(input.code, KeyCode::Esc | KeyCode::Char('?' | 'h')) {
            ui.help = false;
        }
        return None;
    }
    if let Some(highlighted) = ui.theme_picker {
        return handle_theme_picker_input(input, ui, highlighted);
    }
    if let Some(highlighted) = ui.config_dialog {
        return handle_config_dialog_input(input, settings, ui, highlighted);
    }
    let command = handle_command_input(input, state, ui)?;
    Some(Request::Speakers(command))
}

/// Update `ui` according to the key pressed outside of the help and the configuration dialog, and
/// return the command to send to the speakers, if any.
fn handle_command_input(
    input: &KeyEvent,
    state: &HouseholdState,
    ui: &mut UiState,
) -> Option<Command> {
    if let Some(highlighted) = ui.group_picker {
        handle_group_picker_input(input, state, ui, highlighted);
        return None;
    }
    if ui.group_editor.is_some() {
        return handle_group_editor_input(input, state, ui);
    }
    if let Some(highlighted) = ui.volume_mixer {
        return handle_volume_mixer_input(input, state, ui, highlighted);
    }
    if let Some(highlighted) = ui.favorites_picker {
        return handle_favorites_picker_input(input, state, ui, highlighted);
    }
    match input.code {
        KeyCode::Char('f') => ui.favorites_picker = Some(0),

        // Group switching
        KeyCode::Char('g') => ui.open_group_picker(state),
        KeyCode::Char('G') => ui.open_group_editor(state),

        KeyCode::Char('v') => ui.volume_mixer = Some(0),

        KeyCode::Char('?' | 'h') => ui.help = true,
        KeyCode::Char(',') => ui.config_dialog = Some(0),

        _ => return group_command(input, state, ui),
    }
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
    match input.code {
        KeyCode::Up | KeyCode::Char('k') => {
            ui.config_dialog = Some(highlighted.saturating_sub(1));
        }
        KeyCode::Down | KeyCode::Char('j') => {
            ui.config_dialog = Some((highlighted + 1).min(rows - 1));
        }
        KeyCode::Char(' ') | KeyCode::Enter => match UiSetting::ALL.get(highlighted) {
            Some(setting) => return Some(Request::Toggle(*setting)),
            None => {
                let current = THEMES
                    .iter()
                    .position(|entry| entry.name == settings.theme)
                    .unwrap_or(0);
                ui.theme_picker = Some(current);
            }
        },
        KeyCode::Esc | KeyCode::Char(',') => ui.config_dialog = None,
        _ => {}
    }
    None
}

/// Update `ui` according to the key pressed while the theme picker is open, with the theme at
/// `highlighted` highlighted, and return the theme chosen, if any.
fn handle_theme_picker_input(
    input: &KeyEvent,
    ui: &mut UiState,
    highlighted: usize,
) -> Option<Request> {
    match input.code {
        KeyCode::Up | KeyCode::Char('k') => {
            ui.theme_picker = Some(highlighted.saturating_sub(1));
        }
        KeyCode::Down | KeyCode::Char('j') => {
            ui.theme_picker = Some((highlighted + 1).min(THEMES.len() - 1));
        }
        KeyCode::Enter => {
            ui.theme_picker = None;
            let entry = THEMES.get(highlighted)?;
            return Some(Request::SetTheme(entry.name.clone()));
        }
        KeyCode::Esc => ui.theme_picker = None,
        _ => {}
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

/// Update `ui` according to the key pressed while the group editor is open, and return the
/// command changing the members of the group, if any.
fn handle_group_editor_input(
    input: &KeyEvent,
    state: &HouseholdState,
    ui: &mut UiState,
) -> Option<Command> {
    let editor = ui.group_editor.as_mut()?;
    let group = state.group(&editor.group)?;
    match input.code {
        KeyCode::Up | KeyCode::Char('k') => {
            editor.highlighted = editor.highlighted.saturating_sub(1)
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if editor.highlighted + 1 < state.players.len() {
                editor.highlighted += 1;
            }
        }
        KeyCode::Char(' ') => {
            let player = state.players.get(editor.highlighted)?;
            // The coordinator stays, so that the group keeps playing.
            if player.id != group.coordinator_id && !editor.members.remove(&player.id) {
                editor.members.insert(player.id.clone());
            }
        }
        KeyCode::Enter => {
            let editor = ui.group_editor.take()?;
            if group.player_ids.iter().cloned().collect::<HashSet<_>>() == editor.members {
                return None;
            }
            let others = state
                .players
                .iter()
                .filter(|p| p.id != group.coordinator_id && editor.members.contains(&p.id))
                .map(|p| p.id.clone());
            let players = std::iter::once(group.coordinator_id.clone())
                .chain(others)
                .collect();
            return Some(Command::Group {
                group: group.id.clone(),
                action: GroupAction::SetMembers(players),
            });
        }
        KeyCode::Esc | KeyCode::Char('G') => ui.group_editor = None,
        _ => {}
    }
    None
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
    let delta = match input.code {
        KeyCode::Up | KeyCode::Char('k') => {
            ui.volume_mixer = Some(highlighted.saturating_sub(1));
            return None;
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if highlighted < state.members(group).count() {
                ui.volume_mixer = Some(highlighted + 1);
            }
            return None;
        }
        KeyCode::Esc | KeyCode::Char('v') => {
            ui.volume_mixer = None;
            return None;
        }
        KeyCode::Left | KeyCode::Char('h' | '[') => -VOLUME_STEP,
        KeyCode::Right | KeyCode::Char('l' | ']') => VOLUME_STEP,
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
    match input.code {
        KeyCode::Up | KeyCode::Char('k') => {
            ui.favorites_picker = Some(highlighted.saturating_sub(1));
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if highlighted + 1 < state.favorites.len() {
                ui.favorites_picker = Some(highlighted + 1);
            }
        }
        KeyCode::Enter => {
            ui.favorites_picker = None;
            let favorite = state.favorites.get(highlighted)?;
            let (_, group) = ui.current_group(state)?;
            return Some(Command::Group {
                group: group.id.clone(),
                action: GroupAction::LoadFavorite(favorite.id.clone()),
            });
        }
        KeyCode::Esc | KeyCode::Char('f') => ui.favorites_picker = None,
        _ => {}
    }
    None
}

/// The command for the selected group corresponding to the key pressed, if any.
fn group_command(input: &KeyEvent, state: &HouseholdState, ui: &UiState) -> Option<Command> {
    let (_, group) = ui.current_group(state)?;
    let required = match input.code {
        KeyCode::Char(' ') => Some(Action::PlayPause),
        KeyCode::Char('n') => Some(Action::Next),
        KeyCode::Char('p') => Some(Action::Previous),
        KeyCode::Char('s') => Some(Action::Shuffle),
        KeyCode::Char('r') => Some(Action::Repeat),
        KeyCode::Char('c') => Some(Action::Crossfade),
        _ => None,
    };
    if required.is_some_and(|action| !supports(group, action)) {
        return None;
    }
    let action = match input.code {
        KeyCode::Char(' ') => {
            if group.playback.is_some_and(|p| p.is_playing) {
                GroupAction::Pause
            } else {
                GroupAction::Play
            }
        }
        KeyCode::Char('n') => GroupAction::Next,
        KeyCode::Char('p') => GroupAction::Previous,
        KeyCode::Char('[') => GroupAction::AdjustVolume(-VOLUME_STEP),
        KeyCode::Char(']') => GroupAction::AdjustVolume(VOLUME_STEP),

        // Play modes
        KeyCode::Char('s') => GroupAction::SetPlayModes(PlayModesUpdate {
            shuffle: Some(!group.playback?.play_modes.shuffle),
            ..Default::default()
        }),
        KeyCode::Char('r') => {
            let playback = group.playback?;
            GroupAction::SetPlayModes(next_repeat(playback.play_modes, playback.actions))
        }
        KeyCode::Char('c') => GroupAction::SetPlayModes(PlayModesUpdate {
            crossfade: Some(!group.playback?.play_modes.crossfade),
            ..Default::default()
        }),

        _ => return None,
    };
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
    const MIN_VOLUME_WIDTH: u16 = 44;

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
    let volume_width = u16::try_from(controls.width())
        .unwrap_or(u16::MAX)
        .max(MIN_VOLUME_WIDTH);
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
    // As wide as the value, so that the slider is centred in `area`.
    const SIDE_WIDTH: u16 = 3;

    let [icon_area, slider_area, value_area] = Layout::horizontal([
        Constraint::Length(SIDE_WIDTH),
        Constraint::Min(1),
        Constraint::Length(SIDE_WIDTH),
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

/// A popup listing the groups, with the selected group (at `selected`) in colour and the one at
/// `highlighted` highlighted.
fn render_group_picker(
    state: &HouseholdState,
    theme: &Theme,
    selected: usize,
    highlighted: usize,
    frame: &mut Frame,
) {
    let lines = state
        .groups
        .iter()
        .enumerate()
        .map(|(i, group)| {
            let name_style = if i == selected {
                Style::default().fg(theme.name)
            } else {
                Style::default()
            };
            let mut spans = vec![Span::styled(group.name.as_str(), name_style)];
            if group.playback.is_some_and(|p| p.is_playing)
                && let Some(track) = &group.now_playing
            {
                spans.push(Span::styled(
                    format!("  ♪ {}", track.title.as_deref().unwrap_or("Unknown")),
                    Style::default().fg(theme.muted),
                ));
            }
            Line::from(spans)
        })
        .collect();

    render_popup(
        frame,
        theme,
        " Groups ",
        lines,
        Some(highlighted),
        " ↑↓ Navigate • ENTER select • ESC close ",
    );
}

/// A popup listing all the speakers, with the ones to have in the group being edited checked.
fn render_group_editor(
    state: &HouseholdState,
    theme: &Theme,
    editor: &GroupEditor,
    frame: &mut Frame,
) {
    let Some(group) = state.group(&editor.group) else {
        return;
    };
    let lines = state
        .players
        .iter()
        .map(|player| {
            let checkbox = if editor.members.contains(&player.id) {
                "[x] "
            } else {
                "[ ] "
            };
            let mut spans = vec![Span::raw(checkbox), Span::raw(player.name.as_str())];
            if player.id == group.coordinator_id {
                spans.push(Span::styled(
                    "  coordinator",
                    Style::default().fg(theme.muted),
                ));
            }
            Line::from(spans)
        })
        .collect();

    render_popup(
        frame,
        theme,
        " Group speakers ",
        lines,
        Some(editor.highlighted),
        " ↑↓ Navigate • SPACE toggle • ENTER apply • ESC cancel ",
    );
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

    let rows: Vec<_> = std::iter::once(("Group", group.volume))
        .chain(
            state
                .members(group)
                .map(|player| (player.name.as_str(), player.volume)),
        )
        .collect();
    let label_width = rows
        .iter()
        .map(|(label, _)| Span::raw(*label).width())
        .max()
        .unwrap_or(0);

    // Leave room for the sliders, which are drawn over the lines afterwards.
    let lines = rows
        .iter()
        .map(|(label, volume)| {
            let padding = " ".repeat(label_width - Span::raw(*label).width() + 2);
            Line::from(vec![
                Span::raw(*label),
                Span::raw(padding),
                Span::raw(" ".repeat(BAR_WIDTH)),
                Span::raw(format!(" {}", volume_text(*volume))),
            ])
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
    let padding = Span::raw(symbols.checked).width() - Span::raw(symbols.theme).width();
    lines.push(Line::from(vec![
        Span::styled(symbols.theme, Style::default().fg(theme.secondary)),
        Span::raw(" ".repeat(padding + 1)),
        Span::raw("Theme: "),
        Span::styled(
            theme::display_name(&settings.theme).to_owned(),
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
    use crate::sonos::{PlayerAction, PlayerInfo};

    fn household() -> HouseholdState {
        let player = |id: &str, name: &str| PlayerInfo {
            id: id.into(),
            name: name.to_owned(),
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
        assert!(ui.group_editor.is_none());
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
        assert!(ui.group_editor.is_none());

        let keys = [KeyCode::Char('G'), KeyCode::Char('j'), KeyCode::Char(' ')];
        assert!(press(&keys, &state, &mut ui).is_none());
        assert!(press(&[KeyCode::Esc], &state, &mut ui).is_none());
        assert!(ui.group_editor.is_none());
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
        assert!(ui.volume_mixer.is_none());
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
        assert!(ui.config_dialog.is_none());
    }

    #[test]
    fn help_closes_with_the_keys_opening_it() {
        let state = household();
        let mut ui = UiState::default();
        ui.sync(&state);

        assert!(press(&[KeyCode::Char('?'), KeyCode::Char(' ')], &state, &mut ui).is_none());
        assert!(ui.help);
        press(&[KeyCode::Char('h')], &state, &mut ui);
        assert!(!ui.help);
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
        assert!(ui.config_dialog.is_some());

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
