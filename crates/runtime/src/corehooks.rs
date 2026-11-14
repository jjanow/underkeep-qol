//! The runtime's own hooks: save hooks ([`crate::savehook`]) and data
//! patches ([`crate::datapatch`]), plus the HUD's addresses
//! ([`crate::hud`]). Their patterns are `patterns/core.toml` (built in;
//! `uk_qol/patterns/core.toml` replaces it whole, like a mod's override),
//! resolved before any mod's hooks change the code. The hooks go in during
//! startup, before the startup gate lets the game's init run, and stay in
//! for the life of the process. Not installed in safe mode.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Mutex;

use crate::hook::HookSet;
use crate::host::{self, PatternFile, RESERVED_ID};
use crate::pattern::Image;

/// The built-in core patterns.
pub const PATTERNS: &str = include_str!("../patterns/core.toml");

/// Resolves the core patterns (override in `patterns_dir` first). Missing
/// ones are logged and left out of the map.
pub fn resolve(
    image: &Image,
    build: Option<&str>,
    patterns_dir: Option<&Path>,
) -> BTreeMap<String, u32> {
    let mut text = PATTERNS.to_owned();
    if let Some(dir) = patterns_dir {
        match host::read_override(dir, RESERVED_ID) {
            None => {}
            Some(Ok(t)) => {
                crate::log(&format!(
                    "core: using patterns from {}",
                    dir.join("core.toml").display()
                ));
                text = t;
            }
            Some(Err(e)) => crate::log(&format!(
                "core: can't read pattern override: {e}; using the built-in patterns"
            )),
        }
    }
    let file = match PatternFile::parse(&text) {
        Ok(f) => f,
        Err(e) if text != PATTERNS => {
            crate::log(&format!(
                "core: bad pattern override ({e}); using the built-in patterns"
            ));
            match PatternFile::parse(PATTERNS) {
                Ok(f) => f,
                Err(_) => return BTreeMap::new(),
            }
        }
        Err(e) => {
            crate::log(&format!("core: bad built-in patterns: {e}"));
            return BTreeMap::new();
        }
    };
    let mut out = BTreeMap::new();
    for def in &file.patterns {
        match host::resolve_pattern(image, def, build) {
            Ok(r) => {
                crate::log(&format!("core: pattern {} at rva {:#x}", def.name, r.rva));
                out.insert(def.name.clone(), r.rva);
            }
            Err(e) => crate::log(&format!("core: pattern {}: {e}", def.name)),
        }
    }
    out
}

/// The core hook set; never dropped (dropping would take the hooks out).
static HOOKS: Mutex<Option<HookSet>> = Mutex::new(None);

/// Registers the save and data-patch hooks and enables them.
pub fn install(
    patterns: &BTreeMap<String, u32>,
    base: usize,
    patches: crate::datapatch::PatchSet,
    log_defs: bool,
) {
    let mut hooks = HookSet::new("core");
    crate::savehook::install(&mut hooks, patterns, base);
    crate::datapatch::install(&mut hooks, patterns, base, patches, log_defs);
    crate::hud::install(patterns, base);
    match hooks.enable() {
        Ok(()) => crate::log("core hooks installed"),
        Err(e) => crate::log(&format!(
            "core hooks: enabling failed: {e}; saves and data patches are off"
        )),
    }
    *HOOKS.lock().unwrap_or_else(|e| e.into_inner()) = Some(hooks);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_patterns_parse() {
        let f = PatternFile::parse(PATTERNS).unwrap();
        let names: Vec<&str> = f.patterns.iter().map(|p| p.name.as_str()).collect();
        for n in [
            "savegame_write",
            "game_load_from_slot",
            "save_delete_file",
            "party_init_from_setup",
            "g_party",
            "deftable_parse",
            "g_gui_layout",
            "g_game_state",
            "g_hud_mode",
            "g_menu_state",
            "g_interaction_mode",
        ] {
            assert!(names.contains(&n), "{n} missing");
        }
        assert!(f.patterns.iter().all(|p| !p.optional));
    }

    /// With `UNDERKEEP_DIR` set, every core pattern resolves on the local exe.
    #[test]
    fn resolves_on_local_exe() {
        let Ok(dir) = std::env::var("UNDERKEEP_DIR") else {
            eprintln!("UNDERKEEP_DIR not set; skipped");
            return;
        };
        let bytes = std::fs::read(Path::new(&dir).join("underkeep.exe")).unwrap();
        let mapped = crate::pe::map_file(&bytes).unwrap();
        let image = Image::new(&mapped).unwrap();
        let f = PatternFile::parse(PATTERNS).unwrap();
        for def in &f.patterns {
            host::resolve_pattern(&image, def, None)
                .unwrap_or_else(|e| panic!("{}: {e}", def.name));
        }
    }
}
