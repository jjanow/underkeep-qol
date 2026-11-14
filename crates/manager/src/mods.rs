//! The built-in mods, from their `mod.toml` manifests (embedded at build time
//! by `build.rs`), and the compatibility check of their byte patterns against
//! the installed game.
//!
//! Manifests and pattern files are read with the runtime's own parsers
//! (`uk_qol_runtime::host`), so the manager and the game agree on them. A
//! manifest the runtime rejects is still read leniently, so a mod shows up
//! (with its settings) rather than vanishing from the list.

use std::path::Path;

use toml::{Table, Value};
use uk_qol_runtime::host::{self, Builds, Manifest, PatternFile, SettingDef};
use uk_qol_runtime::pattern::Image;

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
    /// The runtime's definition, when the manifest passed its parser; used
    /// for the final check of a new value.
    pub def: Option<SettingDef>,
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
    /// `game_builds` from the manifest; `None` if the runtime's parser
    /// rejected the manifest.
    pub game_builds: Option<Builds>,
    /// Why the runtime's parser rejected the manifest.
    pub manifest_error: Option<String>,
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
        def: None,
    }
}

fn setting_from_def(d: &SettingDef) -> SettingInfo {
    use host::SettingKind as K;
    let (kind, min, max) = match &d.kind {
        K::Bool => (SettingKind::Bool, None, None),
        K::Int { min, max } => (
            SettingKind::Int,
            min.map(|m| m as f64),
            max.map(|m| m as f64),
        ),
        K::Float { min, max } => (SettingKind::Float, *min, *max),
        K::String => (SettingKind::Text, None, None),
        K::Enum { choices } => (SettingKind::Choice(choices.clone()), None, None),
        K::Key => (SettingKind::Key, None, None),
    };
    SettingInfo {
        key: d.key.clone(),
        kind,
        default: Some(d.default.clone()),
        label: d.label.clone(),
        description: d.description.clone(),
        min,
        max,
        def: Some(d.clone()),
    }
}

fn from_manifest(m: &Manifest) -> ModInfo {
    ModInfo {
        id: m.id.clone(),
        name: m.name.clone(),
        version: m.version.clone(),
        description: m.description.clone(),
        enabled_default: m.enabled,
        settings: m.settings.iter().map(setting_from_def).collect(),
        patterns: String::new(),
        game_builds: Some(m.game_builds.clone()),
        manifest_error: None,
    }
}

/// Parses a `mod.toml` with the runtime's parser, falling back to a lenient
/// read. `fallback_id` (the folder name) is used if the manifest has no
/// `id`. Returns `None` only if the text isn't TOML.
pub fn parse_manifest(text: &str, fallback_id: &str) -> Option<ModInfo> {
    let strict_err = match Manifest::parse(text) {
        Ok(m) => return Some(from_manifest(&m)),
        Err(e) => e,
    };
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
        game_builds: None,
        manifest_error: Some(strict_err),
    })
}

impl ModInfo {
    /// Whether the manifest lists this build as tested (`None`: unknown).
    pub fn tested_on(&self, sha256: Option<&str>) -> Option<bool> {
        match self.game_builds.as_ref()? {
            Builds::Any => Some(true),
            Builds::Tested(list) => Some(sha256.is_some_and(|h| list.iter().any(|t| t == h))),
        }
    }
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

/// Checks `patterns_text` (a `patterns.toml`) against a mapped game image,
/// the way the runtime resolves it at start: every required pattern must
/// resolve; optional ones may miss. `build` is the exe's SHA-256 (hex), for
/// per-build candidates.
pub fn check_patterns(image: &Image, patterns_text: &str, build: Option<&str>) -> Compat {
    let pf = match PatternFile::parse(patterns_text) {
        Ok(p) => p,
        Err(e) => return Compat::Unknown(e),
    };
    let failed: Vec<(String, String)> = pf
        .patterns
        .iter()
        .filter(|d| !d.optional)
        .filter_map(|d| {
            host::resolve_pattern(image, d, build)
                .err()
                .map(|e| (d.name.clone(), e))
        })
        .collect();
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
/// Like the runtime, an override that doesn't parse is ignored.
pub fn effective_patterns(m: &ModInfo, game_dir: &Path) -> String {
    let dir = game_dir.join(crate::install::DATA_DIR).join("patterns");
    match host::read_override(&dir, &m.id) {
        Some(Ok(text)) if PatternFile::parse(&text).is_ok() => text,
        _ => m.patterns.clone(),
    }
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
    fn strict_manifest_uses_runtime_parser() {
        let text = r#"
id = "weapon-swap"
name = "Weapon swap"
version = "0.1.0"
game_builds = "any"
enabled = false

[[settings]]
key = "slots"
type = "int"
default = 3
min = 1
max = 5
label = "Extra slots"

[[settings]]
key = "cost"
type = "enum"
choices = ["bones", "health"]
default = "bones"
label = "Spell cost"
"#;
        let m = parse_manifest(text, "x").unwrap();
        assert!(m.manifest_error.is_none());
        assert!(!m.enabled_default);
        assert_eq!(m.tested_on(None), Some(true));
        let slots = m.setting("slots").unwrap();
        assert_eq!(
            (slots.kind.clone(), slots.min, slots.max),
            (SettingKind::Int, Some(1.0), Some(5.0))
        );
        assert!(slots.def.is_some());
        assert_eq!(
            m.setting("cost").unwrap().kind,
            SettingKind::Choice(vec!["bones".into(), "health".into()])
        );
    }

    #[test]
    fn checks_patterns_like_the_runtime() {
        // A PE with one executable section holding a few known bytes.
        let dll = crate::install::tests::fake_dll(0xC3);
        let img = uk_qol_runtime::pe::map_file(&dll).unwrap();
        let image = Image::new(&img).unwrap();
        assert_eq!(
            check_patterns(&image, "# comments only\n", None),
            Compat::Works
        );
        let ok = "[ret]\npattern = \"C3\"\n";
        assert_eq!(check_patterns(&image, ok, None), Compat::Works);
        let missing =
            "[nope]\npattern = \"DE AD BE EF\"\n[opt]\npattern = \"AA BB\"\noptional = true\n";
        match check_patterns(&image, missing, None) {
            Compat::Broken(f) => assert_eq!(f.len(), 1),
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            check_patterns(&image, "[x]\nbogus = 1\n", None),
            Compat::Unknown(_)
        ));
    }
}
