//! What is remembered from one run to the next, e.g. the speaker that was selected.
//!
//! Unlike the configuration, it isn't meant to be edited by hand.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use etcetera::BaseStrategy;
use serde::{Deserialize, Serialize};
use sinuous_client::PlayerId;
use tracing::{debug, warn};

use crate::config;

#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedState {
    /// The coordinator of the group that was selected last.
    pub speaker: Option<PlayerId>,
}

impl SavedState {
    /// Read the state saved at `path`, or the default one if there is none or it can't be read.
    pub fn load(path: &Path) -> Self {
        let contents = match fs::read_to_string(path) {
            Ok(contents) => contents,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                debug!("No saved state at {}", path.display());
                return Self::default();
            }
            Err(err) => {
                warn!("Failed to read {}: {err}", path.display());
                return Self::default();
            }
        };
        toml::from_str(&contents)
            .inspect_err(|err| warn!("Invalid saved state in {}: {err}", path.display()))
            .unwrap_or_default()
    }

    /// Save it to `path`, replacing what was saved there.
    pub fn save(&self, path: &Path) -> Result<()> {
        let contents = toml::to_string(self).context("Failed to serialize the state")?;
        config::write_atomically(path, &contents)
    }
}

/// Where the state is saved: `$XDG_STATE_HOME/sinuous/state.toml`, i.e.
/// `~/.local/state/sinuous/state.toml` by default, or `%APPDATA%\sinuous\state.toml` on
/// Windows.
pub fn default_path() -> Option<PathBuf> {
    let strategy = etcetera::choose_base_strategy().ok()?;
    let dir = strategy.state_dir().unwrap_or_else(|| strategy.data_dir());
    Some(dir.join("sinuous").join("state.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saves_and_loads_the_speaker() {
        let dir = std::env::temp_dir().join(format!("sinuous-state-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join("state.toml");
        assert_eq!(SavedState::load(&path), SavedState::default());

        let state = SavedState {
            speaker: Some("RINCON_1234".into()),
        };
        state.save(&path).unwrap();
        assert_eq!(SavedState::load(&path), state);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn ignores_what_it_doesnt_know() {
        let state: SavedState = toml::from_str("speaker = \"k\"\nlater = 1\n").unwrap();
        assert_eq!(state.speaker, Some("k".into()));
    }
}
