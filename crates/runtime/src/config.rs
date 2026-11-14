//! User settings: `uk_qol/config.toml`, one table per mod (PLAN §2).
//!
//! ```toml
//! [mods.skip-logos]
//! enabled = true
//! # every other key is a mod setting, as declared in the mod's mod.toml
//! ```
//!
//! The manager (`uk-qol enable/disable/config`) and the overlay edit the same
//! file. Interface fixed; implementation pending.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ModConfig {
    /// `None` = use the manifest default.
    pub enabled: Option<bool>,
    /// Setting values the user changed; missing keys use manifest defaults.
    pub settings: toml::Table,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Config {
    pub mods: BTreeMap<String, ModConfig>,
}

impl Config {
    /// Parses a config file's text. Unknown keys are kept.
    pub fn parse(text: &str) -> Result<Config, String> {
        let _ = text;
        Ok(Config::default())
    }

    pub fn to_toml(&self) -> String {
        String::new()
    }

    /// Reads `path`; a missing file is an empty config.
    pub fn load(path: &Path) -> Result<Config, String> {
        let _ = path;
        Ok(Config::default())
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let _ = path;
        Ok(())
    }
}

/// Watches the config file for changes (mtime), for reload on change.
pub struct Watcher {
    pub path: PathBuf,
}

impl Watcher {
    pub fn new(path: PathBuf) -> Watcher {
        Watcher { path }
    }

    /// Returns the new config if the file changed since the last call.
    pub fn poll(&mut self) -> Option<Config> {
        None
    }
}
