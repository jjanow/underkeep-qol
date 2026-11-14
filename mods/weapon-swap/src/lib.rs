//! `weapon-swap`: a ranged slot per hero and keys that swap it with the
//! weapon in hand (README, "Mod designs").
//!
//! - The slot holds one weapon, kept by the mod (serialized: def name,
//!   count, cooldown) and saved in the save's sidecar. Only bows go in from
//!   the menu; after a swap it holds the weapon that came out of the hand,
//!   so sword ⇄ bow goes both ways. One of the two is always a bow.
//! - A swap creates the stored weapon and equips it through the game's own
//!   `Hero::EquipItem` (as from the inventory panel: class checks, on_equip /
//!   on_unequip scripts, stats, HUD). The weapon that was in hand lands in
//!   the inventory as with a manual equip; the mod then takes it out of the
//!   game into the slot. A two-handed bow pushes the shield into the
//!   inventory, like a manual equip; swapping back to a one-handed weapon
//!   puts that shield back on if it is still in the inventory.
//! - In combat (the game's own "battle" check) a swap ends the hero's turn
//!   by default (`swap_costs_turn`), through the round action the game uses
//!   for a hero who is done. It needs the hero to be able to act and the
//!   monsters not to be moving. Outside combat it's free.
//! - Putting a weapon into the slot and taking it out are menu buttons (the
//!   mod's section of the overlay menu). The character sheet has no slot
//!   for it (yet): see PLAN.md.
//! - Keys and buttons only queue an action; `on_frame` runs it on the game
//!   thread between frames. Every action logs the item counts of the party
//!   plus the slots before and after: they must stay equal.

mod game;
mod state;

use std::collections::BTreeMap;
use std::path::PathBuf;

use uk_qol_runtime::egui;
use uk_qol_runtime::host::{FrameCtx, LoadCtx, Mod, ModSaveState};
use uk_qol_runtime::hotkey;
use uk_qol_runtime::input::InputEvent;

use game::{Game, Readiness, SLOT_WEAPON, busy_reason};
use state::{HEROES, Loaded, State, StoredItem};

const ID: &str = "weapon-swap";

/// Shown before first use, in the README and by the manager.
pub const NOTICE: &str = "A weapon in a ranged slot is kept by the mod, not by the game.
If you remove the mod, reinstall the game, play the save on another computer,
or the mod's file next to your save is lost, that weapon is gone for good.
Before uninstalling, move every ranged-slot weapon back to your inventory.";

/// File in `uk_qol/` that records the notice was acknowledged (once per
/// install: uninstalling removes the folder).
const ACK_FILE: &str = "weapon-swap.notice-acknowledged";

const HERO_KEYS: [&str; HEROES] = ["swap_hero_1", "swap_hero_2", "swap_hero_3", "swap_hero_4"];
const SELECTED_KEY: &str = "swap_selected";

/// Messages kept for the menu.
const MAX_MESSAGES: usize = 6;

#[derive(Debug, Clone, PartialEq)]
enum Action {
    /// Swap hero `i` (from a key or the menu).
    Swap(usize),
    /// Swap the hero selected in the game.
    SwapSelected,
    /// Put the bow in hero `i`'s hand into their slot.
    StoreEquipped(usize),
    /// Put inventory item (pointer, def) into hero `i`'s slot.
    StoreFromInventory(usize, usize, String),
    /// Move hero `i`'s stored weapon to the inventory.
    ToInventory(usize),
    /// Move parked weapon `k` to the inventory.
    ParkedToInventory(usize),
}

struct Inner {
    game: Game,
    state: State,
    queue: Vec<Action>,
    costs_turn: bool,
    acknowledged: bool,
    messages: Vec<String>,
    /// For the menu: whether the last loaded sidecar didn't match its save.
    mismatch: bool,
    /// For the menu: focus this hero's first button (after a click).
    refocus: Option<usize>,
}

#[derive(Default)]
pub struct WeaponSwap {
    inner: Option<Inner>,
}

fn log(msg: &str) {
    uk_qol_runtime::log(&format!("{ID}: {msg}"));
}

fn ack_path() -> Option<PathBuf> {
    uk_qol_runtime::data_dir().map(|d| d.join(ACK_FILE))
}

fn costs_turn(settings: &toml::Table) -> bool {
    settings
        .get("swap_costs_turn")
        .and_then(toml::Value::as_bool)
        .unwrap_or(true)
}

impl Inner {
    fn say(&mut self, msg: String) {
        log(&msg);
        self.messages.push(msg);
        if self.messages.len() > MAX_MESSAGES {
            self.messages.remove(0);
        }
    }

    /// Party items plus the slots: def → count.
    fn total(&self) -> BTreeMap<String, i64> {
        let mut t = self.game.tally();
        state::tally_stored(&self.state, &mut t);
        t
    }

    fn keys(&self) -> [Option<String>; HEROES] {
        std::array::from_fn(|i| self.game.hero_key(i))
    }

    fn fingerprint(&self) -> String {
        state::fingerprint(&self.game.tally())
    }

    fn run(&mut self, a: Action) {
        let r = self.game.readiness();
        if let Some(why) = busy_reason(&r) {
            self.say(format!("{}: not now, {why} ({})", describe(&a), raw(&r)));
            return;
        }
        log(&format!("{} ({})", describe(&a), raw(&r)));
        let before = self.total();
        let result = self.apply(&a, &r);
        let after = self.total();
        match result {
            Ok(msg) => self.say(msg),
            Err(e) => self.say(format!("{}: {e}", describe(&a))),
        }
        if before == after {
            log(&format!("items conserved ({} kinds)", after.len()));
        } else {
            let mut diff = Vec::new();
            for k in before.keys().chain(after.keys()) {
                let (b, a) = (
                    before.get(k).copied().unwrap_or(0),
                    after.get(k).copied().unwrap_or(0),
                );
                if b != a
                    && !diff
                        .iter()
                        .any(|d: &String| d.starts_with(&format!("{k}:")))
                {
                    diff.push(format!("{k}: {b} -> {a}"));
                }
            }
            log(&format!("ITEM COUNTS CHANGED: {}", diff.join(", ")));
        }
    }

    fn apply(&mut self, a: &Action, r: &Readiness) -> Result<String, String> {
        match a {
            Action::Swap(i) => self.swap(*i, r),
            Action::SwapSelected => {
                let i = self.game.selected_hero().ok_or("no hero selected")?;
                self.swap(i, r)
            }
            Action::StoreEquipped(i) => {
                self.need_ack()?;
                self.need_empty(*i)?;
                let s = self.game.store_equipped(*i)?;
                let msg = format!(
                    "{}: {} is now in the ranged slot",
                    self.game.hero_name(*i),
                    s.def
                );
                self.state.heroes[*i].stored = Some(s);
                Ok(msg)
            }
            Action::StoreFromInventory(i, ptr, def) => {
                self.need_ack()?;
                self.need_empty(*i)?;
                let s = self.game.store_from_inventory(*ptr as game::Item, def)?;
                let msg = format!(
                    "{}: {} is now in the ranged slot",
                    self.game.hero_name(*i),
                    s.def
                );
                self.state.heroes[*i].stored = Some(s);
                Ok(msg)
            }
            Action::ToInventory(i) => {
                let s = self.state.heroes[*i]
                    .stored
                    .clone()
                    .ok_or("the ranged slot is empty")?;
                self.game.to_inventory(&s)?;
                self.state.heroes[*i].stored = None;
                Ok(format!(
                    "{}: {} moved to the inventory",
                    self.game.hero_name(*i),
                    s.def
                ))
            }
            Action::ParkedToInventory(k) => {
                let p = self.state.parked.get(*k).cloned().ok_or("already moved")?;
                self.game.to_inventory(&p.item)?;
                self.state.parked.remove(*k);
                Ok(format!("{} moved to the inventory", p.item.def))
            }
        }
    }

    fn need_ack(&self) -> Result<(), String> {
        if self.acknowledged {
            Ok(())
        } else {
            Err("read and acknowledge the ranged-slot warning in the mod menu first".into())
        }
    }

    fn need_empty(&self, i: usize) -> Result<(), String> {
        if !self.game.hero_valid(i) {
            return Err("no hero there".into());
        }
        if self.state.heroes[i].stored.is_some() {
            return Err("the ranged slot is taken; move its weapon to the inventory first".into());
        }
        Ok(())
    }

    fn swap(&mut self, i: usize, r: &Readiness) -> Result<String, String> {
        // A swap puts the weapon in hand into the slot.
        self.need_ack()?;
        if i >= HEROES || !self.game.hero_valid(i) {
            return Err("no hero there".into());
        }
        let stored = self.state.heroes[i]
            .stored
            .clone()
            .ok_or("the ranged slot is empty")?;
        let battle = self.game.in_battle();
        let charge = battle && self.costs_turn;
        if battle {
            if r.tm_phase != 0 {
                return Err("wait for the monsters' turn to end".into());
            }
            if charge && !self.game.hero_can_act(i) {
                return Err("this hero can't act any more this round".into());
            }
        }
        let res = self.game.swap(i, &stored)?;
        let name = self.game.hero_name(i);
        let slot = &mut self.state.heroes[i];
        slot.stored = res.out_of_hand.clone();
        let mut msg = format!(
            "{name}: swapped to {}{}",
            stored.def,
            res.out_of_hand
                .as_ref()
                .map(|o| format!("; {} is in the ranged slot", o.def))
                .unwrap_or_default()
        );
        if let Some(w) = res.warning {
            log(&w);
        }
        if let Some(sh) = res.shield_out {
            msg.push_str(&format!("; {sh} went to the inventory"));
            slot.shield = Some(sh);
        } else if let Some(sh) = slot.shield.take()
            && self.game.reequip_shield(i, &sh)
        {
            msg.push_str(&format!("; {sh} back on"));
        }
        if charge {
            self.game.end_turn(i);
            msg.push_str(" (turn used)");
        }
        Ok(msg)
    }
}

fn describe(a: &Action) -> String {
    match a {
        Action::Swap(i) => format!("swap hero {}", i + 1),
        Action::SwapSelected => "swap selected hero".into(),
        Action::StoreEquipped(i) => format!("store hero {}'s bow", i + 1),
        Action::StoreFromInventory(i, _, d) => format!("put {d} in hero {}'s slot", i + 1),
        Action::ToInventory(i) => format!("move hero {}'s slot weapon to the inventory", i + 1),
        Action::ParkedToInventory(_) => "move a parked weapon to the inventory".into(),
    }
}

fn raw(r: &Readiness) -> String {
    format!(
        "state {} ui {} target {} drag {} turn {}/{}",
        r.game_state, r.ui_mode, r.target_mode, r.drag, r.tm_phase, r.tm_count
    )
}

impl Mod for WeaponSwap {
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
        let base = cx.base;
        let pats = cx.patterns;
        // SAFETY: addresses from this mod's verified patterns.
        let game = unsafe {
            Game::new(|n| {
                pats.get(n)
                    .map(|&rva| base + rva as usize)
                    .ok_or_else(|| format!("pattern {n} not resolved"))
            })
        }?;
        let acknowledged = ack_path().is_some_and(|p| p.exists());
        // Turning the mod off and on again keeps the slots of this session.
        let state = self.inner.take().map(|i| i.state).unwrap_or_default();
        self.inner = Some(Inner {
            game,
            state,
            queue: Vec::new(),
            costs_turn: costs_turn(cx.settings),
            acknowledged,
            messages: Vec::new(),
            mismatch: false,
            refocus: None,
        });
        Ok(())
    }

    fn on_unload(&mut self) {
        if let Some(i) = &self.inner
            && !i.state.is_empty()
        {
            log("turned off with weapons in ranged slots; they stay with the mod for this session");
        }
    }

    fn on_settings(&mut self, settings: &toml::Table) {
        if let Some(i) = &mut self.inner {
            i.costs_turn = costs_turn(settings);
        }
    }

    fn on_input(&mut self, ev: &InputEvent) -> bool {
        let Some(inner) = &mut self.inner else {
            return false;
        };
        for (i, k) in HERO_KEYS.iter().enumerate() {
            if hotkey::pressed(ID, k, ev) {
                inner.queue.push(Action::Swap(i));
                return true;
            }
        }
        if hotkey::pressed(ID, SELECTED_KEY, ev) {
            inner.queue.push(Action::SwapSelected);
            return true;
        }
        false
    }

    fn on_frame(&mut self, _cx: &mut FrameCtx) {
        let Some(inner) = &mut self.inner else { return };
        for a in std::mem::take(&mut inner.queue) {
            inner.run(a);
        }
    }

    fn on_save(&mut self, st: &mut ModSaveState) {
        let Some(inner) = &self.inner else { return };
        *st = inner.state.to_save(&inner.keys(), &inner.fingerprint());
    }

    fn on_load_save(&mut self, st: &ModSaveState) {
        let Some(inner) = &mut self.inner else { return };
        inner.queue.clear();
        let keys = inner.keys();
        let fp = inner.fingerprint();
        let mut warns = Vec::new();
        let loaded = State::from_save(st, &keys, &fp, &mut |w| warns.push(w));
        for w in warns {
            log(&w);
        }
        match loaded {
            Loaded::Ok(s) => {
                inner.mismatch = false;
                let n = s.heroes.iter().filter(|h| h.stored.is_some()).count();
                log(&format!(
                    "save loaded: {n} ranged slot(s) filled, {} parked",
                    s.parked.len()
                ));
                inner.state = s;
            }
            Loaded::Mismatch(s) => {
                inner.mismatch = true;
                inner.say(format!(
                    "this save's mod file doesn't match the save (saved while the mod was off?); \
                     {} weapon(s) parked in the mod menu",
                    s.parked.len()
                ));
                inner.state = s;
            }
        }
    }

    fn menu(&mut self, ui: &mut egui::Ui) {
        let Some(inner) = &mut self.inner else { return };
        menu(inner, ui);
    }
}

const WARN: egui::Color32 = egui::Color32::from_rgb(230, 180, 80);

fn menu(inner: &mut Inner, ui: &mut egui::Ui) {
    ui.add_space(6.0);
    if !inner.acknowledged {
        ui.colored_label(WARN, NOTICE);
        if ui.button("I understand").clicked() {
            let ok = ack_path().is_some_and(|p| std::fs::write(&p, "acknowledged\n").is_ok());
            if !ok {
                log("couldn't write the acknowledgement file; asking again next time");
            }
            inner.acknowledged = true;
        }
        return;
    }
    egui::CollapsingHeader::new("About the ranged slot (read before uninstalling)")
        .id_salt("ws-notice")
        .show(ui, |ui| {
            ui.colored_label(WARN, NOTICE);
        });
    for m in &inner.messages {
        ui.weak(m);
    }
    if !inner.game.playing() {
        ui.label("Load a game to use the ranged slots.");
        return;
    }
    if !inner.state.parked.is_empty() {
        ui.separator();
        ui.colored_label(
            WARN,
            if inner.mismatch {
                "Weapons from this save's mod file that don't match the save. They may \
                 already be in your party: check before moving them to the inventory."
            } else {
                "Weapons from ranged slots of heroes not in the party:"
            },
        );
        for (k, p) in inner.state.parked.iter().enumerate() {
            ui.horizontal(|ui| {
                ui.label(format!("{} ({})", item_label(&p.item), p.key));
                if ui.button("Move to inventory").clicked() {
                    inner.queue.push(Action::ParkedToInventory(k));
                }
            });
        }
    }
    let inventory: Vec<game::ItemInfo> = inner
        .game
        .inventory()
        .into_iter()
        // SAFETY: inventory items are live.
        .filter_map(|it| unsafe { inner.game.item_info(it) })
        .filter(|i| i.bow && i.slot_problem().is_none())
        .collect();
    for h in 0..HEROES {
        if !inner.game.hero_valid(h) {
            continue;
        }
        let hand = inner.game.equipped(h, SLOT_WEAPON);
        // SAFETY: the equipped weapon.
        let hand = unsafe { inner.game.item_info(hand) };
        let stored = inner.state.heroes[h].stored.clone();
        ui.horizontal_wrapped(|ui| {
            ui.strong(format!("{}. {}", h + 1, inner.game.hero_name(h)));
            ui.label(format!(
                "hand: {}  |  ranged slot: {}",
                hand.as_ref().map_or("nothing".into(), |i| i.def.clone()),
                stored.as_ref().map_or("empty".into(), item_label)
            ));
        });
        // Buttons as (label, action), one per row: the d-pad moves between
        // rows reliably, not between buttons of one row. A click changes
        // which buttons exist, so the focus moves to this hero's first
        // button next frame.
        let mut buttons: Vec<(String, Action)> = Vec::new();
        match &stored {
            Some(_) => {
                buttons.push(("Swap now".into(), Action::Swap(h)));
                buttons.push(("Move to inventory".into(), Action::ToInventory(h)));
            }
            None => {
                if hand.as_ref().is_some_and(|i| i.bow) {
                    buttons.push((
                        "Put the bow in hand in the slot".into(),
                        Action::StoreEquipped(h),
                    ));
                }
                for b in &inventory {
                    buttons.push((
                        format!("Put {} in the slot", b.def),
                        Action::StoreFromInventory(h, b.ptr, b.def.clone()),
                    ));
                }
            }
        }
        ui.vertical(|ui| {
            for (k, (label, action)) in buttons.into_iter().enumerate() {
                let r = ui.button(label);
                if k == 0 && inner.refocus == Some(h) {
                    r.request_focus();
                    inner.refocus = None;
                }
                if r.clicked() {
                    inner.queue.push(action);
                    inner.refocus = Some(h);
                }
            }
        });
    }
}

fn item_label(s: &StoredItem) -> String {
    if s.count > 1 {
        format!("{} x{}", s.def, s.count)
    } else {
        s.def.clone()
    }
}

/// Constructor passed to `uk_qol_runtime::start`.
pub fn new() -> Box<dyn Mod> {
    Box::new(WeaponSwap::default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use uk_qol_runtime::host::{Manifest, PatternFile};

    #[test]
    fn manifest_and_patterns_parse() {
        let m = new();
        let manifest = Manifest::parse(m.manifest()).unwrap();
        assert_eq!(manifest.id, m.id());
        for k in HERO_KEYS.iter().chain([&SELECTED_KEY]) {
            assert!(manifest.setting(k).is_some(), "{k}");
        }
        assert!(manifest.setting("swap_costs_turn").is_some());
        let pats = PatternFile::parse(m.patterns()).unwrap();
        assert_eq!(pats.patterns.len(), 15);
    }

    #[test]
    fn notice_matches_readme() {
        let readme = include_str!("../../../README.md");
        assert!(readme.contains(NOTICE), "README's player notice changed");
    }

    /// Resolves every pattern against the game's exe when it's installed
    /// (skipped otherwise) and checks the addresses found in 1.0.5.
    #[test]
    fn patterns_resolve_on_installed_game() {
        use uk_qol_runtime::host::resolve_pattern;
        use uk_qol_runtime::pattern::Image;
        let Some(home) = std::env::var_os("HOME") else {
            return;
        };
        let exe =
            PathBuf::from(home).join(".local/share/Steam/steamapps/common/Underkeep/underkeep.exe");
        let Ok(file) = std::fs::read(&exe) else {
            return;
        };
        let mapped = uk_qol_runtime::pe::map_file(&file).unwrap();
        let img = Image::new(&mapped).unwrap();
        let expected: BTreeMap<&str, u32> = [
            ("hero_equip_item", 0x17ddb0),
            ("hero_unequip", 0x17ebf0),
            ("item_create", 0x140f00),
            ("item_take", 0x141ed0),
            ("inventory_add", 0x198b90),
            ("in_battle", 0x191600),
            ("round_action_push", 0x1c83c0),
            ("turn_manager", 0x43f410),
            ("party", 0x43f620),
            ("party_inventory", 0x4402e8),
            ("selected_hero", 0x44011c),
            ("game_state", 0x43ca50),
            ("ui_mode", 0x440430),
            ("cursor", 0x442bc8),
            ("target_mode", 0x441384),
        ]
        .into_iter()
        .collect();
        let pats = PatternFile::parse(include_str!("../patterns.toml")).unwrap();
        let build = uk_qol_runtime::build::detect_from_file(&exe)
            .unwrap()
            .sha256_hex();
        if build != "d3158ad0f94d6770deb64b32d48aeb412a3c5ef3a0b2e37a0e092a7ce66bb4a4" {
            return; // another game version
        }
        for p in &pats.patterns {
            let r = resolve_pattern(&img, p, Some(&build))
                .unwrap_or_else(|e| panic!("{}: {e}", p.name));
            assert_eq!(Some(&r.rva), expected.get(p.name.as_str()), "{}", p.name);
        }
        eprintln!(
            "{} patterns resolved on the installed game",
            pats.patterns.len()
        );
    }
}
