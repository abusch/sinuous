use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{arg, command, value_parser};
use tracing::{error, info};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{EnvFilter, fmt::format::FmtSpan};

mod app;
mod artwork;
mod config;
mod input;
mod sonos;
mod spinner;
mod symbols;
mod theme;
mod view;

use crate::{app::App, config::Config, sonos::ProvidedDevices};

#[tokio::main]
async fn main() -> Result<()> {
    human_panic::setup_panic!();

    // Pick the crypto provider used to talk to the speakers over TLS.
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("a crypto provider is already installed");

    let _guard = init_logger();
    info!("Welcome to Sinuous!");

    // Set the App with clap to accept Command Line Arguments
    let args = command!()
        .arg(
            arg!(
                -d --device <device> "Specify a speaker to connect to. Provide either an Ipv4 Address or a name to search for. Multiple values are possible by seperating them with a comma"
            )
            .required(false)
            .value_delimiter(',')
        )
        .arg(
            arg!(-c --config <path> "Read the configuration from this file instead of the default location")
                .required(false)
                .value_parser(value_parser!(PathBuf))
        )
        .get_matches();

    let config_path = args.get_one::<PathBuf>("config").cloned();
    let mut config = Config::load(config_path.as_deref())?;
    // The command line overrides the configuration file.
    if let Some(devices) = args.get_many::<String>("device") {
        config.core.devices = devices.cloned().collect();
    }

    // Connect before taking over the terminal, so that why we couldn't stays on screen.
    let devices = ProvidedDevices::new(config.core.devices);
    let household = spinner::spin(
        "Connecting to your Sonos speakers...",
        sonos::connect(devices),
    )
    .await
    .context("Failed to connect to your Sonos speakers")
    .inspect_err(|err| error!("{err:#}"))?;

    let app = App::new(
        household,
        config.ui,
        config_path.or_else(config::default_path),
    );
    // Initialize the terminal user interface.
    let mut terminal = ratatui::init();

    if let Err(err) = app.run(&mut terminal).await {
        error!("Main loop exited with error: {}", err);
    } else {
        info!("Bye!");
    }
    ratatui::restore();
    Ok(())
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
