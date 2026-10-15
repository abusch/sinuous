//! Settings read from the configuration file.

use std::{
    ffi::OsString,
    fs, io,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use etcetera::BaseStrategy;
use serde::Deserialize;
use toml_edit::{DocumentMut, Item, Table, Value};
use tracing::{debug, warn};

use crate::theme;

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
    /// Whether to show icons instead of labels, for terminals using a Nerd Font.
    pub nerd_font: bool,
    /// The colours to use: the terminal's, or those of one of the themes in [`crate::theme`].
    pub theme: String,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            album_art: true,
            nerd_font: false,
            theme: theme::TERMINAL.to_owned(),
        }
    }
}

impl UiConfig {
    pub fn get(&self, setting: UiSetting) -> bool {
        match setting {
            UiSetting::AlbumArt => self.album_art,
            UiSetting::NerdFont => self.nerd_font,
        }
    }

    pub fn set(&mut self, setting: UiSetting, value: bool) {
        match setting {
            UiSetting::AlbumArt => self.album_art = value,
            UiSetting::NerdFont => self.nerd_font = value,
        }
    }
}

/// The settings of [`UiConfig`] that can be toggled while running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiSetting {
    AlbumArt,
    NerdFont,
}

impl UiSetting {
    pub const ALL: [Self; 2] = [Self::AlbumArt, Self::NerdFont];

    /// Its name in the configuration file.
    pub fn key(self) -> &'static str {
        match self {
            Self::AlbumArt => "album_art",
            Self::NerdFont => "nerd_font",
        }
    }

    /// What it is shown as in the configuration dialog.
    pub fn description(self) -> &'static str {
        match self {
            Self::AlbumArt => "Show album art",
            Self::NerdFont => "Show icons (requires a Nerd Font)",
        }
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

/// Set the `key` setting of the `[ui]` table to `value` in the configuration file at `path`,
/// creating it if needed, and keeping everything else in it as is, comments included.
pub fn save(path: &Path, key: &str, value: impl Into<Value>) -> Result<()> {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(err) if err.kind() == io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(err).with_context(|| format!("Failed to read {}", path.display())),
    };
    let mut document: DocumentMut = contents
        .parse()
        .with_context(|| format!("Invalid configuration in {}", path.display()))?;
    set_in(&mut document, key, value.into())
        .with_context(|| format!("Invalid configuration in {}", path.display()))?;

    write_atomically(path, &document.to_string())
}

/// Write `contents` to `path`, so that it has either its old or its new contents if writing fails
/// half-way.
fn write_atomically(path: &Path, contents: &str) -> Result<()> {
    // Replace the file a symbolic link points to, e.g. in a dotfiles repository, not the link.
    let path = match fs::canonicalize(path) {
        Ok(target) => target,
        Err(err) if err.kind() == io::ErrorKind::NotFound => path.to_path_buf(),
        Err(err) => return Err(err).with_context(|| format!("Failed to read {}", path.display())),
    };
    let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
        bail!("{} isn't a file", path.display());
    };
    fs::create_dir_all(dir).with_context(|| format!("Failed to create {}", dir.display()))?;

    let mut temp_name = OsString::from(".");
    temp_name.push(name);
    temp_name.push(".tmp");
    let temp = dir.join(temp_name);
    fs::write(&temp, contents).with_context(|| format!("Failed to write {}", temp.display()))?;
    if let Err(err) = fs::rename(&temp, &path) {
        let _ = fs::remove_file(&temp);
        return Err(err).with_context(|| format!("Failed to replace {}", path.display()));
    }
    Ok(())
}

/// Set the `key` setting to `value` in the `[ui]` table of `document`.
fn set_in(document: &mut DocumentMut, key: &str, value: Value) -> Result<()> {
    let ui = document
        .entry("ui")
        .or_insert_with(|| Item::Table(Table::new()))
        .as_table_like_mut()
        .context("`ui` isn't a table")?;
    match ui.get_mut(key) {
        // Replace the value only, to keep the comments around the key.
        Some(item) => {
            let decor = item.as_value().map(|v| v.decor().clone());
            *item = Item::Value(value);
            if let (Some(decor), Some(value)) = (decor, item.as_value_mut()) {
                *value.decor_mut() = decor;
            }
        }
        None => {
            ui.insert(key, Item::Value(value));
        }
    }
    Ok(())
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
        assert!(!config.ui.nerd_font);
        assert_eq!(config.ui.theme, theme::TERMINAL);
    }

    #[test]
    fn reads_all_settings() {
        let config: Config = toml::from_str(
            r#"
            [core]
            devices = ["192.168.1.10", "Living Room"]

            [ui]
            album_art = false
            nerd_font = true
            theme = "nord"
            "#,
        )
        .unwrap();
        assert_eq!(config.core.devices, ["192.168.1.10", "Living Room"]);
        assert!(!config.ui.album_art);
        assert!(config.ui.nerd_font);
        assert_eq!(config.ui.theme, "nord");
    }

    #[test]
    fn sets_setting_keeping_the_rest() {
        let mut document: DocumentMut =
            "# My speakers\n[core]\ndevices = [\"Kitchen\"]\n\n[ui]\n# Pretty\nnerd_font = true\n"
                .parse()
                .unwrap();
        set_in(&mut document, UiSetting::AlbumArt.key(), false.into()).unwrap();
        set_in(&mut document, UiSetting::NerdFont.key(), false.into()).unwrap();
        assert_eq!(
            document.to_string(),
            "# My speakers\n[core]\ndevices = [\"Kitchen\"]\n\n[ui]\n# Pretty\nnerd_font = false\nalbum_art = false\n"
        );
    }

    #[test]
    fn sets_setting_in_empty_file() {
        let mut document = DocumentMut::new();
        set_in(&mut document, UiSetting::AlbumArt.key(), false.into()).unwrap();
        assert_eq!(document.to_string(), "[ui]\nalbum_art = false\n");
        let config: Config = toml::from_str(&document.to_string()).unwrap();
        assert!(!config.ui.album_art);
    }

    #[test]
    fn saves_through_symbolic_links() {
        let dir = std::env::temp_dir().join(format!("sinuous-config-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let target = dir.join("dotfiles.toml");
        fs::write(&target, "[ui]\nnerd_font = true\n").unwrap();
        let link = dir.join("config.toml");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, &link).unwrap();
        #[cfg(not(unix))]
        fs::copy(&target, &link).unwrap();

        save(&link, UiSetting::AlbumArt.key(), false).unwrap();

        #[cfg(unix)]
        {
            assert!(fs::symlink_metadata(&link).unwrap().is_symlink());
            assert_eq!(
                fs::read_to_string(&target).unwrap(),
                "[ui]\nnerd_font = true\nalbum_art = false\n"
            );
        }
        let names: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names.len(), 2, "{names:?}");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rejects_unknown_settings() {
        assert!(toml::from_str::<Config>("[ui]\nalbum_artt = false").is_err());
    }
}
