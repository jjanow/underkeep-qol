//! Mod host (PLAN §2): the `Mod` trait, mod manifests (`mod.toml`) and
//! patterns (`patterns.toml`), loading and unloading.
//!
//! Built-in mods are crates under `mods/`; the proxy passes them to
//! [`crate::start`]. Interface fixed; implementation pending.

use std::collections::BTreeMap;

use crate::hook::HookSet;
use crate::input::InputEvent;
use crate::pattern::Image;

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
    fn on_load(&mut self, cx: &mut LoadCtx) -> Result<(), String>;
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
