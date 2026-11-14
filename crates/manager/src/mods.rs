//! The built-in mods, from their `mod.toml` manifests (embedded at build time
//! by `build.rs`), and the compatibility check of their byte patterns against
//! the installed game.
//!
//! Manifests are read leniently: unknown fields are ignored, missing ones get
//! defaults, so the manager keeps working while the format evolves.

use std::path::Path;

use toml::{Table, Value};
use uk_qol_runtime::pattern::{Image, Pattern};

/// One built-in mod's files as embedded by `build.rs`.
pub struct Embedded {
    pub folder: &'static str,
    pub manifest: &'static str,
    pub patterns: &'static str,
}

include!(concat!(env!("OUT_DIR"), "/builtin_mods.rs"));

#[derive(Debug, Clone, PartialEq)]
pub enum SettingKind {
    Bool,
    Int,
    Float,
    Text,
    /// One of a fixed list.
    Choice(Vec<String>),
    /// A key binding, stored as text (`"Shift+1"`).
    Key,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SettingInfo {
    pub key: String,
    pub kind: SettingKind,
    pub default: Option<Value>,
    pub label: String,
    pub description: String,
    pub min: Option<f64>,
    pub max: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ModInfo {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub enabled_default: bool,
    pub settings: Vec<SettingInfo>,
    /// The mod's built-in `patterns.toml` text ("" if none).
    pub patterns: String,
}

impl ModInfo {
    pub fn setting(&self, key: &str) -> Option<&SettingInfo> {
        self.settings.iter().find(|s| s.key == key)
    }
}

fn str_of(t: &Table, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|k| t.get(*k).and_then(Value::as_str))
        .map(str::to_string)
}

fn num_of(t: &Table, key: &str) -> Option<f64> {
    match t.get(key)? {
        Value::Integer(i) => Some(*i as f64),
        Value::Float(f) => Some(*f),
        _ => None,
    }
}

fn kind_of(t: &Table, default: Option<&Value>) -> SettingKind {
    let options: Option<Vec<String>> = ["options", "choices", "values"].iter().find_map(|k| {
        t.get(*k).and_then(Value::as_array).map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
    });
    let ty = str_of(t, &["type", "kind"]).map(|s| s.to_ascii_lowercase());
    match ty.as_deref() {
        Some("bool" | "boolean" | "toggle") => SettingKind::Bool,
        Some("int" | "integer") => SettingKind::Int,
        Some("float" | "number" | "f32" | "f64") => SettingKind::Float,
        Some("key" | "hotkey" | "keybind" | "binding") => SettingKind::Key,
        Some("choice" | "enum" | "select") => SettingKind::Choice(options.unwrap_or_default()),
        Some(_) => SettingKind::Text,
        None => match (options, default) {
            (Some(o), _) => SettingKind::Choice(o),
            (None, Some(Value::Boolean(_))) => SettingKind::Bool,
            (None, Some(Value::Integer(_))) => SettingKind::Int,
            (None, Some(Value::Float(_))) => SettingKind::Float,
            _ => SettingKind::Text,
        },
    }
}

fn setting_from(key: &str, t: &Table) -> SettingInfo {
    let default = t.get("default").cloned();
    SettingInfo {
        key: key.to_string(),
        kind: kind_of(t, default.as_ref()),
        label: str_of(t, &["label", "name"]).unwrap_or_else(|| key.to_string()),
        description: str_of(t, &["description", "help"]).unwrap_or_default(),
        min: num_of(t, "min"),
        max: num_of(t, "max"),
        default,
    }
}

/// Parses a `mod.toml`. `fallback_id` (the folder name) is used if the
/// manifest has no `id`. Returns `None` only if the text isn't TOML.
pub fn parse_manifest(text: &str, fallback_id: &str) -> Option<ModInfo> {
    let root: Table = text.parse().ok()?;
    // Accept the fields at the top level or under `[mod]`.
    let m = root.get("mod").and_then(Value::as_table).unwrap_or(&root);
    let id = str_of(m, &["id"]).unwrap_or_else(|| fallback_id.to_string());
    let enabled_default = ["enabled", "enabled_by_default", "default_enabled"]
        .iter()
        .find_map(|k| m.get(*k).and_then(Value::as_bool))
        .unwrap_or(true);
    let mut settings = Vec::new();
    match root.get("settings") {
        Some(Value::Table(t)) => {
            for (k, v) in t {
                if let Some(st) = v.as_table() {
                    settings.push(setting_from(k, st));
                }
            }
        }
        Some(Value::Array(a)) => {
            for st in a.iter().filter_map(Value::as_table) {
                if let Some(k) = str_of(st, &["key", "id", "name"]) {
                    settings.push(setting_from(&k, st));
                }
            }
        }
        _ => {}
    }
    Some(ModInfo {
        name: str_of(m, &["name"]).unwrap_or_else(|| id.clone()),
        version: str_of(m, &["version"]).unwrap_or_default(),
        description: str_of(m, &["description"]).unwrap_or_default(),
        id,
        enabled_default,
        settings,
        patterns: String::new(),
    })
}

/// All built-in mods, sorted by id.
pub fn builtin() -> Vec<ModInfo> {
    let mut out: Vec<ModInfo> = BUILTIN
        .iter()
        .filter_map(|e| {
            let mut m = parse_manifest(e.manifest, e.folder)?;
            m.patterns = e.patterns.to_string();
            Some(m)
        })
        .collect();
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

pub fn find<'a>(mods: &'a [ModInfo], id: &str) -> Option<&'a ModInfo> {
    mods.iter()
        .find(|m| m.id == id || m.id.eq_ignore_ascii_case(id))
}

/// A named pattern from a `patterns.toml`.
#[derive(Debug, Clone, PartialEq)]
pub struct NamedPattern {
    pub name: String,
    pub pattern: String,
    /// Restrict the search to this section (default: all executable ones).
    pub section: Option<String>,
}

/// Reads a `patterns.toml` leniently: `name = "48 8B ..."` at the top level,
/// or `[name]` / `[patterns.name]` tables with a `pattern` key (and optional
/// `section`). Tables keyed by build are searched the same way.
pub fn parse_patterns(text: &str) -> Result<Vec<NamedPattern>, String> {
    let root: Table = text.parse().map_err(|e: toml::de::Error| e.to_string())?;
    let mut out = Vec::new();
    collect_patterns(&root, "", &mut out);
    Ok(out)
}

fn collect_patterns(t: &Table, prefix: &str, out: &mut Vec<NamedPattern>) {
    let join = |k: &str| {
        if prefix.is_empty() || prefix == "patterns" {
            k.to_string()
        } else {
            format!("{prefix}.{k}")
        }
    };
    for (k, v) in t {
        match v {
            Value::String(s) if k != "section" && k != "description" => out.push(NamedPattern {
                name: join(k),
                pattern: s.clone(),
                section: None,
            }),
            Value::Table(sub) => {
                if let Some(p) = sub.get("pattern").and_then(Value::as_str) {
                    out.push(NamedPattern {
                        name: join(k),
                        pattern: p.to_string(),
                        section: sub.get("section").and_then(Value::as_str).map(String::from),
                    });
                } else {
                    collect_patterns(sub, &join(k), out);
                }
            }
            _ => {}
        }
    }
}

/// Whether a mod's patterns all match exactly once in the game exe.
#[derive(Debug, Clone, PartialEq)]
pub enum Compat {
    /// Every pattern found exactly once (or the mod needs none).
    Works,
    /// These patterns failed, with the reason.
    Broken(Vec<(String, String)>),
    /// Couldn't check (exe unreadable, bad patterns file).
    Unknown(String),
}

/// Checks `patterns_text` against a mapped game image.
pub fn check_patterns(image: &Image, patterns_text: &str) -> Compat {
    if patterns_text.trim().is_empty() {
        return Compat::Works;
    }
    let pats = match parse_patterns(patterns_text) {
        Ok(p) => p,
        Err(e) => return Compat::Unknown(e),
    };
    let mut failed = Vec::new();
    for np in pats {
        let pat: Pattern = match np.pattern.parse() {
            Ok(p) => p,
            Err(e) => {
                failed.push((np.name, format!("{e}")));
                continue;
            }
        };
        let res = match &np.section {
            Some(s) => image.scan_section(s, &pat),
            None => image.scan(&pat),
        };
        if let Err(e) = res {
            failed.push((np.name, e.to_string()));
        }
    }
    if failed.is_empty() {
        Compat::Works
    } else {
        Compat::Broken(failed)
    }
}

/// Reads and maps the game exe once for [`check_patterns`].
pub fn load_image(exe: &Path) -> Result<Vec<u8>, String> {
    let file = std::fs::read(exe).map_err(|e| e.to_string())?;
    uk_qol_runtime::pe::map_file(&file).map_err(|e| e.to_string())
}

/// The patterns text in effect for `m` in `game_dir`: a downloaded
/// `uk_qol/patterns/<id>.toml` overrides the built-in one.
pub fn effective_patterns(m: &ModInfo, game_dir: &Path) -> String {
    std::fs::read_to_string(
        game_dir
            .join(crate::install::DATA_DIR)
            .join("patterns")
            .join(format!("{}.toml", m.id)),
    )
    .unwrap_or_else(|_| m.patterns.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST: &str = r#"
id = "weapon-swap"
name = "Weapon swap"
version = "0.1.0"
description = "A ranged slot."
enabled = false
future_field = { anything = 1 }

[settings.swap_costs_turn]
type = "bool"
default = true
label = "Swap costs a turn"

[settings.mode]
options = ["a", "b"]
default = "a"

[settings.count]
default = 3
min = 1
max = 5

[settings.hotkey]
type = "key"
default = "Shift+1"
"#;

    #[test]
    fn parses_manifest_leniently() {
        let m = parse_manifest(MANIFEST, "folder").unwrap();
        assert_eq!(m.id, "weapon-swap");
        assert_eq!(m.name, "Weapon swap");
        assert!(!m.enabled_default);
        assert_eq!(
            m.setting("swap_costs_turn").unwrap().kind,
            SettingKind::Bool
        );
        assert_eq!(
            m.setting("swap_costs_turn").unwrap().label,
            "Swap costs a turn"
        );
        assert_eq!(
            m.setting("mode").unwrap().kind,
            SettingKind::Choice(vec!["a".into(), "b".into()])
        );
        let c = m.setting("count").unwrap();
        assert_eq!(
            (c.kind.clone(), c.min, c.max),
            (SettingKind::Int, Some(1.0), Some(5.0))
        );
        assert_eq!(m.setting("hotkey").unwrap().kind, SettingKind::Key);

        let minimal = parse_manifest("name = \"X\"", "skip-logos").unwrap();
        assert_eq!(minimal.id, "skip-logos");
        assert!(minimal.enabled_default);
        assert!(minimal.settings.is_empty());

        let nested = parse_manifest(
            "[mod]\nid = \"a\"\n[[settings]]\nkey = \"k\"\ndefault = 1.5",
            "f",
        )
        .unwrap();
        assert_eq!(nested.id, "a");
        assert_eq!(nested.setting("k").unwrap().kind, SettingKind::Float);

        assert!(parse_manifest("not = [toml", "f").is_none());
    }

    #[test]
    fn builtin_manifests_parse() {
        // Whatever is in mods/ must parse (an empty list is fine).
        assert_eq!(builtin().len(), BUILTIN.len());
    }

    #[test]
    fn parses_pattern_files() {
        let text = r#"
top = "48 8B 05 ?? ?? ?? ??"
[draw_logo]
pattern = "E8 ?? ?? ?? ?? 90"
section = ".text"
[patterns.other]
pattern = "C3"
"#;
        let p = parse_patterns(text).unwrap();
        let names: Vec<_> = p.iter().map(|n| n.name.as_str()).collect();
        assert!(names.contains(&"top"));
        assert!(names.contains(&"draw_logo"));
        assert!(names.contains(&"other"));
        let d = p.iter().find(|n| n.name == "draw_logo").unwrap();
        assert_eq!(d.section.as_deref(), Some(".text"));
    }
}
