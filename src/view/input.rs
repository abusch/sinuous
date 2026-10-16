//! What key presses do.

use std::collections::HashSet;

use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use sinuous_client::{
    PlayerId,
    playback::{PlayModes, PlayModesUpdate, PlaybackActions},
};

use crate::config::{UiConfig, UiSetting};
use crate::sonos::{Command, GroupAction, GroupState, HouseholdState, PlayerAction};
use crate::theme::THEMES;

use super::{
    GroupEditor, Mode, PanelHighlight, PlaybackItem, Request, SLEEP_TIMER_MINUTES, Scroll,
    Selection, UiState,
};

/// How much the volume changes with each key press.
const VOLUME_STEP: i8 = 1;

/// The row to highlight in a list of `len` rows when `key` moves the highlight from the row at
/// `highlighted`, if it does. The highlight stays on the first and last rows.
fn navigate(key: KeyCode, highlighted: usize, len: usize) -> Option<usize> {
    match key {
        KeyCode::Up | KeyCode::Char('k') => Some(highlighted.saturating_sub(1)),
        KeyCode::Down | KeyCode::Char('j') => Some((highlighted + 1).min(len.saturating_sub(1))),
        _ => None,
    }
}

/// Update `ui` according to the key pressed, and return what else it asks for, if anything.
pub fn handle_input(
    input: &KeyEvent,
    state: &HouseholdState,
    settings: &UiConfig,
    ui: &mut UiState,
) -> Option<Request> {
    // q quits from the main screen, and closes what is open otherwise, like ESC.
    let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
    let input = match input.code {
        KeyCode::Char('q') if matches!(ui.mode, Mode::NowPlaying) => return Some(Request::Quit),
        KeyCode::Char('q') => &esc,
        _ => input,
    };
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
        Mode::PlaybackMenu {
            highlighted,
            sleep_timer_picker: None,
        } => handle_playback_menu_input(input, state, ui, highlighted),
        Mode::PlaybackMenu {
            highlighted,
            sleep_timer_picker: Some(choice),
        } => handle_sleep_timer_picker_input(input, state, ui, highlighted, choice),
        Mode::SystemInfo(scroll) => {
            handle_system_info_input(input, ui, scroll);
            None
        }
        Mode::Help => {
            if matches!(input.code, KeyCode::Esc | KeyCode::Char('?')) {
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
        KeyCode::Char('.') => Mode::PlaybackMenu {
            highlighted: 0,
            sleep_timer_picker: None,
        },

        KeyCode::Char('?') => Mode::Help,
        KeyCode::Char('i') => Mode::SystemInfo(Scroll::default()),
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

/// Update `ui` according to the key pressed while the system info is open, scrolled to `scroll`.
fn handle_system_info_input(input: &KeyEvent, ui: &mut UiState, scroll: Scroll) {
    // Scrolling past the end is undone when drawing it, which is when its length is known.
    let page = scroll.page.max(1);
    let offset = match input.code {
        KeyCode::Up | KeyCode::Char('k') => scroll.offset.saturating_sub(1),
        KeyCode::Down | KeyCode::Char('j') => scroll.offset.saturating_add(1),
        KeyCode::PageUp => scroll.offset.saturating_sub(page),
        KeyCode::PageDown => scroll.offset.saturating_add(page),
        KeyCode::Home => 0,
        KeyCode::End => usize::MAX,
        KeyCode::Esc | KeyCode::Char('i') => {
            ui.mode = Mode::NowPlaying;
            return;
        }
        _ => return,
    };
    ui.mode = Mode::SystemInfo(Scroll { offset, ..scroll });
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

/// Update `ui` according to the key pressed while the playback menu is open, with the item at
/// `highlighted` highlighted, and return the command it asks for, if any.
fn handle_playback_menu_input(
    input: &KeyEvent,
    state: &HouseholdState,
    ui: &mut UiState,
    highlighted: usize,
) -> Option<Command> {
    if let Some(highlighted) = navigate(input.code, highlighted, PlaybackItem::ALL.len()) {
        ui.mode = Mode::PlaybackMenu {
            highlighted,
            sleep_timer_picker: None,
        };
        return None;
    }
    match input.code {
        // It stays open, to see the change.
        KeyCode::Char(' ') | KeyCode::Enter => {
            let (_, group) = ui.current_group(state)?;
            match PlaybackItem::ALL.get(highlighted)? {
                PlaybackItem::Crossfade => {
                    let update = PlayModesUpdate {
                        crossfade: Some(!group.playback?.play_modes.crossfade),
                        ..Default::default()
                    };
                    command_for(
                        group,
                        (GroupAction::SetPlayModes(update), Some(Action::Crossfade)),
                    )
                }
                PlaybackItem::SleepTimer => {
                    ui.mode = Mode::PlaybackMenu {
                        highlighted,
                        sleep_timer_picker: Some(0),
                    };
                    None
                }
            }
        }
        KeyCode::Esc | KeyCode::Char('.') => {
            ui.mode = Mode::NowPlaying;
            None
        }
        _ => None,
    }
}

/// Update `ui` according to the key pressed while the sleep timer picker is open, over the
/// playback menu with the item at `item` highlighted, with the choice at `highlighted`
/// highlighted, and return the command setting the sleep timer, if any.
fn handle_sleep_timer_picker_input(
    input: &KeyEvent,
    state: &HouseholdState,
    ui: &mut UiState,
    item: usize,
    highlighted: usize,
) -> Option<Command> {
    let menu = |sleep_timer_picker| Mode::PlaybackMenu {
        highlighted: item,
        sleep_timer_picker,
    };
    if let Some(highlighted) = navigate(input.code, highlighted, SLEEP_TIMER_MINUTES.len()) {
        ui.mode = menu(Some(highlighted));
        return None;
    }
    match input.code {
        KeyCode::Enter => {
            ui.mode = menu(None);
            let minutes = *SLEEP_TIMER_MINUTES.get(highlighted)?;
            let (_, group) = ui.current_group(state)?;
            Some(Command::Group {
                group: group.id.clone(),
                action: GroupAction::SetSleepTimer(
                    minutes.map(|minutes| Duration::from_secs(minutes * 60)),
                ),
            })
        }
        KeyCode::Esc => {
            ui.mode = menu(None);
            None
        }
        _ => None,
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
    let action = match input.code {
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

        _ => return None,
    };
    command_for(group, action)
}

/// The command doing `action` for `group`, unless its source doesn't support what it requires.
fn command_for(
    group: &GroupState,
    (action, required): (GroupAction, Option<Action>),
) -> Option<Command> {
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
pub(super) enum Action {
    PlayPause,
    Next,
    Previous,
    Shuffle,
    Repeat,
    Crossfade,
}

/// Whether the source `group` is playing from supports `action`. Everything is supported until
/// the group has said otherwise.
pub(super) fn supports(group: &GroupState, action: Action) -> bool {
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

#[cfg(test)]
mod tests {
    use sinuous_client::GroupId;

    use super::*;
    use crate::sonos::Playback;
    use crate::view::tests::{household, press};

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
        assert!(press(&[KeyCode::Char('s')], &state, &mut ui).is_none());
        assert!(press(&[KeyCode::Esc], &state, &mut ui).is_none());
        assert!(matches!(ui.mode, Mode::NowPlaying));
    }

    #[test]
    fn q_closes_what_is_open_then_quits() {
        let state = household();
        let mut ui = UiState::default();
        ui.sync(&state);

        for opening in [',', 'v', '.', 'f', 'g', 'G', 'i', '?'] {
            assert!(press(&[KeyCode::Char(opening)], &state, &mut ui).is_none());
            assert!(!matches!(ui.mode, Mode::NowPlaying), "{opening}");
            assert!(press(&[KeyCode::Char('q')], &state, &mut ui).is_none());
            assert!(matches!(ui.mode, Mode::NowPlaying), "{opening}");
        }

        // The theme picker closes back to the configuration dialog.
        let mut keys = vec![KeyCode::Char(',')];
        keys.extend(UiSetting::ALL.map(|_| KeyCode::Char('j')));
        keys.extend([KeyCode::Enter, KeyCode::Char('q')]);
        assert!(press(&keys, &state, &mut ui).is_none());
        assert!(matches!(
            ui.mode,
            Mode::Config {
                theme_picker: None,
                ..
            }
        ));

        press(&[KeyCode::Char('q')], &state, &mut ui);
        assert!(matches!(
            press(&[KeyCode::Char('q')], &state, &mut ui),
            Some(Request::Quit)
        ));
    }

    #[test]
    fn playback_menu_toggles_crossfade_when_supported() {
        let mut state = household();
        let actions = PlaybackActions {
            can_crossfade: true,
            ..Default::default()
        };
        state.groups[0].playback = Some(Playback::paused(PlayModes::default(), actions));
        let mut ui = UiState::default();
        ui.sync(&state);

        let request = press(&[KeyCode::Char('.'), KeyCode::Char(' ')], &state, &mut ui);
        let Some(Request::Speakers(Command::Group {
            group,
            action: GroupAction::SetPlayModes(update),
        })) = request
        else {
            panic!("Unexpected request: {request:?}");
        };
        assert_eq!(group, GroupId::from("bedroom"));
        assert_eq!(update.crossfade, Some(true));
        // It stays open, to see the change.
        assert!(matches!(
            ui.mode,
            Mode::PlaybackMenu {
                highlighted: 0,
                sleep_timer_picker: None
            }
        ));

        state.groups[0].playback = Some(Playback::paused(
            PlayModes::default(),
            PlaybackActions::default(),
        ));
        assert!(press(&[KeyCode::Enter], &state, &mut ui).is_none());
        assert!(press(&[KeyCode::Char('.')], &state, &mut ui).is_none());
        assert!(matches!(ui.mode, Mode::NowPlaying));
    }

    #[test]
    fn sleep_timer_picker_sets_and_cancels_the_timer() {
        let state = household();
        let mut ui = UiState::default();
        ui.sync(&state);
        let sleep_timer = |request| match request {
            Some(Request::Speakers(Command::Group {
                group,
                action: GroupAction::SetSleepTimer(duration),
            })) => {
                assert_eq!(group, GroupId::from("bedroom"));
                duration
            }
            request => panic!("Unexpected request: {request:?}"),
        };

        // Off, then 15 and 30 minutes
        let keys = [
            KeyCode::Char('.'),
            KeyCode::Char('j'),
            KeyCode::Enter,
            KeyCode::Char('j'),
            KeyCode::Char('j'),
            KeyCode::Enter,
        ];
        let duration = sleep_timer(press(&keys, &state, &mut ui));
        assert_eq!(duration, Some(Duration::from_secs(30 * 60)));
        // Back to the menu, on the sleep timer
        assert!(matches!(
            ui.mode,
            Mode::PlaybackMenu {
                highlighted: 1,
                sleep_timer_picker: None
            }
        ));

        let duration = sleep_timer(press(&[KeyCode::Enter, KeyCode::Enter], &state, &mut ui));
        assert_eq!(duration, None);

        // Cancelling the picker leaves the timer alone.
        let keys = [KeyCode::Enter, KeyCode::Char('j'), KeyCode::Char('q')];
        assert!(press(&keys, &state, &mut ui).is_none());
        assert!(matches!(
            ui.mode,
            Mode::PlaybackMenu {
                sleep_timer_picker: None,
                ..
            }
        ));
    }

    #[test]
    fn help_closes_with_the_key_opening_it() {
        let state = household();
        let mut ui = UiState::default();
        ui.sync(&state);

        assert!(press(&[KeyCode::Char('?'), KeyCode::Char(' ')], &state, &mut ui).is_none());
        assert!(matches!(ui.mode, Mode::Help));
        press(&[KeyCode::Char('?')], &state, &mut ui);
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
}
