//! A spinner shown in the terminal while waiting, before the user interface takes it over.

use std::io::{self, IsTerminal};
use std::time::Duration;

use crossterm::{
    cursor::MoveToColumn,
    execute,
    style::Print,
    terminal::{Clear, ClearType},
};
use tokio::{
    select,
    time::{self, MissedTickBehavior},
};

const FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
const FRAME_INTERVAL: Duration = Duration::from_millis(80);

/// Show `message` next to a spinner until `future` completes, then erase it.
pub async fn spin<F: Future>(message: &str, future: F) -> F::Output {
    let mut stderr = io::stderr();
    if !stderr.is_terminal() {
        return future.await;
    }

    let mut ticks = time::interval(FRAME_INTERVAL);
    ticks.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut frames = FRAMES.iter().cycle();
    let mut future = std::pin::pin!(future);
    let output = loop {
        select! {
            output = &mut future => break output,
            _ = ticks.tick() => {
                let frame = frames.next().expect("the frames cycle forever");
                // Not worth failing over: the spinner is only decoration.
                let _ = execute!(
                    stderr,
                    MoveToColumn(0),
                    Clear(ClearType::CurrentLine),
                    Print(format!("{frame} {message}")),
                );
            }
        }
    };
    let _ = execute!(stderr, MoveToColumn(0), Clear(ClearType::CurrentLine));
    output
}
