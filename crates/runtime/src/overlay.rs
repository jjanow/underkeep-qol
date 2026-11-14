//! In-game overlay (PLAN §4): the mod menu, drawn with egui on the game's
//! OpenGL context, and the input routing behind hotkeys.
//!
//! [`install`] hooks four SDL3 functions in `underkeep.exe`'s import table
//! (see [`crate::iat`]):
//! - `SDL_GL_SwapWindow`: draws the menu just before the game's frame is
//!   presented, saving and restoring all GL state it touches;
//! - `SDL_PollEvent`: routes each event (see `router`): menu toggle (F10, or
//!   LS+RS on a gamepad), menu input, and mod hotkeys; a swallowed event is
//!   dropped and the next one polled, so the game never sees it;
//! - `SDL_GetGamepadButton` / `SDL_GetGamepadAxis`: the game also polls pad
//!   state directly, so while the menu is open (and while a swallowed button
//!   is held) these read "released".
//!
//! # Host interface
//!
//! The mod host feeds the menu through [`set_host`] (an [`OverlayHost`]: mod
//! rows with status and settings, and setters) and receives input for mods
//! through [`set_input_sink`]. Both can be called at any time; until then the
//! menu says no mods are loaded and input goes straight to the game.

#[cfg(windows)]
mod capture;
#[cfg(windows)]
mod gl;
#[cfg_attr(not(windows), allow(dead_code))] // used by the Windows glue
pub(crate) mod router;
pub(crate) mod sdl;
#[cfg_attr(not(windows), allow(dead_code))] // used by the Windows glue
mod ui;
#[cfg(windows)]
mod win;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};

use crate::input::InputEvent;

/// A mod's state as shown in the menu.
#[derive(Debug, Clone, PartialEq)]
pub enum ModState {
    /// Loaded and hooked.
    Active,
    /// Turned off by the user.
    Off,
    /// Turned off after an error this session (panic, hook failure); the
    /// reason is shown.
    Failed(String),
    /// Can't run on this game build (patterns not found); the reason is shown.
    Unsupported(String),
}

/// The type of a setting, from the mod manifest. See
/// [`SettingKind::from_manifest`].
#[derive(Debug, Clone, PartialEq)]
pub enum SettingKind {
    Bool,
    /// With both bounds the menu shows a slider, otherwise a number field.
    Int {
        min: Option<i64>,
        max: Option<i64>,
    },
    Float {
        min: Option<f64>,
        max: Option<f64>,
    },
    String,
    /// One of the listed strings.
    Enum(Vec<String>),
    /// A hotkey spec (see [`crate::hotkey`]), stored as a string.
    Key,
}

impl SettingKind {
    /// Reads a setting's type from its manifest table:
    ///
    /// ```toml
    /// [settings.swap_key]
    /// type = "key"            # bool | int | float | string | enum | key
    /// default = "Shift+1"
    /// min = 0                 # int / float, optional
    /// max = 10
    /// choices = ["bones", "health"]   # enum
    /// ```
    pub fn from_manifest(t: &toml::Table) -> Result<SettingKind, String> {
        let ty = t
            .get("type")
            .and_then(toml::Value::as_str)
            .ok_or("missing `type`")?;
        let int = |k: &str| t.get(k).and_then(toml::Value::as_integer);
        let float = |k: &str| {
            t.get(k)
                .and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)))
        };
        Ok(match ty {
            "bool" => SettingKind::Bool,
            "int" => SettingKind::Int {
                min: int("min"),
                max: int("max"),
            },
            "float" => SettingKind::Float {
                min: float("min"),
                max: float("max"),
            },
            "string" => SettingKind::String,
            "key" => SettingKind::Key,
            "enum" => {
                let choices: Vec<String> = t
                    .get("choices")
                    .and_then(toml::Value::as_array)
                    .ok_or("enum without `choices`")?
                    .iter()
                    .map(|c| c.as_str().map(str::to_owned).ok_or("non-string choice"))
                    .collect::<Result<_, _>>()?;
                if choices.is_empty() {
                    return Err("enum with no choices".into());
                }
                SettingKind::Enum(choices)
            }
            other => return Err(format!("unknown setting type {other:?}")),
        })
    }
}

/// One setting row in the menu.
#[derive(Debug, Clone, PartialEq)]
pub struct SettingRow {
    /// Key in the mod's config table.
    pub key: String,
    /// Shown name (translated by the host if it can); falls back to `key`.
    pub label: String,
    /// Tooltip; may be empty.
    pub help: String,
    pub kind: SettingKind,
    /// Effective value: Boolean / Integer / Float / String per `kind`
    /// (`Key` and `Enum` are strings).
    pub value: toml::Value,
    pub default: toml::Value,
}

/// One mod in the menu.
#[derive(Debug, Clone, PartialEq)]
pub struct ModRow {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    /// The user's on/off switch.
    pub enabled: bool,
    pub state: ModState,
    pub settings: Vec<SettingRow>,
}

/// What the menu needs from the mod host. Called on the game's render
/// thread while the menu is open (`mods` once per frame), so keep it cheap
/// and don't block. Runs inside the overlay's panic guard: wrap calls into
/// a mod (`on_settings`, ...) in `guard::call(mod_id, ...)`. Must not call
/// back into the overlay (except [`is_open`]).
pub trait OverlayHost: Send + Sync {
    fn mods(&self) -> Vec<ModRow>;
    /// The user flipped a mod's switch. Err is shown under the mod.
    fn set_enabled(&self, mod_id: &str, enabled: bool) -> Result<(), String>;
    /// The user changed a setting (already the right toml type for its
    /// kind; for `Key` a spec string, empty = unbound). The host validates,
    /// saves the config, calls `Mod::on_settings`, and for `Key` settings
    /// calls [`crate::hotkey::set_binding`]. Err is shown under the setting.
    fn set_setting(&self, mod_id: &str, key: &str, value: toml::Value) -> Result<(), String>;
}

/// Receives each key / gamepad press and release while the menu is closed
/// and the game has no text field focused. For a press, return true to
/// swallow it (the host returns true if any mod's `on_input` did); the
/// return value for a release is ignored (the game gets a release exactly
/// when it got the press). Called on the thread that polls SDL events,
/// inside the overlay's panic guard: the host must run each mod's
/// `on_input` in `guard::call(mod_id, ...)`, or a mod's panic turns the
/// overlay off. Must not call back into the overlay (except [`is_open`] and
/// [`crate::hotkey`] functions).
pub type InputSink = fn(&InputEvent) -> bool;

static HOST: RwLock<Option<Arc<dyn OverlayHost>>> = RwLock::new(None);
static SINK: AtomicUsize = AtomicUsize::new(0);
static OPEN: AtomicBool = AtomicBool::new(false);

/// Registers the mod host that fills the menu.
pub fn set_host(host: Arc<dyn OverlayHost>) {
    *HOST.write().unwrap_or_else(|e| e.into_inner()) = Some(host);
}

#[cfg_attr(not(windows), allow(dead_code))] // used by the Windows glue
pub(crate) fn host() -> Option<Arc<dyn OverlayHost>> {
    HOST.read().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Registers where mod input goes (see [`InputSink`]).
pub fn set_input_sink(sink: InputSink) {
    SINK.store(sink as usize, Ordering::Release);
}

#[cfg_attr(not(windows), allow(dead_code))] // used by the Windows glue
pub(crate) fn sink() -> Option<InputSink> {
    let p = SINK.load(Ordering::Acquire);
    // SAFETY: only ever stored from a valid `InputSink` fn pointer.
    (p != 0).then(|| unsafe { std::mem::transmute::<usize, InputSink>(p) })
}

static FRAME_SINK: AtomicUsize = AtomicUsize::new(0);

/// Registers a function called once per game frame on the render thread,
/// before the overlay draws (the host's `on_frame` dispatch). Runs inside
/// the overlay's panic guard, so it must guard each mod call itself.
pub fn set_frame_sink(sink: fn()) {
    FRAME_SINK.store(sink as usize, Ordering::Release);
}

#[cfg_attr(not(windows), allow(dead_code))] // used by the Windows glue
pub(crate) fn frame_sink() -> Option<fn()> {
    let p = FRAME_SINK.load(Ordering::Acquire);
    // SAFETY: only ever stored from a valid `fn()` pointer.
    (p != 0).then(|| unsafe { std::mem::transmute::<usize, fn()>(p) })
}

/// Whether the mod menu is open.
pub fn is_open() -> bool {
    OPEN.load(Ordering::Relaxed)
}

/// Hooks SDL in the game exe. Call once, as early as possible, from the
/// runtime thread (the exe's imports are bound by then). Logs what it did;
/// on failure the game runs without the overlay.
pub fn install() {
    #[cfg(windows)]
    win::install();
    #[cfg(not(windows))]
    crate::log("overlay: not supported on this platform");
}

/// Restores the hooked import slots.
pub fn uninstall() {
    #[cfg(windows)]
    win::uninstall();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(s: &str) -> toml::Table {
        s.parse().unwrap()
    }

    #[test]
    fn setting_kinds_from_manifest() {
        assert_eq!(
            SettingKind::from_manifest(&table("type = 'bool'")),
            Ok(SettingKind::Bool)
        );
        assert_eq!(
            SettingKind::from_manifest(&table("type = 'int'\nmin = 1\nmax = 5")),
            Ok(SettingKind::Int {
                min: Some(1),
                max: Some(5)
            })
        );
        assert_eq!(
            SettingKind::from_manifest(&table("type = 'float'\nmin = 0")),
            Ok(SettingKind::Float {
                min: Some(0.0),
                max: None
            })
        );
        assert_eq!(
            SettingKind::from_manifest(&table("type = 'enum'\nchoices = ['bones', 'health']")),
            Ok(SettingKind::Enum(vec!["bones".into(), "health".into()]))
        );
        assert_eq!(
            SettingKind::from_manifest(&table("type = 'key'")),
            Ok(SettingKind::Key)
        );
        assert!(SettingKind::from_manifest(&table("type = 'enum'")).is_err());
        assert!(SettingKind::from_manifest(&table("type = 'color'")).is_err());
        assert!(SettingKind::from_manifest(&table("default = 1")).is_err());
    }
}
