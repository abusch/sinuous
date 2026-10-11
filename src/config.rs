//! Settings read from the configuration file.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use etcetera::BaseStrategy;
use serde::Deserialize;
use tracing::{debug, warn};

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub core: CoreConfig,
    pub ui: UiConfig,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CoreConfig {
    /// The speakers to connect to, as IPv4 addresses or names. Speakers are discovered via SSDP
    /// if empty.
    pub devices: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UiConfig {
    /// Whether to show the album art of the current track.
    pub album_art: bool,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self { album_art: true }
    }
}

impl Config {
    /// Read the configuration file at `path`, or at [`default_path`] if `None`.
    ///
    /// Not having a configuration file at the default location is fine: the defaults are used.
    pub fn load(path: Option<&Path>) -> Result<Self> {
        let (path, required) = match path {
            Some(path) => (path.to_path_buf(), true),
            None => match default_path() {
                Some(path) => (path, false),
                None => {
                    warn!("Failed to find the home directory: using the default configuration");
                    return Ok(Self::default());
                }
            },
        };
        let contents = match fs::read_to_string(&path) {
            Ok(contents) => contents,
            Err(err) if err.kind() == io::ErrorKind::NotFound && !required => {
                debug!("No configuration file at {}", path.display());
                return Ok(Self::default());
            }
            Err(err) => {
                return Err(err).with_context(|| format!("Failed to read {}", path.display()));
            }
        };
        debug!("Reading configuration from {}", path.display());
        toml::from_str(&contents)
            .with_context(|| format!("Invalid configuration in {}", path.display()))
    }
}

/// Where the configuration file is looked for: `$XDG_CONFIG_HOME/sinuous/config.toml`, i.e.
/// `~/.config/sinuous/config.toml` by default, or `%APPDATA%\sinuous\config.toml` on Windows.
pub fn default_path() -> Option<PathBuf> {
    let strategy = etcetera::choose_base_strategy().ok()?;
    Some(strategy.config_dir().join("sinuous").join("config.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_file_gives_defaults() {
        let config: Config = toml::from_str("").unwrap();
        assert!(config.core.devices.is_empty());
        assert!(config.ui.album_art);
    }

    #[test]
    fn reads_all_settings() {
        let config: Config = toml::from_str(
            r#"
            [core]
            devices = ["192.168.1.10", "Living Room"]

            [ui]
            album_art = false
            "#,
        )
        .unwrap();
        assert_eq!(config.core.devices, ["192.168.1.10", "Living Room"]);
        assert!(!config.ui.album_art);
    }

    #[test]
    fn rejects_unknown_settings() {
        assert!(toml::from_str::<Config>("[ui]\nalbum_artt = false").is_err());
    }
}
