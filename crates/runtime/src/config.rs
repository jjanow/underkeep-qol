//! User settings: `uk_qol/config.toml`, one table per mod (PLAN §2).
//!
//! ```toml
//! [mods.skip-logos]
//! enabled = true
//! # every other key is a mod setting, as declared in the mod's mod.toml
//! ```
//!
//! The manager (`uk-qol enable/disable/config`) and the overlay edit the same
//! file. Both write it with [`write_atomic`], so a reader never sees half a file.
//!
//! - Keys this version doesn't know are kept: other top-level keys and tables
//!   in [`Config::extra`], every key of a mod table except `enabled` in
//!   [`ModConfig::settings`]. Comments and key order are not kept (keys are
//!   written sorted).
//! - A file that doesn't parse is an error, never an empty config: callers
//!   must not save defaults over a file they couldn't read.
//! - [`effective_settings`] overlays the user's values on the manifest defaults.
//! - [`Watcher`] polls the file's mtime and length for reload on change.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::SystemTime;

use toml::{Table, Value};

/// File name of the config in the data folder (`<game folder>/uk_qol`).
pub const FILE_NAME: &str = "config.toml";

/// `<data_dir>/config.toml`.
pub fn config_path(data_dir: &Path) -> PathBuf {
    data_dir.join(FILE_NAME)
}

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
    /// Top-level keys other than `mods`, kept as they were read.
    pub extra: toml::Table,
}

impl Config {
    /// Parses a config file's text. Unknown keys are kept.
    ///
    /// Errors: invalid TOML, `mods` or `mods.<id>` not a table, `enabled` not
    /// a boolean.
    pub fn parse(text: &str) -> Result<Config, String> {
        let mut root: Table = text.parse().map_err(|e| format!("config.toml: {e}"))?;
        let mut cfg = Config::default();
        if let Some(mods) = root.remove("mods") {
            let Value::Table(mods) = mods else {
                return Err("config.toml: `mods` must be a table".into());
            };
            for (id, m) in mods {
                let Value::Table(mut settings) = m else {
                    return Err(format!("config.toml: `mods.{id}` must be a table"));
                };
                let enabled = match settings.remove("enabled") {
                    None => None,
                    Some(Value::Boolean(b)) => Some(b),
                    Some(_) => {
                        return Err(format!(
                            "config.toml: `mods.{id}.enabled` must be true or false"
                        ));
                    }
                };
                cfg.mods.insert(id, ModConfig { enabled, settings });
            }
        }
        cfg.extra = root;
        Ok(cfg)
    }

    pub fn to_toml(&self) -> String {
        let mut root = self.extra.clone();
        let mut mods = Table::new();
        for (id, m) in &self.mods {
            let mut t = m.settings.clone();
            match m.enabled {
                Some(b) => {
                    t.insert("enabled".into(), Value::Boolean(b));
                }
                None => {
                    t.remove("enabled");
                }
            }
            mods.insert(id.clone(), Value::Table(t));
        }
        if !mods.is_empty() {
            root.insert("mods".into(), Value::Table(mods));
        }
        // A `Table` of TOML values always serializes.
        toml::to_string(&root).unwrap_or_default()
    }

    /// Reads `path`; a missing file is an empty config.
    pub fn load(path: &Path) -> Result<Config, String> {
        match fs::read_to_string(path) {
            Ok(text) => Config::parse(&text),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    /// Writes the config atomically (see [`write_atomic`]).
    pub fn save(&self, path: &Path) -> io::Result<()> {
        write_atomic(path, self.to_toml().as_bytes())
    }

    /// Whether mod `id` is on: the user's choice, else `default` (the
    /// manifest's).
    pub fn is_enabled(&self, id: &str, default: bool) -> bool {
        self.mods.get(id).and_then(|m| m.enabled).unwrap_or(default)
    }

    pub fn set_enabled(&mut self, id: &str, enabled: bool) {
        self.mods.entry(id.to_owned()).or_default().enabled = Some(enabled);
    }

    /// Sets one setting of mod `id`. `enabled` is not a setting; use
    /// [`Config::set_enabled`].
    pub fn set_setting(&mut self, id: &str, key: &str, value: Value) {
        if key == "enabled" {
            return;
        }
        self.mods
            .entry(id.to_owned())
            .or_default()
            .settings
            .insert(key.to_owned(), value);
    }

    /// Mod `id`'s effective settings: `defaults` (from the manifest) overlaid
    /// with the user's values. See [`effective_settings`].
    pub fn settings_for(&self, id: &str, defaults: &Table) -> Table {
        match self.mods.get(id) {
            Some(m) => effective_settings(defaults, &m.settings),
            None => defaults.clone(),
        }
    }
}

/// `defaults` overlaid with `user`. A user value replaces the default only if
/// it has the same TOML type (an integer is accepted where the default is a
/// float, as a float); otherwise the default stands, so a hand-edited typo
/// can't hand a mod a value of the wrong type. User keys without a default are
/// passed through (the mod ignores what it doesn't know). Nested tables are
/// merged key by key.
pub fn effective_settings(defaults: &Table, user: &Table) -> Table {
    let mut out = defaults.clone();
    for (k, u) in user {
        match out.get_mut(k) {
            None => {
                out.insert(k.clone(), u.clone());
            }
            Some(d) => {
                if let Some(v) = overlay(d, u) {
                    *d = v;
                }
            }
        }
    }
    out
}

fn overlay(default: &Value, user: &Value) -> Option<Value> {
    match (default, user) {
        (Value::Table(d), Value::Table(u)) => Some(Value::Table(effective_settings(d, u))),
        (Value::Float(_), Value::Integer(i)) => Some(Value::Float(*i as f64)),
        (d, u) if std::mem::discriminant(d) == std::mem::discriminant(u) => Some(u.clone()),
        _ => None,
    }
}

/// Writes `bytes` to `path` so that readers see either the old file or the
/// new one, never a partial write: a temp file in the same folder, flushed to
/// disk, then renamed over `path` (`MoveFileExW` with replace on Windows).
/// Creates the parent folder. The temp file name is unique per process and
/// call, so the manager and the runtime can write at the same time (the last
/// rename wins).
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    static SEQ: AtomicU32 = AtomicU32::new(0);
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(dir)?;
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no file name"))?;
    let mut tmp_name = std::ffi::OsString::from(".");
    tmp_name.push(name);
    tmp_name.push(format!(
        ".{}-{}.tmp",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let tmp = dir.join(tmp_name);
    let result = (|| {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// A file's change stamp: modification time and length. `None` = missing.
type Stamp = Option<(Option<SystemTime>, u64)>;

fn stamp(path: &Path) -> Stamp {
    let m = fs::metadata(path).ok()?;
    Some((m.modified().ok(), m.len()))
}

/// Watches the config file for changes (mtime and length), for reload on
/// change. One `poll` is a single `stat`, cheap enough to call every second.
pub struct Watcher {
    pub path: PathBuf,
    seen: Stamp,
}

impl Watcher {
    /// Starts watching; the file as it is now counts as seen, so load it
    /// yourself first ([`Config::load`]).
    pub fn new(path: PathBuf) -> Watcher {
        let seen = stamp(&path);
        Watcher { path, seen }
    }

    /// Returns the new config if the file changed since the last call. A
    /// deleted file reads as an empty config. A file that doesn't parse is
    /// logged and skipped (it's reported again once it changes).
    pub fn poll(&mut self) -> Option<Config> {
        match self.poll_result()? {
            Ok(c) => Some(c),
            Err(e) => {
                crate::log(&format!("config not reloaded: {e}"));
                None
            }
        }
    }

    /// Like [`Watcher::poll`], but returns the parse error instead of logging it.
    pub fn poll_result(&mut self) -> Option<Result<Config, String>> {
        let now = stamp(&self.path);
        if now == self.seen {
            return None;
        }
        self.seen = now;
        Some(Config::load(&self.path))
    }

    /// Marks the file as it is now as seen, e.g. right after the runtime saved
    /// it itself, so its own write isn't reported as a change.
    pub fn sync(&mut self) {
        self.seen = stamp(&self.path);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A fresh, empty folder under the system temp dir (not removed after).
    pub(crate) fn temp_dir(tag: &str) -> PathBuf {
        static N: AtomicU32 = AtomicU32::new(0);
        let t = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let d = std::env::temp_dir().join(format!(
            "uk-qol-test-{tag}-{}-{t}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&d).unwrap();
        d
    }

    const SAMPLE: &str = r#"
future_key = "kept"

[ui]
lang = "de"

[mods.skip-logos]
enabled = false

[mods.bloodmage-cost]
hotkey = "B"
volume = 0.5
nested = { a = 1 }

[mods.empty]
"#;

    #[test]
    fn parse_and_round_trip() {
        let c = Config::parse(SAMPLE).unwrap();
        assert_eq!(c.mods.len(), 3);
        assert_eq!(c.mods["skip-logos"].enabled, Some(false));
        assert!(c.mods["skip-logos"].settings.is_empty());
        let bm = &c.mods["bloodmage-cost"];
        assert_eq!(bm.enabled, None);
        assert_eq!(bm.settings["hotkey"].as_str(), Some("B"));
        assert_eq!(c.mods["empty"], ModConfig::default());
        assert_eq!(c.extra["future_key"].as_str(), Some("kept"));
        assert_eq!(c.extra["ui"]["lang"].as_str(), Some("de"));

        let text = c.to_toml();
        let again = Config::parse(&text).unwrap();
        assert_eq!(again, c, "{text}");
        assert!(text.contains("[mods.skip-logos]"), "{text}");
        assert!(text.contains("[mods.empty]"), "{text}");
    }

    #[test]
    fn empty_and_errors() {
        assert_eq!(Config::parse("").unwrap(), Config::default());
        assert_eq!(Config::default().to_toml(), "");
        assert!(Config::parse("mods = 1").is_err());
        assert!(Config::parse("[mods]\nx = 1").is_err());
        assert!(Config::parse("[mods.x]\nenabled = \"yes\"").is_err());
        assert!(Config::parse("[mods.x").is_err());
    }

    #[test]
    fn enabled_and_settings() {
        let mut c = Config::default();
        assert!(c.is_enabled("a", true));
        assert!(!c.is_enabled("a", false));
        c.set_enabled("a", false);
        assert!(!c.is_enabled("a", true));
        c.set_setting("a", "enabled", Value::Boolean(true));
        assert!(!c.is_enabled("a", true));
        c.set_setting("a", "speed", Value::Integer(3));
        let back = Config::parse(&c.to_toml()).unwrap();
        assert_eq!(back, c);
    }

    #[test]
    fn effective_overlay() {
        let defaults: Table = r#"
            on = true
            speed = 1.5
            name = "x"
            sub = { a = 1, b = 2 }
        "#
        .parse()
        .unwrap();
        let user: Table = r#"
            on = false
            speed = 2
            name = 5
            extra = "kept"
            sub = { b = 3 }
        "#
        .parse()
        .unwrap();
        let e = effective_settings(&defaults, &user);
        assert_eq!(e["on"].as_bool(), Some(false));
        assert_eq!(e["speed"].as_float(), Some(2.0));
        assert_eq!(
            e["name"].as_str(),
            Some("x"),
            "wrong type keeps the default"
        );
        assert_eq!(e["extra"].as_str(), Some("kept"));
        assert_eq!(e["sub"]["a"].as_integer(), Some(1));
        assert_eq!(e["sub"]["b"].as_integer(), Some(3));

        let mut c = Config::default();
        assert_eq!(c.settings_for("m", &defaults), defaults);
        c.set_setting("m", "on", Value::Boolean(false));
        assert_eq!(c.settings_for("m", &defaults)["on"].as_bool(), Some(false));
    }

    #[test]
    fn save_load_and_watch() {
        let dir = temp_dir("config");
        let path = config_path(&dir.join("sub"));
        assert_eq!(Config::load(&path).unwrap(), Config::default());

        let mut w = Watcher::new(path.clone());
        assert!(w.poll().is_none());

        let c = Config::parse(SAMPLE).unwrap();
        c.save(&path).unwrap();
        assert_eq!(Config::load(&path).unwrap(), c);
        assert_eq!(w.poll(), Some(c.clone()));
        assert!(w.poll().is_none());

        // Different length, so the change is seen even within one mtime tick.
        let mut c2 = c.clone();
        c2.set_enabled("skip-logos", true);
        c2.set_setting("new", "k", Value::Integer(1));
        c2.save(&path).unwrap();
        w.sync();
        assert!(w.poll().is_none(), "own save after sync is not a change");

        fs::write(&path, "[mods.x\n").unwrap();
        assert!(matches!(w.poll_result(), Some(Err(_))));
        assert!(w.poll_result().is_none());

        // No temp files left behind.
        let names: Vec<_> = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from(FILE_NAME)]);
    }
}
