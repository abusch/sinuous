use std::io;

use anyhow::{Context, Result};
use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture},
    terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
};
use tui::{backend::CrosstermBackend, Terminal};

pub fn init() -> Result<(Terminal<CrosstermBackend<io::Stdout>>, OnShutdown)> {
    terminal::enable_raw_mode().context("Failed to enable crossterm raw mode")?;

    let mut stdout = std::io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen, EnableMouseCapture)
        .context("Failed to enable crossterm alternate screen and mouse capture")?;
    let backend = CrosstermBackend::new(stdout);
    let term = Terminal::new(backend).context("Failed to create crossterm terminal")?;

    let cleanup = OnShutdown::new(|| {
        // Be a good terminal citizen...
        reset()
    });

    Ok((term, cleanup))
}

pub fn reset() -> Result<()> {
    let mut stdout = std::io::stdout();
    crossterm::execute!(stdout, LeaveAlternateScreen, DisableMouseCapture)
        .context("Failed to disable crossterm alternate screen and mouse capture")?;
    terminal::disable_raw_mode().context("Failed to enable crossterm raw mode")?;
    Ok(())
}

pub struct OnShutdown {
    action: fn() -> Result<()>,
}

impl OnShutdown {
    fn new(action: fn() -> Result<()>) -> Self {
        Self { action }
    }
}

impl Drop for OnShutdown {
    fn drop(&mut self) {
        if let Err(error) = (self.action)() {
            tracing::error!(%error, "error running terminal cleanup");
        }
    }
}
