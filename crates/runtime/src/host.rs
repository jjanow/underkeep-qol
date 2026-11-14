//! Mod host (PLAN §2): the `Mod` trait, mod manifests (`mod.toml`) and
//! patterns (`patterns.toml`), loading and unloading.
//!
//! Built-in mods are crates under `mods/`; the proxy passes them to
//! [`crate::start`]. At start the runtime builds a [`Host`] (manifests
//! parsed, every mod's patterns resolved against the unhooked exe, enabled
//! mods loaded in dependency order) and installs it as the global that the
//! overlay and the save hooks reach through [`with`].
//!
//! The file formats are documented in CONTRIBUTING.md ("Adding a mod").

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::Deserialize;

use crate::config::{self, Config, ModConfig};
use crate::guard;
use crate::hook::{HookError, HookSet};
use crate::input::InputEvent;
use crate::pattern::{Image, Pattern};
use crate::safe::SafeMode;

/// Everything a mod gets while loading or unloading.
pub struct LoadCtx<'a> {
    /// The game exe as mapped in memory.
    pub image: &'a Image<'a>,
    /// Base address of the exe in memory (RVA + base = address).
    pub base: usize,
    /// Each name in the mod's `patterns.toml`, resolved to an RVA.
    pub patterns: &'a BTreeMap<String, u32>,
    /// The mod's own hooks; enabled by the host after `on_load` returns Ok.
    pub hooks: &'a mut HookSet,
    /// Effective settings (manifest defaults overlaid with the user's).
    pub settings: &'a toml::Table,
}

/// Per-frame context (called from the game's render thread).
pub struct FrameCtx<'a> {
    pub settings: &'a toml::Table,
}

/// One mod's state in a save's sidecar, keyed by hero id.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ModSaveState {
    pub heroes: BTreeMap<String, toml::Table>,
    pub global: toml::Table,
}

pub trait Mod: Send {
    /// Must equal `id` in the mod's `mod.toml`.
    fn id(&self) -> &'static str;
    /// The mod's `mod.toml` text (built-in mods `include_str!` it).
    fn manifest(&self) -> &'static str;
    /// The mod's `patterns.toml` text; an empty string if it needs none.
    fn patterns(&self) -> &'static str;

    /// Registers hooks. Returning Err disables the mod with that reason.
    /// May run again after `on_unload` when the player turns the mod back on.
    fn on_load(&mut self, cx: &mut LoadCtx) -> Result<(), String>;
    /// Called after the mod's hooks are removed.
    fn on_unload(&mut self) {}
    fn on_frame(&mut self, _cx: &mut FrameCtx) {}
    /// Return true to swallow the event (the game doesn't see it).
    fn on_input(&mut self, _ev: &InputEvent) -> bool {
        false
    }
    /// Settings changed in the overlay or config file.
    fn on_settings(&mut self, _settings: &toml::Table) {}
    fn on_save(&mut self, _state: &mut ModSaveState) {}
    fn on_load_save(&mut self, _state: &ModSaveState) {}
}

/// Constructor of a built-in mod, as passed by the proxy.
pub type ModCtor = fn() -> Box<dyn Mod>;

// ---------------------------------------------------------------------------
// mod.toml

/// Which game builds a mod was tested on. Informational: on any other build
/// the mod still loads if all its required patterns resolve (README,
/// "Unknown builds"); the overlay and log say it's untested there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Builds {
    /// Doesn't depend on the exe (no patterns), works on every build.
    Any,
    /// Lowercase hex SHA-256 of each exe the mod was tested on.
    Tested(Vec<String>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum SettingKind {
    Bool,
    Int {
        min: Option<i64>,
        max: Option<i64>,
    },
    Float {
        min: Option<f64>,
        max: Option<f64>,
    },
    String,
    Enum {
        choices: Vec<String>,
    },
    /// A hotkey, see [`check_hotkey`].
    Key,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SettingDef {
    pub key: String,
    pub kind: SettingKind,
    pub default: toml::Value,
    pub label: String,
    pub description: String,
}

impl SettingDef {
    /// Checks `v` against the setting's type and limits. Returns the value as
    /// stored (an integer given for a float setting becomes a float).
    pub fn check(&self, v: &toml::Value) -> Result<toml::Value, String> {
        use toml::Value as V;
        let out = match (&self.kind, v) {
            (SettingKind::Bool, V::Boolean(_)) => v.clone(),
            (SettingKind::Int { min, max }, V::Integer(i)) => {
                if min.is_some_and(|m| *i < m) || max.is_some_and(|m| *i > m) {
                    return Err(format!("{i} is outside {}", range(min, max)));
                }
                v.clone()
            }
            (SettingKind::Float { min, max }, V::Float(_) | V::Integer(_)) => {
                let f = match v {
                    V::Float(f) => *f,
                    V::Integer(i) => *i as f64,
                    _ => unreachable!(),
                };
                if !f.is_finite() || min.is_some_and(|m| f < m) || max.is_some_and(|m| f > m) {
                    return Err(format!("{f} is outside {}", range(min, max)));
                }
                V::Float(f)
            }
            (SettingKind::String, V::String(_)) => v.clone(),
            (SettingKind::Enum { choices }, V::String(s)) => {
                if !choices.contains(s) {
                    return Err(format!("{s:?} is not one of {}", choices.join(", ")));
                }
                v.clone()
            }
            (SettingKind::Key, V::String(s)) => {
                check_hotkey(s)?;
                v.clone()
            }
            _ => return Err(format!("expected {}", self.kind.name())),
        };
        Ok(out)
    }
}

fn range<T: fmt::Display>(min: &Option<T>, max: &Option<T>) -> String {
    let show = |b: &Option<T>| b.as_ref().map_or(String::new(), |b| b.to_string());
    format!("{}..={}", show(min), show(max))
}

impl SettingKind {
    pub fn name(&self) -> &'static str {
        match self {
            SettingKind::Bool => "a bool",
            SettingKind::Int { .. } => "an integer",
            SettingKind::Float { .. } => "a number",
            SettingKind::String => "a string",
            SettingKind::Enum { .. } => "one of the choices",
            SettingKind::Key => "a hotkey",
        }
    }
}

/// Modifier names accepted in a hotkey, in canonical order.
pub const HOTKEY_MODIFIERS: [&str; 3] = ["Shift", "Ctrl", "Alt"];

/// Checks the hotkey grammar: `""` (unbound), `Pad:<Button>`, or
/// `[Shift+][Ctrl+][Alt+]<Key>` (modifiers in any order, case-insensitive,
/// each at most once). `<Key>` and `<Button>` are names of letters and
/// digits (`S`, `F10`, `PageUp`, `DpadLeft`); which names exist is up to
/// the hotkey system (PLAN §4), so they aren't checked here.
pub fn check_hotkey(s: &str) -> Result<(), String> {
    if s.is_empty() {
        return Ok(());
    }
    let name_ok = |n: &str| !n.is_empty() && n.chars().all(|c| c.is_ascii_alphanumeric());
    if let Some(button) = s.strip_prefix("Pad:") {
        return if name_ok(button) {
            Ok(())
        } else {
            Err(format!("bad gamepad button in hotkey {s:?}"))
        };
    }
    let mut parts: Vec<&str> = s.split('+').collect();
    let key = parts.pop().unwrap_or_default();
    let mut seen = BTreeSet::new();
    for m in parts {
        let Some(canon) = HOTKEY_MODIFIERS.iter().find(|c| c.eq_ignore_ascii_case(m)) else {
            return Err(format!("unknown modifier {m:?} in hotkey {s:?}"));
        };
        if !seen.insert(*canon) {
            return Err(format!("modifier {canon} twice in hotkey {s:?}"));
        }
    }
    if name_ok(key) {
        Ok(())
    } else {
        Err(format!("bad key name in hotkey {s:?}"))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub author: String,
    pub description: String,
    pub game_builds: Builds,
    /// Ids of mods that can't be active at the same time as this one.
    pub conflicts: Vec<String>,
    /// Ids of mods that must be active for this one to load.
    pub dependencies: Vec<String>,
    /// Whether the mod is on when the user hasn't said otherwise.
    pub enabled: bool,
    pub settings: Vec<SettingDef>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawManifest {
    id: String,
    name: String,
    version: String,
    #[serde(default)]
    author: String,
    #[serde(default)]
    description: String,
    game_builds: toml::Value,
    #[serde(default)]
    conflicts: Vec<String>,
    #[serde(default)]
    dependencies: Vec<String>,
    #[serde(default = "yes")]
    enabled: bool,
    #[serde(default)]
    settings: Vec<RawSetting>,
}

fn yes() -> bool {
    true
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSetting {
    key: String,
    #[serde(rename = "type")]
    ty: String,
    default: toml::Value,
    label: String,
    #[serde(default)]
    description: String,
    min: Option<toml::Value>,
    max: Option<toml::Value>,
    choices: Option<Vec<String>>,
}

/// `[a-z0-9]` words joined by single `-`.
pub fn valid_mod_id(id: &str) -> bool {
    !id.is_empty()
        && id.split('-').all(|w| {
            !w.is_empty()
                && w.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        })
}

fn valid_setting_key(k: &str) -> bool {
    k.bytes().next().is_some_and(|b| b.is_ascii_lowercase())
        && k.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

fn is_sha256_hex(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

impl Manifest {
    pub fn parse(text: &str) -> Result<Manifest, String> {
        let raw: RawManifest = toml::from_str(text).map_err(|e| e.to_string())?;
        if !valid_mod_id(&raw.id) {
            return Err(format!(
                "id {:?}: use lowercase letters, digits and single dashes",
                raw.id
            ));
        }
        let game_builds = match &raw.game_builds {
            toml::Value::String(s) if s == "any" => Builds::Any,
            toml::Value::Array(a) => Builds::Tested(
                a.iter()
                    .map(|v| match v.as_str() {
                        Some(s) if is_sha256_hex(s) => Ok(s.to_ascii_lowercase()),
                        _ => Err(format!("game_builds: {v} is not an exe SHA-256 in hex")),
                    })
                    .collect::<Result<_, _>>()?,
            ),
            _ => return Err("game_builds must be \"any\" or a list of exe SHA-256s".into()),
        };
        for d in raw.dependencies.iter().chain(&raw.conflicts) {
            if !valid_mod_id(d) || *d == raw.id {
                return Err(format!("bad mod id {d:?} in dependencies/conflicts"));
            }
        }
        let mut settings: Vec<SettingDef> = Vec::new();
        for s in raw.settings {
            let def = setting(s)?;
            if settings.iter().any(|o| o.key == def.key) {
                return Err(format!("setting {:?} declared twice", def.key));
            }
            settings.push(def);
        }
        Ok(Manifest {
            id: raw.id,
            name: raw.name,
            version: raw.version,
            author: raw.author,
            description: raw.description,
            game_builds,
            conflicts: raw.conflicts,
            dependencies: raw.dependencies,
            enabled: raw.enabled,
            settings,
        })
    }

    /// Whether the mod lists the build `sha256` (lowercase hex) as tested.
    /// `None` (unknown build) is untested unless `game_builds = "any"`.
    pub fn tested_on(&self, sha256: Option<&str>) -> bool {
        match &self.game_builds {
            Builds::Any => true,
            Builds::Tested(list) => sha256.is_some_and(|h| list.iter().any(|t| t == h)),
        }
    }

    pub fn setting(&self, key: &str) -> Option<&SettingDef> {
        self.settings.iter().find(|s| s.key == key)
    }

    /// The declared settings' defaults.
    pub fn defaults(&self) -> toml::Table {
        self.settings
            .iter()
            .map(|s| (s.key.clone(), s.default.clone()))
            .collect()
    }

    /// Defaults overlaid with the user's values ([`config::effective_settings`]:
    /// a value of the wrong TOML type keeps the default), then checked against
    /// the schema: a value out of range, not among an enum's choices or not a
    /// valid hotkey falls back to the default. Returns the reasons for any
    /// user value not used, to log. Keys the manifest doesn't declare pass
    /// through.
    pub fn effective_settings(&self, user: &toml::Table) -> (toml::Table, Vec<String>) {
        let mut out = config::effective_settings(&self.defaults(), user);
        let mut warnings = Vec::new();
        for s in &self.settings {
            let Some(u) = user.get(&s.key) else { continue };
            if let Err(e) = s.check(u) {
                warnings.push(format!("setting {}: {e}; using the default", s.key));
                out.insert(s.key.clone(), s.default.clone());
            }
        }
        (out, warnings)
    }
}

fn setting(s: RawSetting) -> Result<SettingDef, String> {
    let ctx = |m: String| format!("setting {:?}: {m}", s.key);
    if !valid_setting_key(&s.key) || s.key == "enabled" {
        return Err(ctx(
            "key must be lowercase letters, digits and `_`, and not `enabled`".into(),
        ));
    }
    let int = |v: &Option<toml::Value>| -> Result<Option<i64>, String> {
        match v {
            None => Ok(None),
            Some(toml::Value::Integer(i)) => Ok(Some(*i)),
            Some(_) => Err(ctx("min/max must be integers".into())),
        }
    };
    let float = |v: &Option<toml::Value>| -> Result<Option<f64>, String> {
        match v {
            None => Ok(None),
            Some(toml::Value::Float(f)) => Ok(Some(*f)),
            Some(toml::Value::Integer(i)) => Ok(Some(*i as f64)),
            Some(_) => Err(ctx("min/max must be numbers".into())),
        }
    };
    let kind = match s.ty.as_str() {
        "bool" => SettingKind::Bool,
        "int" => SettingKind::Int {
            min: int(&s.min)?,
            max: int(&s.max)?,
        },
        "float" => SettingKind::Float {
            min: float(&s.min)?,
            max: float(&s.max)?,
        },
        "string" => SettingKind::String,
        "enum" => {
            let choices = s.choices.clone().unwrap_or_default();
            if choices.is_empty() {
                return Err(ctx("enum needs `choices`".into()));
            }
            SettingKind::Enum { choices }
        }
        "key" => SettingKind::Key,
        t => {
            return Err(ctx(format!(
                "unknown type {t:?} (bool, int, float, string, enum, key)"
            )));
        }
    };
    let numeric = matches!(kind, SettingKind::Int { .. } | SettingKind::Float { .. });
    if !numeric && (s.min.is_some() || s.max.is_some()) {
        return Err(ctx("min/max only apply to int and float".into()));
    }
    if !matches!(kind, SettingKind::Enum { .. }) && s.choices.is_some() {
        return Err(ctx("choices only apply to enum".into()));
    }
    let mut def = SettingDef {
        key: s.key.clone(),
        kind,
        default: s.default.clone(),
        label: s.label,
        description: s.description,
    };
    def.default = def
        .check(&s.default)
        .map_err(|e| ctx(format!("default: {e}")))?;
    Ok(def)
}

// ---------------------------------------------------------------------------
// patterns.toml

/// What to do with a pattern's match address, after `offset` is added.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolve {
    /// The address itself.
    Address,
    /// The target of a RIP-relative operand of the instruction there.
    Rip { disp: u32, len: u32 },
    /// The target of the `call`/`jmp rel32` there.
    Call,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub pattern: Pattern,
    /// Scan only this section; `None` = all executable sections.
    pub section: Option<String>,
    /// Added to the match RVA before `resolve`.
    pub offset: i64,
    pub resolve: Resolve,
    /// Builds this candidate was written for (lowercase hex SHA-256).
    pub builds: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PatternDef {
    pub name: String,
    /// A missing optional pattern doesn't disable the mod.
    pub optional: bool,
    pub candidates: Vec<Candidate>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct PatternFile {
    pub patterns: Vec<PatternDef>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPatternDef {
    #[serde(default)]
    optional: bool,
    #[serde(flatten)]
    single: RawCandidateOpt,
    candidates: Option<Vec<RawCandidate>>,
}

#[derive(Deserialize, Default)]
struct RawCandidateOpt {
    pattern: Option<String>,
    section: Option<String>,
    offset: Option<i64>,
    rip: Option<RawRip>,
    call: Option<bool>,
    builds: Option<Vec<String>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCandidate {
    pattern: String,
    section: Option<String>,
    offset: Option<i64>,
    rip: Option<RawRip>,
    call: Option<bool>,
    builds: Option<Vec<String>>,
}

#[derive(Deserialize, Clone, Copy)]
#[serde(deny_unknown_fields)]
struct RawRip {
    disp: u32,
    len: u32,
}

fn candidate(c: RawCandidate) -> Result<Candidate, String> {
    let pattern: Pattern = c
        .pattern
        .parse()
        .map_err(|e| format!("pattern {:?}: {e}", c.pattern))?;
    let resolve = match (c.rip, c.call.unwrap_or(false)) {
        (Some(_), true) => return Err("`rip` and `call` can't both be set".into()),
        (Some(RawRip { disp, len }), false) => {
            if disp + 4 > len {
                return Err(format!("rip: disp {disp} + 4 bytes don't fit in len {len}"));
            }
            Resolve::Rip { disp, len }
        }
        (None, true) => Resolve::Call,
        (None, false) => Resolve::Address,
    };
    let builds = c.builds.unwrap_or_default();
    if let Some(b) = builds.iter().find(|b| !is_sha256_hex(b)) {
        return Err(format!("builds: {b:?} is not an exe SHA-256 in hex"));
    }
    Ok(Candidate {
        pattern,
        section: c.section,
        offset: c.offset.unwrap_or(0),
        resolve,
        builds: builds.iter().map(|b| b.to_ascii_lowercase()).collect(),
    })
}

impl PatternFile {
    pub fn parse(text: &str) -> Result<PatternFile, String> {
        let raw: BTreeMap<String, RawPatternDef> =
            toml::from_str(text).map_err(|e| e.to_string())?;
        let mut patterns = Vec::new();
        for (name, d) in raw {
            let ctx = |m: String| format!("pattern {name:?}: {m}");
            let s = d.single;
            let has_single = s.pattern.is_some()
                || s.section.is_some()
                || s.offset.is_some()
                || s.rip.is_some()
                || s.call.is_some()
                || s.builds.is_some();
            let raws = match (s.pattern, d.candidates) {
                (Some(_), Some(_)) => {
                    return Err(ctx("give `pattern` or `[[candidates]]`, not both".into()));
                }
                (None, Some(_)) if has_single => {
                    return Err(ctx("with `[[candidates]]`, put every field in them".into()));
                }
                (None, Some(c)) if c.is_empty() => return Err(ctx("no candidates".into())),
                (None, Some(c)) => c,
                (Some(pattern), None) => vec![RawCandidate {
                    pattern,
                    section: s.section,
                    offset: s.offset,
                    rip: s.rip,
                    call: s.call,
                    builds: s.builds,
                }],
                (None, None) => return Err(ctx("needs `pattern` or `[[candidates]]`".into())),
            };
            let candidates = raws
                .into_iter()
                .enumerate()
                .map(|(i, c)| candidate(c).map_err(|e| ctx(format!("candidate {}: {e}", i + 1))))
                .collect::<Result<_, _>>()?;
            patterns.push(PatternDef {
                name,
                optional: d.optional,
                candidates,
            });
        }
        Ok(PatternFile { patterns })
    }
}

/// Result of resolving one [`PatternDef`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub rva: u32,
    /// Index of the candidate that gave it (the first, if several agreed).
    pub candidate: usize,
}

fn resolve_candidate(img: &Image, c: &Candidate) -> Result<u32, String> {
    let at = match &c.section {
        Some(s) => img.scan_section(s, &c.pattern),
        None => img.scan(&c.pattern),
    }
    .map_err(|e| e.to_string())?;
    let at = i64::from(at) + c.offset;
    let at = u32::try_from(at)
        .ok()
        .filter(|&a| (a as usize) < img.bytes.len())
        .ok_or_else(|| format!("match + offset {at:#x} is outside the image"))?;
    match c.resolve {
        Resolve::Address => Ok(at),
        Resolve::Rip { disp, len } => img.rip_target(at, disp, len),
        Resolve::Call => img.call_target(at),
    }
    .map_err(|e| e.to_string())
}

/// Resolves a pattern on the running build (`build` = lowercase hex SHA-256
/// of the exe). Candidates tagged with this build are tried first, and the
/// first that resolves wins. Otherwise every candidate is tried, and those
/// that resolve must agree on the address: two different answers are an
/// error, never a guess.
pub fn resolve_pattern(
    img: &Image,
    def: &PatternDef,
    build: Option<&str>,
) -> Result<Resolved, String> {
    let tagged = |c: &Candidate| build.is_some_and(|b| c.builds.iter().any(|t| t == b));
    let mut errors = Vec::new();
    for (i, c) in def.candidates.iter().enumerate().filter(|(_, c)| tagged(c)) {
        match resolve_candidate(img, c) {
            Ok(rva) => return Ok(Resolved { rva, candidate: i }),
            Err(e) => errors.push(format!("candidate {} (this build): {e}", i + 1)),
        }
    }
    let mut found: Vec<Resolved> = Vec::new();
    for (i, c) in def
        .candidates
        .iter()
        .enumerate()
        .filter(|(_, c)| !tagged(c))
    {
        match resolve_candidate(img, c) {
            Ok(rva) => found.push(Resolved { rva, candidate: i }),
            Err(e) => errors.push(if def.candidates.len() == 1 {
                e
            } else {
                format!("candidate {}: {e}", i + 1)
            }),
        }
    }
    match found.as_slice() {
        [] => Err(errors.join("; ")),
        [first, rest @ ..] if rest.iter().all(|r| r.rva == first.rva) => Ok(first.clone()),
        _ => Err(format!(
            "candidates disagree ({})",
            found
                .iter()
                .map(|r| format!("{} at {:#x}", r.candidate + 1, r.rva))
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// Reads the pattern override for mod `id` from `dir` (`uk_qol/patterns/`),
/// if one exists. The override replaces the built-in `patterns.toml` whole.
pub fn read_override(dir: &Path, id: &str) -> Option<std::io::Result<String>> {
    let p = dir.join(format!("{id}.toml"));
    match std::fs::read_to_string(&p) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        r => Some(r),
    }
}

// ---------------------------------------------------------------------------
// The host

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Active,
    /// Turned off in the config (or off by default and not turned on).
    DisabledByUser,
    /// The session runs in safe mode; no mod is loaded.
    SafeMode,
    /// These required patterns didn't resolve on this build.
    PatternsMissing(Vec<String>),
    /// Waiting on another mod: a dependency isn't active, or a conflicting
    /// mod is. Retried whenever the set of active mods changes.
    Blocked(String),
    /// Bad manifest or patterns file, `on_load` error, hook error or panic.
    Failed(String),
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Status::Active => write!(f, "active"),
            Status::DisabledByUser => write!(f, "off"),
            Status::SafeMode => write!(f, "off (safe mode)"),
            Status::PatternsMissing(p) => write!(f, "patterns not found: {}", p.join(", ")),
            Status::Blocked(r) => write!(f, "waiting: {r}"),
            Status::Failed(r) => write!(f, "failed: {r}"),
        }
    }
}

/// One row of the overlay's mod list.
#[derive(Debug, Clone, PartialEq)]
pub struct ModInfo {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub status: Status,
    /// Detail for the status (pattern errors, failure reason), for display.
    pub reason: Option<String>,
    /// The manifest doesn't list this game build as tested.
    pub untested_build: bool,
}

/// What the host needs from the process it runs in. Built by `run()` in game
/// and by hand in tests.
pub struct HostEnv {
    /// The exe as mapped in memory; `None` if it couldn't be read (then no
    /// mod can load).
    pub image: Option<Image<'static>>,
    pub base: usize,
    /// Lowercase hex SHA-256 of the exe, if known.
    pub build: Option<String>,
    /// `uk_qol/patterns/`: per-mod pattern overrides.
    pub patterns_dir: Option<PathBuf>,
    /// Where log lines go.
    pub log: fn(&str),
    /// Told which mods are (about to be) active before their hooks go in,
    /// for crash triage (`crate::log_active_mods` in game).
    pub note_active: fn(&[&str]),
}

struct Slot {
    id: String,
    m: Box<dyn Mod>,
    manifest: Option<Manifest>,
    patterns: BTreeMap<String, u32>,
    /// Required patterns that didn't resolve, with why.
    missing: Vec<(String, String)>,
    status: Status,
    hooks: Option<HookSet>,
    /// Set when the mod panicked: it stays off for the session.
    panicked: Option<String>,
    user: ModConfig,
    settings: toml::Table,
}

impl Slot {
    fn enabled(&self) -> bool {
        self.user
            .enabled
            .unwrap_or(self.manifest.as_ref().is_some_and(|m| m.enabled))
    }

    fn depends_on(&self, id: &str) -> bool {
        self.manifest
            .as_ref()
            .is_some_and(|m| m.dependencies.iter().any(|d| d == id))
    }

    fn conflicts_with(&self, id: &str) -> bool {
        self.manifest
            .as_ref()
            .is_some_and(|m| m.conflicts.iter().any(|d| d == id))
    }
}

pub struct Host {
    slots: Vec<Slot>,
    /// Slot indices in dependency order (dependencies first).
    order: Vec<usize>,
    image: Option<Image<'static>>,
    base: usize,
    build: Option<String>,
    safe: SafeMode,
    patterns_dir: Option<PathBuf>,
    log: fn(&str),
    note_active: fn(&[&str]),
    /// True while `start` runs (the active list is noted once, up front).
    starting: bool,
    /// The whole user config, unknown keys included (see [`Host::config`]).
    config: Config,
}

impl Host {
    /// Builds the host: instantiates every mod, parses its manifest and
    /// patterns, and unless in safe mode resolves all patterns (before any
    /// hook changes the code) and loads the enabled mods.
    pub fn start(builtin: &[ModCtor], env: HostEnv, config: &Config, safe: SafeMode) -> Host {
        let mut h = Host {
            slots: Vec::new(),
            order: Vec::new(),
            image: env.image,
            base: env.base,
            build: env.build,
            safe,
            patterns_dir: env.patterns_dir,
            log: env.log,
            note_active: env.note_active,
            starting: true,
            config: config.clone(),
        };
        let mut pattern_files = Vec::new();
        for ctor in builtin {
            let m = ctor();
            let id = m.id().to_owned();
            let user = config.mods.get(&id).cloned().unwrap_or_default();
            let mut slot = Slot {
                id: id.clone(),
                manifest: None,
                patterns: BTreeMap::new(),
                missing: Vec::new(),
                status: Status::DisabledByUser,
                hooks: None,
                panicked: None,
                user,
                settings: toml::Table::new(),
                m,
            };
            let mut pf = PatternFile::default();
            match h.prepare(&mut slot) {
                Ok((manifest, p)) => {
                    slot.manifest = Some(manifest);
                    pf = p;
                }
                Err(e) => slot.status = Status::Failed(e),
            }
            if h.slots.iter().any(|s| s.id == id) {
                slot.manifest = None;
                slot.status = Status::Failed(format!("another mod has the id {id:?}"));
            }
            h.slots.push(slot);
            pattern_files.push(pf);
        }
        h.order = h.dependency_order();

        if h.safe != SafeMode::Off {
            for s in h.slots.iter_mut().filter(|s| s.manifest.is_some()) {
                s.status = Status::SafeMode;
            }
            h.starting = false;
            return h;
        }
        // Resolve everything first: once a mod's hooks are in, the code other
        // patterns match may have changed.
        for (i, pf) in pattern_files.iter().enumerate() {
            if h.slots[i].manifest.is_some() {
                h.resolve(i, pf);
            }
        }
        // Note every mod that may load before any hook goes in; dependencies
        // and conflicts can only make the list shorter.
        let planned: Vec<String> = h
            .order
            .iter()
            .filter(|&&i| h.slots[i].manifest.is_some() && h.static_check(i).is_ok())
            .map(|&i| h.slots[i].id.clone())
            .collect();
        (h.note_active)(&strs(&planned));
        for i in h.order.clone() {
            if h.slots[i].manifest.is_some() {
                h.try_load(i);
            }
        }
        h.retry_blocked();
        h.starting = false;
        let active = h.active_ids();
        if active != planned {
            (h.note_active)(&strs(&active));
        }
        h
    }

    /// Ids of the active mods, in load order.
    pub fn active_ids(&self) -> Vec<String> {
        self.order
            .iter()
            .filter(|&&i| self.slots[i].status == Status::Active)
            .map(|&i| self.slots[i].id.clone())
            .collect()
    }

    /// Parses the manifest, checks it against the mod, reads the patterns
    /// (override first), computes settings.
    fn prepare(&self, slot: &mut Slot) -> Result<(Manifest, PatternFile), String> {
        let manifest =
            Manifest::parse(slot.m.manifest()).map_err(|e| format!("bad mod.toml: {e}"))?;
        if manifest.id != slot.id {
            return Err(format!(
                "mod.toml id {:?} doesn't match the mod's id {:?}",
                manifest.id, slot.id
            ));
        }
        let (settings, warnings) = manifest.effective_settings(&slot.user.settings);
        for w in warnings {
            (self.log)(&format!("mod {}: {w}", slot.id));
        }
        slot.settings = settings;

        let builtin =
            PatternFile::parse(slot.m.patterns()).map_err(|e| format!("bad patterns.toml: {e}"))?;
        let mut patterns = builtin;
        if let Some(dir) = &self.patterns_dir {
            match read_override(dir, &slot.id) {
                None => {}
                Some(Err(e)) => (self.log)(&format!(
                    "mod {}: can't read pattern override: {e}; using the built-in patterns",
                    slot.id
                )),
                Some(Ok(text)) => match PatternFile::parse(&text) {
                    Ok(p) => {
                        (self.log)(&format!(
                            "mod {}: using patterns from {}",
                            slot.id,
                            dir.join(format!("{}.toml", slot.id)).display()
                        ));
                        patterns = p;
                    }
                    Err(e) => (self.log)(&format!(
                        "mod {}: bad pattern override ({e}); using the built-in patterns",
                        slot.id
                    )),
                },
            }
        }
        Ok((manifest, patterns))
    }

    fn resolve(&mut self, i: usize, pf: &PatternFile) {
        let log = self.log;
        let slot = &mut self.slots[i];
        let Some(img) = &self.image else {
            if !pf.patterns.is_empty() {
                slot.missing = pf
                    .patterns
                    .iter()
                    .filter(|p| !p.optional)
                    .map(|p| (p.name.clone(), "game image not available".into()))
                    .collect();
            }
            return;
        };
        for def in &pf.patterns {
            match resolve_pattern(img, def, self.build.as_deref()) {
                Ok(r) => {
                    let alt = if def.candidates.len() > 1 {
                        format!(" (candidate {})", r.candidate + 1)
                    } else {
                        String::new()
                    };
                    log(&format!(
                        "mod {}: pattern {} at rva {:#x}{alt}",
                        slot.id, def.name, r.rva
                    ));
                    slot.patterns.insert(def.name.clone(), r.rva);
                }
                Err(e) => {
                    let opt = if def.optional { " (optional)" } else { "" };
                    log(&format!("mod {}: pattern {}{opt}: {e}", slot.id, def.name));
                    if !def.optional {
                        slot.missing.push((def.name.clone(), e));
                    }
                }
            }
        }
    }

    /// Dependency order (Kahn's algorithm, builtin order as tie-break). Mods
    /// in a dependency cycle are marked failed and appended at the end.
    fn dependency_order(&mut self) -> Vec<usize> {
        let n = self.slots.len();
        let index: BTreeMap<&str, usize> = self
            .slots
            .iter()
            .enumerate()
            .filter(|(_, s)| s.manifest.is_some())
            .map(|(i, s)| (s.id.as_str(), i))
            .collect();
        let deps: Vec<Vec<usize>> = self
            .slots
            .iter()
            .map(|s| {
                s.manifest.as_ref().map_or_else(Vec::new, |m| {
                    m.dependencies
                        .iter()
                        .filter_map(|d| index.get(d.as_str()).copied())
                        .collect()
                })
            })
            .collect();
        let mut order = Vec::with_capacity(n);
        let mut placed = vec![false; n];
        loop {
            let next = (0..n).find(|&i| !placed[i] && deps[i].iter().all(|&d| placed[d]));
            match next {
                Some(i) => {
                    placed[i] = true;
                    order.push(i);
                }
                None => break,
            }
        }
        for (i, &done) in placed.iter().enumerate() {
            if !done {
                self.slots[i].status = Status::Failed("dependency cycle".into());
                self.slots[i].manifest = None;
                order.push(i);
            }
        }
        order
    }

    fn find(&self, id: &str) -> Option<usize> {
        self.slots.iter().position(|s| s.id == id)
    }

    fn is_active(&self, id: &str) -> bool {
        self.find(id)
            .is_some_and(|i| self.slots[i].status == Status::Active)
    }

    /// Loads mod `i` if it's enabled and everything it needs is there; sets
    /// its status either way. Doesn't touch an active mod.
    fn try_load(&mut self, i: usize) {
        let log = self.log;
        if self.slots[i].status == Status::Active || self.slots[i].manifest.is_none() {
            return;
        }
        let status = self.check_loadable(i);
        if let Err(st) = status {
            self.slots[i].status = st;
            return;
        }
        let Some(image) = &self.image else {
            self.slots[i].status = Status::Failed("game image not available".into());
            return;
        };
        let note_active = self.note_active;
        let will_be_active = (!self.starting).then(|| {
            let mut ids = self.active_ids();
            ids.push(self.slots[i].id.clone());
            ids
        });
        let slot = &mut self.slots[i];
        let Slot {
            id,
            m,
            patterns,
            settings,
            ..
        } = slot;
        let mut hooks = HookSet::new(id);
        let r = guard::call(id, || {
            let mut cx = LoadCtx {
                image,
                base: self.base,
                patterns,
                hooks: &mut hooks,
                settings,
            };
            m.on_load(&mut cx)
        });
        let err = match r {
            None => {
                let r = panic_reason(id);
                slot.panicked = Some(r.clone());
                Some(r)
            }
            Some(Err(e)) => Some(e),
            Some(Ok(())) => {
                match enable_noted(&mut hooks, will_be_active.as_deref(), note_active) {
                    Ok(()) => None,
                    Err(e) => {
                        // `enable` already restored what it wrote.
                        let _ = guard::call(id, || m.on_unload());
                        Some(format!("hooks: {e}"))
                    }
                }
            }
        };
        match err {
            None => {
                slot.hooks = Some(hooks);
                slot.status = Status::Active;
                log(&format!("mod {}: loaded", slot.id));
            }
            Some(e) => {
                // Nothing of a failed load stays in game memory.
                let _ = hooks.disable();
                log(&format!("mod {}: failed to load: {e}", slot.id));
                slot.status = Status::Failed(e);
            }
        }
    }

    /// Why mod `i` can't load right now, as the status to give it.
    fn check_loadable(&self, i: usize) -> Result<(), Status> {
        self.static_check(i)?;
        let s = &self.slots[i];
        let m = s.manifest.as_ref().expect("checked by caller");
        for d in &m.dependencies {
            if !self.is_active(d) {
                let what = if self.find(d).is_some() {
                    "which is not active"
                } else {
                    "which is not installed"
                };
                return Err(Status::Blocked(format!("needs {d}, {what}")));
            }
        }
        for o in self.slots.iter().filter(|o| o.status == Status::Active) {
            if s.conflicts_with(&o.id) || o.conflicts_with(&s.id) {
                return Err(Status::Blocked(format!("conflicts with {}", o.id)));
            }
        }
        Ok(())
    }

    /// The checks that don't depend on other mods' state.
    fn static_check(&self, i: usize) -> Result<(), Status> {
        let s = &self.slots[i];
        if self.safe != SafeMode::Off {
            return Err(Status::SafeMode);
        }
        if let Some(r) = s.panicked.clone().or_else(|| guard::failure(&s.id)) {
            return Err(Status::Failed(r));
        }
        if !s.enabled() {
            return Err(Status::DisabledByUser);
        }
        if !s.missing.is_empty() {
            return Err(Status::PatternsMissing(
                s.missing.iter().map(|(n, _)| n.clone()).collect(),
            ));
        }
        Ok(())
    }

    /// Retries blocked mods until nothing changes.
    fn retry_blocked(&mut self) {
        loop {
            let mut changed = false;
            for i in self.order.clone() {
                if matches!(self.slots[i].status, Status::Blocked(_)) {
                    self.try_load(i);
                    changed |= self.slots[i].status == Status::Active;
                }
            }
            if !changed {
                break;
            }
        }
    }

    /// Unloads active mod `i` (hooks first, then `on_unload`) and the active
    /// mods that depend on it, which become blocked.
    fn unload(&mut self, i: usize, status: Status) {
        let id = self.slots[i].id.clone();
        for j in self.order.clone().into_iter().rev() {
            if self.slots[j].status == Status::Active && self.slots[j].depends_on(&id) {
                self.unload(
                    j,
                    Status::Blocked(format!("needs {id}, which is not active")),
                );
            }
        }
        let log = self.log;
        let slot = &mut self.slots[i];
        if slot.status == Status::Active {
            if let Some(mut hooks) = slot.hooks.take()
                && let Err(e) = hooks.disable()
            {
                log(&format!("mod {}: removing hooks failed: {e}", slot.id));
            }
            let Slot { id, m, .. } = slot;
            let _ = guard::call(id, || m.on_unload());
            log(&format!("mod {}: unloaded ({status})", slot.id));
            slot.status = status;
            if !self.starting {
                (self.note_active)(&strs(&self.active_ids()));
            }
        } else {
            slot.status = status;
        }
    }

    /// Turns a mod that failed during a callback off for the session.
    fn fail(&mut self, i: usize) {
        let id = self.slots[i].id.clone();
        let reason = panic_reason(&id);
        self.slots[i].panicked = Some(reason.clone());
        (self.log)(&format!("mod {id}: turned off: {reason}"));
        self.unload(i, Status::Failed(reason));
        self.retry_blocked();
    }

    /// Fails every active mod the guard has marked failed (e.g. a panic in
    /// one of its detours).
    fn sweep(&mut self) {
        for i in 0..self.slots.len() {
            if self.slots[i].status == Status::Active && guard::is_failed(&self.slots[i].id) {
                self.fail(i);
            }
        }
    }

    /// Runs `f` on each active mod in load order, inside `guard::call`.
    /// Stops early when `f` returns true; returns whether it did.
    fn each_active(&mut self, mut f: impl FnMut(&mut dyn Mod, &toml::Table, &str) -> bool) -> bool {
        self.sweep();
        let mut stopped = false;
        for i in self.order.clone() {
            let slot = &mut self.slots[i];
            if slot.status != Status::Active {
                continue;
            }
            let Slot {
                id, m, settings, ..
            } = slot;
            match guard::call(id, || f(m.as_mut(), settings, id)) {
                Some(true) => {
                    stopped = true;
                    break;
                }
                Some(false) => {}
                None => self.fail(i),
            }
        }
        stopped
    }

    /// Calls `on_frame` on every active mod.
    pub fn frame(&mut self) {
        self.each_active(|m, settings, _| {
            m.on_frame(&mut FrameCtx { settings });
            false
        });
    }

    /// Calls `on_input` on active mods until one swallows the event. Returns
    /// true if one did (the game must not see the event).
    pub fn input(&mut self, ev: &InputEvent) -> bool {
        self.each_active(|m, _, _| m.on_input(ev))
    }

    /// Calls `on_save` on every active mod with its entry in `states`
    /// (created if missing). Entries of other mods are left as they are, so
    /// pass the sidecar as read: a mod that's off keeps its saved state.
    pub fn save(&mut self, states: &mut BTreeMap<String, ModSaveState>) {
        self.each_active(|m, _, id| {
            m.on_save(states.entry(id.to_owned()).or_default());
            false
        });
    }

    /// Calls `on_load_save` on every active mod with its entry in `states`
    /// (an empty state if it has none).
    pub fn load_save(&mut self, states: &BTreeMap<String, ModSaveState>) {
        let empty = ModSaveState::default();
        self.each_active(|m, _, id| {
            m.on_load_save(states.get(id).unwrap_or(&empty));
            false
        });
    }

    /// Turns a mod on or off for this session (and in [`Host::config`]).
    /// Turning a mod off also unloads the mods that depend on it; any change
    /// retries blocked mods. Returns the mod's new status.
    pub fn set_enabled(&mut self, id: &str, on: bool) -> Result<Status, String> {
        let i = self.find(id).ok_or_else(|| format!("no mod {id:?}"))?;
        self.slots[i].user.enabled = Some(on);
        self.config.set_enabled(id, on);
        if self.slots[i].manifest.is_some() {
            if on {
                self.try_load(i);
            } else if self.slots[i].status == Status::Active {
                self.unload(i, Status::DisabledByUser);
            } else if matches!(self.slots[i].status, Status::Blocked(_)) {
                self.slots[i].status = Status::DisabledByUser;
            }
            self.retry_blocked();
        }
        Ok(self.slots[i].status.clone())
    }

    /// Applies a changed config: new settings go to `on_settings` of active
    /// mods, changed `enabled` flags load or unload mods.
    pub fn apply_config(&mut self, config: &Config) {
        for i in 0..self.slots.len() {
            let user = config
                .mods
                .get(&self.slots[i].id)
                .cloned()
                .unwrap_or_default();
            let Some(manifest) = &self.slots[i].manifest else {
                self.slots[i].user = user;
                continue;
            };
            let (settings, warnings) = manifest.effective_settings(&user.settings);
            for w in warnings {
                (self.log)(&format!("mod {}: {w}", self.slots[i].id));
            }
            let enabled_before = self.slots[i].enabled();
            let slot = &mut self.slots[i];
            slot.user.settings = user.settings;
            if settings != slot.settings {
                slot.settings = settings;
                if slot.status == Status::Active {
                    let Slot {
                        id, m, settings, ..
                    } = slot;
                    if guard::call(id, || m.on_settings(settings)).is_none() {
                        self.fail(i);
                    }
                }
            }
            let enabled_now = user
                .enabled
                .unwrap_or(self.slots[i].manifest.as_ref().is_some_and(|m| m.enabled));
            self.slots[i].user.enabled = user.enabled;
            if enabled_now != enabled_before {
                let id = self.slots[i].id.clone();
                let _ = self.set_enabled(&id, enabled_now);
                self.slots[i].user.enabled = user.enabled;
            }
        }
        self.config = config.clone();
    }

    /// The config as read (or last applied), with this session's
    /// [`Host::set_enabled`] changes, for the overlay to save. Keys and mod
    /// tables the host doesn't know are kept.
    pub fn config(&self) -> Config {
        self.config.clone()
    }

    /// Unloads every active mod, dependents first.
    pub fn unload_all(&mut self) {
        for i in self.order.clone().into_iter().rev() {
            if self.slots[i].status == Status::Active {
                self.unload(i, Status::DisabledByUser);
            }
        }
    }

    pub fn status(&self, id: &str) -> Option<&Status> {
        self.find(id).map(|i| &self.slots[i].status)
    }

    pub fn manifest(&self, id: &str) -> Option<&Manifest> {
        self.find(id).and_then(|i| self.slots[i].manifest.as_ref())
    }

    /// Effective settings of a mod.
    pub fn settings(&self, id: &str) -> Option<&toml::Table> {
        self.find(id).map(|i| &self.slots[i].settings)
    }

    pub fn safe_mode(&self) -> &SafeMode {
        &self.safe
    }

    /// The mod list for the overlay and the log, in load order.
    pub fn mods(&self) -> Vec<ModInfo> {
        self.order
            .iter()
            .map(|&i| {
                let s = &self.slots[i];
                let m = s.manifest.as_ref();
                let reason = match &s.status {
                    Status::PatternsMissing(_) => Some(
                        s.missing
                            .iter()
                            .map(|(n, e)| format!("{n}: {e}"))
                            .collect::<Vec<_>>()
                            .join("; "),
                    ),
                    Status::Blocked(r) | Status::Failed(r) => Some(r.clone()),
                    _ => None,
                };
                ModInfo {
                    id: s.id.clone(),
                    name: m.map_or_else(|| s.id.clone(), |m| m.name.clone()),
                    version: m.map_or_else(String::new, |m| m.version.clone()),
                    description: m.map_or_else(String::new, |m| m.description.clone()),
                    status: s.status.clone(),
                    reason,
                    untested_build: m.is_some_and(|m| !m.tested_on(self.build.as_deref())),
                }
            })
            .collect()
    }

    /// One log line per mod with its status. The active list itself is
    /// logged through `HostEnv::note_active`.
    pub fn summary(&self) -> Vec<String> {
        let mods = self.mods();
        let mut out: Vec<String> = mods
            .iter()
            .map(|m| {
                let untested = if m.untested_build && m.status == Status::Active {
                    " (not tested on this game build)"
                } else {
                    ""
                };
                format!("mod {} {}: {}{untested}", m.id, m.version, m.status)
            })
            .collect();
        if self.safe.is_on() {
            out.push(format!("safe mode ({:?}): all mods off", self.safe));
        }
        out
    }
}

/// Notes the active list (when given) right before the hooks go in.
fn enable_noted(
    hooks: &mut HookSet,
    active: Option<&[String]>,
    note_active: fn(&[&str]),
) -> Result<(), HookError> {
    if let Some(ids) = active {
        note_active(&strs(ids));
    }
    hooks.enable()
}

fn strs(v: &[String]) -> Vec<&str> {
    v.iter().map(String::as_str).collect()
}

/// Why `id` failed inside `guard::call`: the guard's reason if it recorded
/// one, else a generic one, which is also handed to the guard so the mod
/// stays off for the session.
fn panic_reason(id: &str) -> String {
    guard::failure(id).unwrap_or_else(|| {
        let r = "panicked".to_owned();
        guard::fail(id, r.clone());
        r
    })
}

// ---------------------------------------------------------------------------
// The global

static HOST: Mutex<Option<Host>> = Mutex::new(None);

thread_local! {
    static IN_HOST: Cell<bool> = const { Cell::new(false) };
}

/// Installs `host` as the global (replacing and returning any previous one).
pub fn install(host: Host) -> Option<Host> {
    HOST.lock().unwrap_or_else(|e| e.into_inner()).replace(host)
}

/// Removes the global host.
pub fn take() -> Option<Host> {
    HOST.lock().unwrap_or_else(|e| e.into_inner()).take()
}

/// Runs `f` on the global host. `None` if there is none yet, or if called
/// from inside another `with` on the same thread (a mod callback reaching
/// back into the host), which would otherwise deadlock.
pub fn with<R>(f: impl FnOnce(&mut Host) -> R) -> Option<R> {
    if IN_HOST.with(Cell::get) {
        return None;
    }
    let mut g = HOST.lock().unwrap_or_else(|e| e.into_inner());
    let h = g.as_mut()?;
    IN_HOST.with(|c| c.set(true));
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            IN_HOST.with(|c| c.set(false));
        }
    }
    let _reset = Reset;
    Some(f(h))
}

/// The running exe's own image, as mapped in memory, and its base address.
#[cfg(windows)]
pub fn own_image() -> Option<(Image<'static>, usize)> {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetModuleHandleW(name: *const u16) -> *mut core::ffi::c_void;
    }
    // SAFETY: a null name returns the exe's module handle, i.e. its base.
    let base = unsafe { GetModuleHandleW(std::ptr::null()) } as usize;
    if base == 0 {
        return None;
    }
    // SAFETY: the first page of a mapped module holds its headers and is
    // always readable.
    let head = unsafe { std::slice::from_raw_parts(base as *const u8, 0x1000) };
    let headers = crate::pe::Headers::parse(head).ok()?;
    // SAFETY: the loader maps SizeOfImage bytes from the base, and the exe
    // stays mapped for the life of the process. Hooks may change code bytes
    // later; patterns are resolved before any hook is enabled.
    let bytes =
        unsafe { std::slice::from_raw_parts(base as *const u8, headers.size_of_image as usize) };
    Some((Image { bytes, headers }, base))
}

/// The running exe's own image (only available in game, on Windows).
#[cfg(not(windows))]
pub fn own_image() -> Option<(Image<'static>, usize)> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pe::{self, SCN_MEM_EXECUTE, tests::build_pe};

    const SHA_A: &str = "d3158ad0f94d6770deb64b32d48aeb412a3c5ef3a0b2e37a0e092a7ce66bb4a4";
    const SHA_B: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    // ----- manifests

    #[test]
    fn manifest_full() {
        let m = Manifest::parse(&format!(
            r#"
            id = "weapon-swap"
            name = "Weapon swap"
            version = "1.2.0"
            author = "someone"
            description = "A ranged slot."
            game_builds = ["{}"]
            conflicts = ["other-swap"]
            dependencies = ["core-lib"]
            enabled = false

            [[settings]]
            key = "swap_costs_turn"
            type = "bool"
            default = true
            label = "Swapping costs a turn"

            [[settings]]
            key = "slots"
            type = "int"
            default = 3
            min = 1
            max = 5
            label = "Slots"

            [[settings]]
            key = "scale"
            type = "float"
            default = 1
            min = 0.5
            label = "Scale"

            [[settings]]
            key = "mode"
            type = "enum"
            choices = ["bones", "health"]
            default = "bones"
            label = "Mode"
            description = "What a spell costs."

            [[settings]]
            key = "hotkey"
            type = "key"
            default = "Shift+S"
            label = "Hotkey"

            [[settings]]
            key = "title"
            type = "string"
            default = ""
            label = "Title"
            "#,
            SHA_A.to_uppercase()
        ))
        .unwrap();
        assert_eq!(m.id, "weapon-swap");
        assert_eq!(m.game_builds, Builds::Tested(vec![SHA_A.into()]));
        assert!(m.tested_on(Some(SHA_A)));
        assert!(!m.tested_on(Some(SHA_B)) && !m.tested_on(None));
        assert!(!m.enabled);
        assert_eq!(m.settings.len(), 6);
        assert_eq!(
            m.setting("slots").unwrap().kind,
            SettingKind::Int {
                min: Some(1),
                max: Some(5)
            }
        );
        // An integer default of a float setting is stored as a float.
        assert_eq!(m.setting("scale").unwrap().default, toml::Value::Float(1.0));
        assert_eq!(
            m.setting("mode").unwrap().description,
            "What a spell costs."
        );

        let minimal =
            Manifest::parse("id = \"a\"\nname = \"A\"\nversion = \"0.1.0\"\ngame_builds = \"any\"")
                .unwrap();
        assert!(minimal.enabled && minimal.settings.is_empty());
        assert!(minimal.tested_on(None));
    }

    #[test]
    fn manifest_errors() {
        let base = "name = \"A\"\nversion = \"1\"\ngame_builds = \"any\"\n";
        let err = |extra: &str| {
            Manifest::parse(&format!("id = \"a\"\n{base}{extra}"))
                .map(|_| ())
                .unwrap_err()
        };
        assert!(
            Manifest::parse(&format!("id = \"Bad_Id\"\n{base}"))
                .unwrap_err()
                .contains("id")
        );
        assert!(Manifest::parse("id = \"a\"\nname = \"A\"\nversion = \"1\"").is_err());
        assert!(
            Manifest::parse("id = \"a\"\nname = \"A\"\nversion = \"1\"\ngame_builds = [\"abc\"]")
                .unwrap_err()
                .contains("SHA-256")
        );
        assert!(
            err("colour = 1").contains("colour"),
            "unknown keys rejected"
        );
        assert!(err("dependencies = [\"a\"]").contains("bad mod id"));
        let setting = |s: &str| err(&format!("[[settings]]\nlabel = \"L\"\n{s}"));
        assert!(setting("key = \"enabled\"\ntype = \"bool\"\ndefault = true").contains("enabled"));
        assert!(setting("key = \"x\"\ntype = \"colour\"\ndefault = 1").contains("unknown type"));
        assert!(setting("key = \"x\"\ntype = \"bool\"\ndefault = 1").contains("default"));
        assert!(setting("key = \"x\"\ntype = \"bool\"\ndefault = true\nmin = 1").contains("min"));
        assert!(
            setting("key = \"x\"\ntype = \"int\"\ndefault = 9\nmax = 5").contains("outside ..=5")
        );
        assert!(setting("key = \"x\"\ntype = \"enum\"\ndefault = \"a\"").contains("choices"));
        assert!(
            setting("key = \"x\"\ntype = \"enum\"\nchoices = [\"a\"]\ndefault = \"b\"")
                .contains("not one of")
        );
        assert!(setting("key = \"x\"\ntype = \"key\"\ndefault = \"Hyper+S\"").contains("modifier"));
        let twice = "[[settings]]\nkey = \"x\"\ntype = \"bool\"\ndefault = true\nlabel = \"L\"\n";
        assert!(err(&format!("{twice}{twice}")).contains("twice"));
    }

    #[test]
    fn hotkeys() {
        for ok in ["", "S", "F10", "Shift+1", "ctrl+alt+PageUp", "Pad:DpadLeft"] {
            assert_eq!(check_hotkey(ok), Ok(()), "{ok}");
        }
        for bad in [
            "+",
            "Shift+",
            "Shift+Shift+S",
            "Win+S",
            "Pad:",
            "S S",
            "Shift+-",
        ] {
            assert!(check_hotkey(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn effective_settings_check_the_schema() {
        let m = Manifest::parse(
            r#"id = "a"
            name = "A"
            version = "1"
            game_builds = "any"
            [[settings]]
            key = "n"
            type = "int"
            default = 2
            max = 5
            label = "N"
            [[settings]]
            key = "f"
            type = "float"
            default = 0.5
            label = "F"
            [[settings]]
            key = "k"
            type = "key"
            default = "S"
            label = "K"
            "#,
        )
        .unwrap();
        let user: toml::Table = "n = 9\nf = 3\nk = \"Bogus+X\"\nextra = 1".parse().unwrap();
        let (s, warnings) = m.effective_settings(&user);
        assert_eq!(s["n"], toml::Value::Integer(2), "out of range: default");
        assert_eq!(s["f"], toml::Value::Float(3.0), "integer accepted as float");
        assert_eq!(s["k"], toml::Value::String("S".into()));
        assert_eq!(
            s["extra"],
            toml::Value::Integer(1),
            "undeclared keys pass through"
        );
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        let (s, warnings) = m.effective_settings(&"n = \"x\"".parse().unwrap());
        assert_eq!(s["n"], toml::Value::Integer(2));
        assert_eq!(warnings.len(), 1);
    }

    // ----- patterns

    /// .text at 0x1000: `mov rax, [rip+X]` (→ 0x2000), `call F` (→ 0x100E),
    /// `nop`, `ret`, F: `ret`, then a lone `CC CC 77`. .rdata at 0x2000 holds
    /// a copy of the code (code scans must not see it).
    fn image() -> Image<'static> {
        let mut code = vec![0x48, 0x8B, 0x05];
        code.extend_from_slice(&(0x2000i32 - 0x1007).to_le_bytes());
        code.push(0xE8);
        code.extend_from_slice(&(0x100Ei32 - 0x100C).to_le_bytes());
        code.extend_from_slice(&[0x90, 0xC3, 0xC3, 0xCC, 0xCC, 0x77]);
        let rdata = code.clone();
        let bytes = pe::map_file(&build_pe(&[
            (".text", 0x1000, &code, SCN_MEM_EXECUTE),
            (".rdata", 0x2000, &rdata, 0),
        ]))
        .unwrap();
        Image::new(Box::leak(bytes.into_boxed_slice())).unwrap()
    }

    fn resolve_one(text: &str, build: Option<&str>) -> Result<Resolved, String> {
        let pf = PatternFile::parse(text).unwrap();
        resolve_pattern(&image(), &pf.patterns[0], build)
    }

    #[test]
    fn pattern_file_parse() {
        let pf = PatternFile::parse(&format!(
            r#"
            # the global the mov loads
            [global]
            pattern = "48 8B 05 ?? ?? ?? ?? E8"
            rip = {{ disp = 3, len = 7 }}

            [callee]
            optional = true
            [[callee.candidates]]
            pattern = "E8 ?? ?? ?? ?? 90"
            call = true
            builds = ["{SHA_A}"]
            [[callee.candidates]]
            pattern = "48 8B 05"
            offset = 7
            call = true
            section = ".text"
            "#
        ))
        .unwrap();
        assert_eq!(pf.patterns.len(), 2);
        let callee = &pf.patterns[0];
        assert_eq!(callee.name, "callee");
        assert!(callee.optional);
        assert_eq!(callee.candidates.len(), 2);
        assert_eq!(callee.candidates[0].builds, vec![SHA_A.to_owned()]);
        assert_eq!(callee.candidates[1].offset, 7);
        assert_eq!(callee.candidates[1].section.as_deref(), Some(".text"));
        assert_eq!(
            pf.patterns[1].candidates[0].resolve,
            Resolve::Rip { disp: 3, len: 7 }
        );
        assert!(PatternFile::parse("").unwrap().patterns.is_empty());

        let err = |t: &str| PatternFile::parse(t).unwrap_err();
        assert!(err("[a]\noptional = true").contains("needs `pattern`"));
        assert!(err("[a]\npattern = \"?? 48\"").contains("wildcard"));
        assert!(
            err("[a]\npattern = \"48\"\ncall = true\nrip = { disp = 1, len = 5 }").contains("both")
        );
        assert!(err("[a]\npattern = \"48\"\nrip = { disp = 3, len = 6 }").contains("fit"));
        assert!(err("[a]\npattern = \"48\"\nbuilds = [\"x\"]").contains("SHA-256"));
        assert!(
            err("[a]\npattern = \"48\"\n[[a.candidates]]\npattern = \"48\"").contains("not both")
        );
        assert!(err("[a]\noffset = 1\n[[a.candidates]]\npattern = \"48\"").contains("every field"));
        assert!(err("[a]\npattern = \"48\"\ncolour = 1").contains("colour"));
    }

    #[test]
    fn resolve_steps() {
        let at = |t: &str| resolve_one(t, None).map(|r| r.rva);
        assert_eq!(at("[a]\npattern = \"48 8B 05\""), Ok(0x1000));
        assert_eq!(at("[a]\npattern = \"48 8B 05\"\noffset = 7"), Ok(0x1007));
        assert_eq!(
            at("[a]\npattern = \"48 8B 05\"\nrip = { disp = 3, len = 7 }"),
            Ok(0x2000)
        );
        assert_eq!(
            at("[a]\npattern = \"48 8B 05\"\noffset = 7\ncall = true"),
            Ok(0x100E)
        );
        assert_eq!(
            at("[a]\npattern = \"48 8B 05\"\nsection = \".rdata\""),
            Ok(0x2000)
        );
        // Negative offset: the call is the 5 bytes before the nop.
        assert_eq!(
            at("[a]\npattern = \"90 C3\"\noffset = -5\ncall = true"),
            Ok(0x100E)
        );
        assert!(
            at("[a]\npattern = \"C3\"")
                .unwrap_err()
                .contains("matched 2 times")
        );
        assert!(
            at("[a]\npattern = \"12 34\"")
                .unwrap_err()
                .contains("not found")
        );
        assert!(
            at("[a]\npattern = \"48 8B 05\"\ncall = true")
                .unwrap_err()
                .contains("unexpected")
        );
        assert!(
            at("[a]\npattern = \"48\"\noffset = -99999")
                .unwrap_err()
                .contains("outside")
        );
    }

    #[test]
    fn resolve_candidates() {
        let two = |b1: &str, p2: &str| {
            format!(
                "[a]\n[[a.candidates]]\npattern = \"12 34\"\n{b1}\n\
                 [[a.candidates]]\npattern = \"{p2}\"\n\
                 [[a.candidates]]\npattern = \"48 8B 05\"\n"
            )
        };
        // One fails, two agree on 0x1000: fine, first agreeing one reported.
        let r = resolve_one(&two("", "48 8B"), None).unwrap();
        assert_eq!(
            r,
            Resolved {
                rva: 0x1000,
                candidate: 1
            }
        );
        // Two resolve to different places: refused.
        let e = resolve_one(&two("", "E8"), None).unwrap_err();
        assert!(e.contains("disagree"), "{e}");
        // A candidate tagged with this build wins outright...
        let tagged = format!(
            "[a]\n[[a.candidates]]\npattern = \"48 8B 05\"\n\
             [[a.candidates]]\npattern = \"E8\"\nbuilds = [\"{SHA_A}\"]\n"
        );
        assert_eq!(resolve_one(&tagged, Some(SHA_A)).unwrap().rva, 0x1007);
        // ...but on another build it's just one more candidate.
        assert!(
            resolve_one(&tagged, Some(SHA_B))
                .unwrap_err()
                .contains("disagree")
        );
        // All fail: every reason is listed.
        let e = resolve_one(
            "[a]\n[[a.candidates]]\npattern = \"12 34\"\n\
             [[a.candidates]]\npattern = \"C3\"\n",
            None,
        )
        .unwrap_err();
        assert!(
            e.contains("candidate 1: pattern not found")
                && e.contains("candidate 2: pattern matched 2 times"),
            "{e}"
        );
    }

    // ----- host lifecycle

    static EVENTS: Mutex<Vec<String>> = Mutex::new(Vec::new());
    static NOTED: Mutex<Vec<String>> = Mutex::new(Vec::new());

    fn event(s: String) {
        EVENTS.lock().unwrap_or_else(|e| e.into_inner()).push(s);
    }

    /// Events for mods whose id starts with `prefix`.
    fn events(prefix: &str) -> Vec<String> {
        EVENTS
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e.starts_with(prefix))
            .cloned()
            .collect()
    }

    fn clear_events(prefix: &str) {
        EVENTS.lock().unwrap().retain(|e| !e.starts_with(prefix));
    }

    fn noted(list: &[&str]) {
        NOTED.lock().unwrap().push(list.join(","));
    }

    struct Fake {
        id: &'static str,
        manifest: &'static str,
        patterns: &'static str,
    }

    impl Mod for Fake {
        fn id(&self) -> &'static str {
            self.id
        }
        fn manifest(&self) -> &'static str {
            self.manifest
        }
        fn patterns(&self) -> &'static str {
            self.patterns
        }
        fn on_load(&mut self, cx: &mut LoadCtx) -> Result<(), String> {
            event(format!(
                "{} load {:?} {}",
                self.id,
                cx.patterns,
                cx.settings
                    .get("n")
                    .map_or(String::new(), |v| v.to_string())
            ));
            if self.id.ends_with("-err") {
                return Err("boom".into());
            }
            Ok(())
        }
        fn on_unload(&mut self) {
            event(format!("{} unload", self.id));
        }
        fn on_frame(&mut self, _cx: &mut FrameCtx) {
            event(format!("{} frame", self.id));
            if self.id.ends_with("-panic") {
                panic!("frame panic (expected in this test)");
            }
        }
        fn on_input(&mut self, _ev: &InputEvent) -> bool {
            event(format!("{} input", self.id));
            self.id.ends_with("-swallow")
        }
        fn on_settings(&mut self, s: &toml::Table) {
            event(format!("{} settings {}", self.id, s["n"]));
        }
        fn on_save(&mut self, st: &mut ModSaveState) {
            st.global.insert("saved".into(), self.id.into());
        }
        fn on_load_save(&mut self, st: &ModSaveState) {
            event(format!("{} loadsave {}", self.id, st.global.len()));
        }
    }

    macro_rules! fake {
        ($fn:ident, $id:literal, $extra:literal, $patterns:expr) => {
            fn $fn() -> Box<dyn Mod> {
                Box::new(Fake {
                    id: $id,
                    manifest: concat!(
                        "id = \"",
                        $id,
                        "\"\nname = \"",
                        $id,
                        "\"\nversion = \"1.0\"\ngame_builds = \"any\"\n",
                        $extra
                    ),
                    patterns: $patterns,
                })
            }
        };
    }

    fake!(
        l_base_swallow,
        "l-base-swallow",
        "[[settings]]\nkey = \"n\"\ntype = \"int\"\ndefault = 1\nlabel = \"N\"\n",
        "[mov]\npattern = \"48 8B 05\"\n[maybe]\noptional = true\npattern = \"12 34\"\n"
    );
    fake!(l_dep, "l-dep", "dependencies = [\"l-base-swallow\"]\n", "");
    fake!(l_rival, "l-rival", "conflicts = [\"l-base-swallow\"]\n", "");
    fake!(l_off, "l-off", "enabled = false\n", "");
    fake!(l_load_err, "l-load-err", "", "");
    fake!(l_frame_panic, "l-frame-panic", "", "");
    fake!(l_missing, "l-missing", "", "[gone]\npattern = \"12 34\"\n");
    fake!(l_bad, "l-bad", "colour = 1\n", "");
    fake!(
        l_needs_ghost,
        "l-needs-ghost",
        "dependencies = [\"ghost\"]\n",
        ""
    );

    fn env() -> HostEnv {
        HostEnv {
            image: Some(image()),
            base: 0x1_4000_0000,
            build: Some(SHA_A.into()),
            patterns_dir: None,
            log: |_| {},
            note_active: noted,
        }
    }

    fn status(h: &Host, id: &str) -> Status {
        h.status(id).unwrap().clone()
    }

    #[test]
    fn host_lifecycle() {
        // Builtin order puts the dependent first; the host loads by dependency.
        let ctors: &[ModCtor] = &[
            l_dep,
            l_base_swallow,
            l_rival,
            l_off,
            l_load_err,
            l_frame_panic,
            l_missing,
            l_bad,
            l_needs_ghost,
        ];
        let mut config = Config::parse("future_key = 1\n[mods.not-built-in]\nx = 2\n").unwrap();
        config.set_enabled("l-rival", true);
        config.set_setting("l-base-swallow", "n", toml::Value::Integer(5));
        let mut h = Host::start(ctors, env(), &config, SafeMode::Off);

        assert_eq!(status(&h, "l-base-swallow"), Status::Active);
        assert_eq!(status(&h, "l-dep"), Status::Active);
        assert_eq!(
            status(&h, "l-rival"),
            Status::Blocked("conflicts with l-base-swallow".into())
        );
        assert_eq!(status(&h, "l-off"), Status::DisabledByUser);
        assert_eq!(status(&h, "l-load-err"), Status::Failed("boom".into()));
        assert_eq!(status(&h, "l-frame-panic"), Status::Active);
        assert_eq!(
            status(&h, "l-missing"),
            Status::PatternsMissing(vec!["gone".into()])
        );
        assert!(matches!(status(&h, "l-bad"), Status::Failed(r) if r.contains("bad mod.toml")));
        assert_eq!(
            status(&h, "l-needs-ghost"),
            Status::Blocked("needs ghost, which is not installed".into())
        );
        let missing = h.mods().into_iter().find(|m| m.id == "l-missing").unwrap();
        assert!(missing.reason.unwrap().contains("gone: pattern not found"));
        assert_eq!(
            h.active_ids(),
            ["l-base-swallow", "l-dep", "l-frame-panic"],
            "dependency order"
        );
        assert!(
            NOTED
                .lock()
                .unwrap()
                .contains(&"l-base-swallow,l-dep,l-frame-panic".to_owned())
        );
        // Resolved patterns and user settings reach on_load; optional missing is absent.
        assert!(
            events("l-base-swallow load")[0].ends_with("load {\"mov\": 4096} 5"),
            "{:?}",
            events("l-base-swallow")
        );

        // Frame: the panicking mod is turned off, the others keep running.
        h.frame();
        h.frame();
        assert_eq!(
            status(&h, "l-frame-panic"),
            Status::Failed("panicked: frame panic (expected in this test)".into())
        );
        assert_eq!(events("l-frame-panic frame").len(), 1);
        assert_eq!(events("l-dep frame").len(), 2);
        // A mod that panicked stays off even if turned on again.
        assert_eq!(
            h.set_enabled("l-frame-panic", true),
            Ok(Status::Failed(
                "panicked: frame panic (expected in this test)".into()
            ))
        );

        // Input stops at the first mod that swallows it.
        let ev = InputEvent::PadDown { button: 0 };
        assert!(h.input(&ev));
        assert_eq!(events("l-base-swallow input").len(), 1);
        assert!(events("l-dep input").is_empty());

        // Save keeps other mods' state; load hands each mod its own.
        let mut states = BTreeMap::new();
        states.insert("l-off".to_owned(), ModSaveState::default());
        h.save(&mut states);
        assert_eq!(
            states.keys().collect::<Vec<_>>(),
            ["l-base-swallow", "l-dep", "l-off"]
        );
        h.load_save(&states);
        assert_eq!(events("l-dep loadsave"), ["l-dep loadsave 1"]);

        // Turning the base off unloads its dependent first, then lets the
        // rival in.
        clear_events("l-");
        assert_eq!(
            h.set_enabled("l-base-swallow", false),
            Ok(Status::DisabledByUser)
        );
        assert_eq!(
            events("l-"),
            ["l-dep unload", "l-base-swallow unload", "l-rival load {} "]
        );
        assert_eq!(
            status(&h, "l-dep"),
            Status::Blocked("needs l-base-swallow, which is not active".into())
        );
        assert_eq!(status(&h, "l-rival"), Status::Active);
        // Back on: the rival now blocks it.
        assert_eq!(
            h.set_enabled("l-base-swallow", true),
            Ok(Status::Blocked("conflicts with l-rival".into()))
        );
        // Rival off: base and its dependent come back (on_load again).
        clear_events("l-");
        h.set_enabled("l-rival", false).unwrap();
        assert_eq!(status(&h, "l-base-swallow"), Status::Active);
        assert_eq!(status(&h, "l-dep"), Status::Active);
        assert_eq!(events("l-base-swallow load").len(), 1);

        // Config reload: changed settings go to on_settings, enabled flags apply.
        let mut c = h.config();
        assert_eq!(c.mods["l-rival"].enabled, Some(false));
        // What the host doesn't know survives into the config it hands back.
        assert_eq!(c.extra["future_key"], toml::Value::Integer(1));
        assert_eq!(
            c.mods["not-built-in"].settings["x"],
            toml::Value::Integer(2)
        );
        c.set_setting("l-base-swallow", "n", toml::Value::Integer(7));
        c.set_enabled("l-off", true);
        clear_events("l-");
        h.apply_config(&c);
        assert_eq!(
            events("l-base-swallow settings"),
            ["l-base-swallow settings 7"]
        );
        assert_eq!(status(&h, "l-off"), Status::Active);
        assert_eq!(
            h.settings("l-base-swallow").unwrap()["n"],
            toml::Value::Integer(7)
        );

        // Unload all: dependents first.
        clear_events("l-");
        h.unload_all();
        assert!(h.active_ids().is_empty());
        let ev = events("l-");
        let pos = |s: &str| ev.iter().position(|e| e == s).unwrap();
        assert!(pos("l-dep unload") < pos("l-base-swallow unload"));

        assert!(h.set_enabled("nope", true).is_err());
    }

    #[test]
    fn safe_mode_loads_nothing() {
        fake!(s_a, "s-a", "", "");
        let h = Host::start(&[s_a], env(), &Config::default(), SafeMode::ShiftHeld);
        assert_eq!(status(&h, "s-a"), Status::SafeMode);
        assert!(events("s-a").is_empty());
        assert!(h.summary().last().unwrap().contains("safe mode"));
    }

    #[test]
    fn pattern_override_replaces_builtin() {
        fake!(o_a, "o-a", "", "[f]\npattern = \"12 34\"\n");
        fake!(o_b, "o-b", "", "[f]\npattern = \"12 34\"\n");
        let dir = std::env::temp_dir().join(format!("uk-qol-host-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("o-a.toml"), "[f]\npattern = \"E8\"\ncall = true\n").unwrap();
        std::fs::write(dir.join("o-b.toml"), "this is not toml [").unwrap();
        let mut e = env();
        e.patterns_dir = Some(dir.clone());
        let h = Host::start(&[o_a, o_b], e, &Config::default(), SafeMode::Off);
        let _ = std::fs::remove_file(dir.join("o-a.toml"));
        let _ = std::fs::remove_file(dir.join("o-b.toml"));
        let _ = std::fs::remove_dir(&dir);
        assert_eq!(status(&h, "o-a"), Status::Active);
        assert_eq!(events("o-a load"), ["o-a load {\"f\": 4110} "]);
        // A broken override falls back to the built-in file.
        assert_eq!(status(&h, "o-b"), Status::PatternsMissing(vec!["f".into()]));
    }

    #[test]
    fn mismatched_and_duplicate_ids() {
        fn wrong() -> Box<dyn Mod> {
            Box::new(Fake {
                id: "d-wrong",
                manifest: "id = \"d-other\"\nname = \"x\"\nversion = \"1\"\ngame_builds = \"any\"",
                patterns: "",
            })
        }
        fake!(d_a, "d-a", "", "");
        let h = Host::start(&[wrong, d_a, d_a], env(), &Config::default(), SafeMode::Off);
        assert!(matches!(status(&h, "d-wrong"), Status::Failed(r) if r.contains("doesn't match")));
        let mods = h.mods();
        let d_a: Vec<_> = mods.iter().filter(|m| m.id == "d-a").collect();
        assert_eq!(d_a[0].status, Status::Active);
        assert!(matches!(&d_a[1].status, Status::Failed(r) if r.contains("another mod")));
    }

    #[test]
    fn dependency_cycle() {
        fake!(c_a, "c-a", "dependencies = [\"c-b\"]\n", "");
        fake!(c_b, "c-b", "dependencies = [\"c-a\"]\n", "");
        let h = Host::start(&[c_a, c_b], env(), &Config::default(), SafeMode::Off);
        assert_eq!(status(&h, "c-a"), Status::Failed("dependency cycle".into()));
        assert_eq!(status(&h, "c-b"), Status::Failed("dependency cycle".into()));
    }

    #[test]
    fn untested_build_is_reported_not_refused() {
        fn t() -> Box<dyn Mod> {
            Box::new(Fake {
                id: "t-a",
                manifest: "id = \"t-a\"\nname = \"x\"\nversion = \"1\"\n\
                           game_builds = [\"ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad\"]",
                patterns: "",
            })
        }
        let h = Host::start(&[t], env(), &Config::default(), SafeMode::Off);
        let info = &h.mods()[0];
        assert_eq!(info.status, Status::Active);
        assert!(info.untested_build);
        assert!(h.summary()[0].contains("not tested on this game build"));
    }

    #[test]
    fn global_with() {
        fake!(g_a, "g-a", "", "");
        install(Host::start(
            &[g_a],
            env(),
            &Config::default(),
            SafeMode::Off,
        ));
        assert_eq!(with(|h| h.active_ids()), Some(vec!["g-a".to_owned()]));
        // Re-entrant use from the same thread is refused instead of deadlocking.
        assert_eq!(with(|_| with(|_| ())), Some(None));
        assert!(with(|_| ()).is_some(), "flag reset after the call");
        let h = take().unwrap();
        assert_eq!(h.active_ids(), ["g-a"]);
        assert!(with(|_| ()).is_none());
    }

    /// In a Windows process (or Wine as the cargo runner): the test exe's own
    /// mapped image is readable, and a pattern cut from a function compiled
    /// into it resolves back to that function's RVA.
    #[cfg(windows)]
    #[test]
    fn own_image_scans() {
        #[inline(never)]
        extern "C" fn marker(x: u64) -> u64 {
            std::hint::black_box(x.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0x5151)
        }
        let (img, base) = own_image().expect("own image");
        assert!(
            img.headers
                .section(".text")
                .is_some_and(|s| s.is_executable())
        );
        let rva = (marker as *const () as usize - base) as u32;
        let toks: Vec<String> = img.bytes[rva as usize..rva as usize + 24]
            .iter()
            .map(|b| format!("{b:02X}"))
            .collect();
        let pf = PatternFile::parse(&format!("[m]\npattern = \"{}\"\n", toks.join(" "))).unwrap();
        assert_eq!(
            resolve_pattern(&img, &pf.patterns[0], None).unwrap().rva,
            rva
        );
        assert_eq!(marker(1), std::hint::black_box(marker(1)));
    }
}
