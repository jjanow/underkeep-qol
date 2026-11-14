//! `inventory-sort`: sorts the party inventory with one key press (README,
//! "Mod designs").
//!
//! The game has one party inventory (heroes only hold their equipment) and
//! keeps it grouped by category itself: the inventory panel sorts the
//! entries by (category, sequence number) whenever it rebuilds. The mod
//! renumbers the sequence numbers in its own order (see `sort`), moves the
//! entries into that order and flags the panels to rebuild. Stackable items
//! that ended up in two stacks are merged first, through the game's own
//! remove and add. Saves write the inventory in array order, so the order
//! is kept in the save; nothing goes into a sidecar.
//!
//! The hotkey is read in `on_input` (the `SDL_PollEvent` hook) and the sort
//! runs at the end of that frame in `on_frame` (the `SDL_GL_SwapWindow`
//! hook); both run on the game's main thread. The key is swallowed only when
//! a sort is allowed: in game with no menu or dialog open, in the HUD modes
//! where the party inventory is on screen, and with no mouse press or drag
//! in progress (an item being dragged).

mod game;
mod sort;

use std::time::{Duration, Instant};

use uk_qol_runtime::host::{FrameCtx, LoadCtx, Mod};
use uk_qol_runtime::input::InputEvent;

use game::{Game, Sorted};

const ID: &str = "inventory-sort";
/// Hotkey settings (both bound to the same action).
const KEYS: [&str; 2] = ["sort_key", "sort_pad"];
/// Don't log "can't sort" more often than this.
const REFUSE_LOG_EVERY: Duration = Duration::from_secs(2);

#[derive(Default)]
pub struct InventorySort {
    game: Option<Game>,
    pending: bool,
    merge: bool,
    debug: bool,
    last_refusal: Option<Instant>,
}

fn log(msg: &str) {
    uk_qol_runtime::log(&format!("{ID}: {msg}"));
}

impl InventorySort {
    fn apply(&mut self, settings: &toml::Table) {
        self.merge = settings
            .get("merge_stacks")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        self.debug = settings
            .get("debug")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
    }

    fn run(&mut self, g: Game) {
        // SAFETY: resolved addresses of this build, on the game thread.
        if let Err(why) = unsafe { g.can_sort() } {
            log(&format!("not sorting: {why}"));
            return;
        }
        if self.debug {
            for l in unsafe { g.dump("before") } {
                log(&l);
            }
        }
        // SAFETY: as above; `can_sort` just passed.
        match unsafe { g.sort(self.merge) } {
            Ok(Sorted::AlreadySorted { entries }) => {
                log(&format!("already sorted ({entries} entries)"))
            }
            Ok(Sorted::Sorted { entries, merged }) => log(&format!(
                "sorted {entries} entries, merged {merged} duplicate stacks"
            )),
            Err(e) => log(&format!("sort failed: {e}")),
        }
        if self.debug {
            for l in unsafe { g.dump("after") } {
                log(&l);
            }
        }
    }
}

impl Mod for InventorySort {
    fn id(&self) -> &'static str {
        ID
    }

    fn manifest(&self) -> &'static str {
        include_str!("../mod.toml")
    }

    fn patterns(&self) -> &'static str {
        include_str!("../patterns.toml")
    }

    fn on_load(&mut self, cx: &mut LoadCtx) -> Result<(), String> {
        let p = |n: &str| {
            cx.patterns
                .get(n)
                .map(|r| cx.base + *r as usize)
                .ok_or(format!("pattern {n} not resolved"))
        };
        let inventory = p("party_inventory")?;
        self.game = Some(Game {
            inventory,
            hud_mode: p("hud_mode")?,
            gui_ctx: p("gui_ctx")?,
            game_state: p("game_state")?,
            menu_state: p("menu_state")?,
            hud_modal: p("hud_modal")?,
            input_blocked: p("input_blocked")?,
            inventory_add: p("inventory_add")?,
            inventory_remove: p("inventory_remove")?,
        });
        self.pending = false;
        self.apply(cx.settings);
        Ok(())
    }

    fn on_unload(&mut self) {
        self.game = None;
        self.pending = false;
    }

    fn on_settings(&mut self, settings: &toml::Table) {
        self.apply(settings);
    }

    fn on_frame(&mut self, _cx: &mut FrameCtx) {
        let Some(g) = self.game else { return };
        if std::mem::take(&mut self.pending) {
            self.run(g);
        }
    }

    fn on_input(&mut self, ev: &InputEvent) -> bool {
        let Some(g) = self.game else { return false };
        if !KEYS
            .iter()
            .any(|k| uk_qol_runtime::hotkey::pressed(ID, k, ev))
        {
            return false;
        }
        // SAFETY: resolved addresses of this build, on the game thread.
        match unsafe { g.can_sort() } {
            Ok(()) => {
                self.pending = true;
                true
            }
            Err(why) => {
                let now = Instant::now();
                if self
                    .last_refusal
                    .is_none_or(|t| now.duration_since(t) >= REFUSE_LOG_EVERY)
                {
                    self.last_refusal = Some(now);
                    log(&format!("key ignored: {why}"));
                }
                false
            }
        }
    }
}

/// Constructor passed to `uk_qol_runtime::start`.
pub fn new() -> Box<dyn Mod> {
    Box::new(InventorySort::default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use uk_qol_runtime::host::{Manifest, PatternFile, SettingKind, resolve_pattern};
    use uk_qol_runtime::pattern::Image;

    /// Game 1.0.5 (Steam).
    const BUILD_105: &str = "d3158ad0f94d6770deb64b32d48aeb412a3c5ef3a0b2e37a0e092a7ce66bb4a4";

    #[test]
    fn manifest_and_patterns_parse() {
        let m = new();
        let manifest = Manifest::parse(m.manifest()).unwrap();
        assert_eq!(manifest.id, m.id());
        for k in KEYS {
            assert_eq!(manifest.setting(k).unwrap().kind, SettingKind::Key);
        }
        // Not S: the game binds S to "move backward".
        let d = manifest.defaults();
        assert_eq!(d["sort_key"].as_str(), Some("Z"));
        assert_eq!(d["sort_pad"].as_str(), Some(""));
        let pats = PatternFile::parse(m.patterns()).unwrap();
        assert_eq!(pats.patterns.len(), 10);
    }

    /// With `UNDERKEEP_DIR` set to a 1.0.5 install: every pattern matches
    /// exactly once (the runtime's own scanner) and lands where the static
    /// analysis says. Skipped without the game or on another build.
    #[test]
    fn patterns_resolve_on_105() {
        let Some(dir) = std::env::var_os("UNDERKEEP_DIR") else {
            return;
        };
        let exe = std::path::Path::new(&dir).join("underkeep.exe");
        let Ok(file) = std::fs::read(&exe) else {
            return;
        };
        let bytes = uk_qol_runtime::pe::map_file(&file).unwrap();
        let img = Image::new(&bytes).unwrap();
        let expected = [
            ("party_inventory", 0x4402e8),
            ("hud_mode", 0x440430),
            ("gui_ctx", 0x43ca68),
            ("game_state", 0x43ca50),
            ("menu_state", 0x43e680),
            ("hud_modal", 0x441384),
            ("input_blocked", 0x53c20),
            ("inventory_add", 0x198b90),
            ("inventory_remove", 0x198f80),
            ("inventory_view", 0x440310),
        ];
        let pats = PatternFile::parse(new().patterns()).unwrap();
        for def in &pats.patterns {
            let r = resolve_pattern(&img, def, Some(BUILD_105));
            let want = expected.iter().find(|(n, _)| *n == def.name).map(|e| e.1);
            match (r, want) {
                (Ok(r), Some(w)) => assert_eq!(r.rva, w, "{}", def.name),
                (r, w) => panic!("{}: {r:?}, expected {w:?}", def.name),
            }
        }
    }
}
