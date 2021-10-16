use crossterm::event::{Event, KeyCode, KeyEvent};

pub fn should_quit(key: &Event) -> bool {
    match key {
        Event::Key(KeyEvent { code: KeyCode::Char('q'), ..}) => true,
        _ => false,
    }
}
