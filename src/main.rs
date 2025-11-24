use clap::{arg, command};
use tracing::{error, info};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{EnvFilter, fmt::format::FmtSpan};

mod app;
mod input;
mod sonos;
mod view;

use crate::{app::App, sonos::SpeakerState};

#[derive(Debug)]
pub enum State {
    Ready(Box<SpeakerState>),
    Connecting,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    Queue,
    Favorites,
}

#[derive(Debug)]
pub enum Direction {
    Up,
    Down,
}

#[derive(Debug)]
pub enum Action {
    Play,
    Pause,
    Next,
    Prev,
    NextSpeaker,
    PrevSpeaker,
    VolAdjust(i16),
    SwitchView(ViewMode),
    NavigateFavorites(Direction),
    PlayFavorite(usize),
    Nop,
}

#[derive(Debug)]
pub enum Update {
    NewState(Box<SpeakerState>),
    Nop,
}

#[tokio::main]
async fn main() {
    human_panic::setup_panic!();

    let _guard = init_logger();
    info!("Welcome to Sinuous!");

    // Set the App with clap to accept Command Line Arguments
    let args = command!()
        .arg(
            arg!(
                -d --device <device> "Specify a speaker to connect to. Provide either an Ipv4 Address or a name to search for. Multiple values are possible by seperating them with a comma"
            )
            .required(false)
        )
        .get_matches();

    let app = App::new(args);
    // Initialize the terminal user interface.
    let mut terminal = ratatui::init();

    if let Err(err) = app.run(&mut terminal).await {
        error!("Main loop exited with error: {}", err);
    } else {
        info!("Bye!");
    }
    ratatui::restore();
}

fn init_logger() -> WorkerGuard {
    // Initialize logging framework
    let rolling = tracing_appender::rolling::never(std::env::temp_dir(), "sinuous.log");
    let (appender, guard) = tracing_appender::non_blocking(rolling);
    tracing_subscriber::fmt::SubscriberBuilder::default()
        .with_writer(appender)
        .with_env_filter(EnvFilter::from_default_env())
        .with_ansi(false)
        .with_span_events(FmtSpan::FULL)
        .init();
    guard
}
