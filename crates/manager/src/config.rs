//! Editing `uk_qol/config.toml`, the settings file shared with the runtime
//! and the in-game overlay:
//!
//! ```toml
//! [mods.skip-logos]
//! enabled = true
//! some_setting = 3
//! ```
//!
//! Edits keep everything else in the file (comments, unknown keys, other
//! mods' tables, formatting): only the touched value changes.

use std::path::{Path, PathBuf};

use toml_edit::{DocumentMut, Item, Table, Value};

use crate::mods::{ModInfo, SettingInfo, SettingKind};
use crate::tr;

pub const FILE: &str = "config.toml";

pub fn path(game_dir: &Path) -> PathBuf {
    game_dir.join(crate::install::DATA_DIR).join(FILE)
}

pub struct ConfigFile {
    pub path: PathBuf,
    doc: DocumentMut,
}

impl ConfigFile {
    /// Reads the file; a missing file is an empty config.
    pub fn load(path: &Path) -> Result<ConfigFile, String> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(tr!("err_read", path.display(), e)),
        };
        Self::parse(path, &text)
    }

    pub fn parse(path: &Path, text: &str) -> Result<ConfigFile, String> {
        let doc: DocumentMut = text
            .parse()
            .map_err(|e: toml_edit::TomlError| tr!("err_config_parse", path.display(), e))?;
        Ok(ConfigFile {
            path: path.to_path_buf(),
            doc,
        })
    }

    pub fn text(&self) -> String {
        self.doc.to_string()
    }

    fn mod_table(&self, id: &str) -> Option<&Table> {
        self.doc.get("mods")?.as_table()?.get(id)?.as_table()
    }

    fn mod_table_mut(&mut self, id: &str) -> Result<&mut Table, String> {
        let mods = self
            .doc
            .entry("mods")
            .or_insert_with(|| {
                let mut t = Table::new();
                t.set_implicit(true);
                Item::Table(t)
            })
            .as_table_mut()
            .ok_or_else(|| tr!("err_config_shape", "mods"))?;
        mods.entry(id)
            .or_insert_with(|| Item::Table(Table::new()))
            .as_table_mut()
            .ok_or_else(|| tr!("err_config_shape", format!("mods.{id}")))
    }

    /// The user's `enabled` value, if set.
    pub fn enabled(&self, id: &str) -> Option<bool> {
        self.mod_table(id)?.get("enabled")?.as_bool()
    }

    /// Whether the mod is on: the user's value, else the manifest default.
    pub fn is_enabled(&self, m: &ModInfo) -> bool {
        self.enabled(&m.id).unwrap_or(m.enabled_default)
    }

    pub fn set_enabled(&mut self, id: &str, on: bool) -> Result<(), String> {
        self.set(id, "enabled", on.into())
    }

    /// The user's value for a setting, if set.
    pub fn get(&self, id: &str, key: &str) -> Option<&Value> {
        self.mod_table(id)?.get(key)?.as_value()
    }

    pub fn set(&mut self, id: &str, key: &str, v: Value) -> Result<(), String> {
        let t = self.mod_table_mut(id)?;
        // Keep the decor (comments) of an existing value.
        match t.get_mut(key).and_then(Item::as_value_mut) {
            Some(old) => {
                let decor = old.decor().clone();
                *old = v;
                *old.decor_mut() = decor;
            }
            None => t[key] = Item::Value(v),
        }
        Ok(())
    }

    /// Removes a user value so the manifest default applies again.
    pub fn reset(&mut self, id: &str, key: &str) {
        if let Some(t) = self
            .doc
            .get_mut("mods")
            .and_then(Item::as_table_mut)
            .and_then(|m| m.get_mut(id))
            .and_then(Item::as_table_mut)
        {
            t.remove(key);
        }
    }

    /// Writes the file atomically (temporary file, then rename), creating
    /// `uk_qol/` if needed.
    pub fn save(&self) -> Result<(), String> {
        crate::install::write_atomic(&self.path, self.doc.to_string().as_bytes())
    }
}

/// Turns command-line text into a value of the setting's type, checked by
/// the runtime's rules (`SettingDef::check`) when the manifest has them.
pub fn coerce(s: &SettingInfo, raw: &str) -> Result<Value, String> {
    let v = coerce_kind(s, raw)?;
    let Some(def) = &s.def else { return Ok(v) };
    let tv: toml::Table = format!("v = {v}")
        .parse()
        .map_err(|e: toml::de::Error| e.to_string())?;
    let checked = def
        .check(&tv["v"])
        .map_err(|e| tr!("err_bad_setting", s.key, e))?;
    let mut t = toml::Table::new();
    t.insert("v".into(), checked);
    let doc: toml_edit::DocumentMut = toml::to_string(&t)
        .map_err(|e| e.to_string())?
        .parse()
        .map_err(|e: toml_edit::TomlError| e.to_string())?;
    doc["v"].as_value().cloned().ok_or_else(|| "?".into())
}

fn coerce_kind(s: &SettingInfo, raw: &str) -> Result<Value, String> {
    let bad = || tr!("err_bad_value", raw, s.key, kind_name(&s.kind));
    let in_range = |x: f64| s.min.is_none_or(|m| x >= m) && s.max.is_none_or(|m| x <= m);
    let range_err = || {
        tr!(
            "err_out_of_range",
            raw,
            s.min.map_or("-".into(), |m| m.to_string()),
            s.max.map_or("-".into(), |m| m.to_string())
        )
    };
    match &s.kind {
        SettingKind::Bool => match raw.to_ascii_lowercase().as_str() {
            "true" | "on" | "yes" | "1" => Ok(true.into()),
            "false" | "off" | "no" | "0" => Ok(false.into()),
            _ => Err(bad()),
        },
        SettingKind::Int => {
            let i: i64 = raw.trim().parse().map_err(|_| bad())?;
            if in_range(i as f64) {
                Ok(i.into())
            } else {
                Err(range_err())
            }
        }
        SettingKind::Float => {
            let f: f64 = raw.trim().parse().map_err(|_| bad())?;
            if f.is_finite() && in_range(f) {
                Ok(f.into())
            } else {
                Err(range_err())
            }
        }
        SettingKind::Choice(opts) => {
            if opts.is_empty() || opts.iter().any(|o| o == raw) {
                Ok(raw.into())
            } else {
                Err(tr!("err_bad_choice", raw, opts.join(", ")))
            }
        }
        SettingKind::Text | SettingKind::Key => Ok(raw.into()),
    }
}

pub fn kind_name(k: &SettingKind) -> String {
    match k {
        SettingKind::Bool => "true/false".into(),
        SettingKind::Int => tr!("kind_int"),
        SettingKind::Float => tr!("kind_float"),
        SettingKind::Text => tr!("kind_text"),
        SettingKind::Key => tr!("kind_key"),
        SettingKind::Choice(o) => o.join(" | "),
    }
}

/// A TOML value as shown to the user (`"text"` without quotes).
pub fn show_value(v: &Value) -> String {
    match v {
        Value::String(s) => s.value().clone(),
        other => other.to_string().trim().to_string(),
    }
}

/// Same for a `toml` (manifest) value.
pub fn show_toml(v: &toml::Value) -> String {
    match v {
        toml::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mods::parse_manifest;

    const ORIGINAL: &str = r#"# my settings
[runtime]
log_level = "debug" # keep me

[mods.skip-logos]
enabled = false # user choice
unknown_key = [1, 2]

[mods.other-mod]
x = 1
"#;

    #[test]
    fn edits_preserve_unknown_keys_and_comments() {
        let mut c = ConfigFile::parse(Path::new("config.toml"), ORIGINAL).unwrap();
        assert_eq!(c.enabled("skip-logos"), Some(false));
        c.set_enabled("skip-logos", true).unwrap();
        c.set("skip-logos", "speed", 2.into()).unwrap();
        c.set_enabled("new-mod", false).unwrap();
        let out = c.text();
        assert!(out.contains("# my settings"));
        assert!(out.contains("log_level = \"debug\" # keep me"));
        assert!(out.contains("unknown_key = [1, 2]"));
        assert!(out.contains("enabled = true # user choice"));
        assert!(out.contains("[mods.other-mod]\nx = 1"));
        assert!(out.contains("[mods.new-mod]\nenabled = false"));
        assert!(!out.contains("[mods]\n"));
        let again = ConfigFile::parse(Path::new("c"), &out).unwrap();
        assert_eq!(again.enabled("skip-logos"), Some(true));
        assert_eq!(
            again.get("skip-logos", "speed").and_then(Value::as_integer),
            Some(2)
        );
        let mut again = again;
        again.reset("skip-logos", "speed");
        assert!(again.get("skip-logos", "speed").is_none());
    }

    #[test]
    fn empty_file_gets_dotted_tables() {
        let mut c = ConfigFile::parse(Path::new("c"), "").unwrap();
        c.set_enabled("skip-logos", true).unwrap();
        assert_eq!(c.text(), "[mods.skip-logos]\nenabled = true\n");
    }

    #[test]
    fn save_and_load_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let p = path(tmp.path());
        let mut c = ConfigFile::load(&p).unwrap();
        c.set_enabled("a", false).unwrap();
        c.save().unwrap();
        assert_eq!(ConfigFile::load(&p).unwrap().enabled("a"), Some(false));
    }

    #[test]
    fn coerces_with_runtime_rules() {
        let m = parse_manifest(
            r#"
id = "m"
name = "M"
version = "0.1.0"
game_builds = "any"
[[settings]]
key = "hotkey"
type = "key"
default = "S"
label = "Key"
[[settings]]
key = "speed"
type = "float"
default = 1.0
min = 0.5
max = 2.0
label = "Speed"
"#,
            "m",
        )
        .unwrap();
        assert!(m.manifest_error.is_none());
        let s = |k| m.setting(k).unwrap();
        assert_eq!(
            coerce(s("hotkey"), "Shift+1").unwrap().as_str(),
            Some("Shift+1")
        );
        assert!(coerce(s("hotkey"), "Hyper+S").is_err());
        assert_eq!(coerce(s("speed"), "1.5").unwrap().as_float(), Some(1.5));
        assert!(coerce(s("speed"), "3").is_err());
    }

    #[test]
    fn coerces_by_setting_type() {
        let m = parse_manifest(
            r#"
[settings.b]
type = "bool"
[settings.n]
type = "int"
min = 1
max = 5
[settings.c]
options = ["x", "y"]
"#,
            "m",
        )
        .unwrap();
        let s = |k| m.setting(k).unwrap();
        assert_eq!(coerce(s("b"), "on").unwrap().as_bool(), Some(true));
        assert!(coerce(s("b"), "maybe").is_err());
        assert_eq!(coerce(s("n"), "3").unwrap().as_integer(), Some(3));
        assert!(coerce(s("n"), "9").is_err());
        assert!(coerce(s("n"), "x").is_err());
        assert_eq!(coerce(s("c"), "y").unwrap().as_str(), Some("y"));
        assert!(coerce(s("c"), "z").is_err());
    }
}
