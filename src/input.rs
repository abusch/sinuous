use crossterm::event::{Event, KeyCode, KeyEvent};

pub fn should_quit(key: &Event) -> bool {
    matches!(
        key,
        Event::Key(KeyEvent {
            code: KeyCode::Char('q'),
            ..
        })
    )
}
