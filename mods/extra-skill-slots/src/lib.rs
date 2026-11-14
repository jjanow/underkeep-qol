//! `extra-skill-slots`: more active-skill slots per hero (README, "Mod
//! designs").
//!
//! The game gives each hero one selected skill, a pair at Hero+0x278 (see
//! [`game`]). Using it queues a round action ("hero N uses their skill");
//! when that action runs, `_DoRoundAction` checks the pair with CanCastSkill
//! and casts it through `UseSelectedSkill`. Both read the pair only then.
//!
//! An extra slot holds another skill. Its key queues the same action the HUD
//! skill button queues, for the selected hero, and remembers the action's
//! sequence number. The `_DoRoundAction` detour recognises that action on
//! its first run, puts the slot's pair into Hero+0x278 for exactly that call
//! and puts the original pair back right after. Every check, cost, cooldown
//! and message is the game's own; skills that need a target carry their own
//! copy of the pair into target selection, so nothing is left swapped while
//! the player picks a target or cancels, and a save never sees the swap.
//!
//! Skills are assigned in this mod's section of the overlay menu. Slots are
//! stored by skill name (item skills by equipment slot and name), per hero,
//! in the save's sidecar.

mod game;

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use uk_qol_runtime::egui;
use uk_qol_runtime::host::{FrameCtx, LoadCtx, Mod, ModSaveState};
use uk_qol_runtime::input::InputEvent;
use uk_qol_runtime::{guard, hotkey};

use game::{Game, HEROES, Skill};

const ID: &str = "extra-skill-slots";
/// Most extra slots per hero (the `slots` setting's maximum).
pub const MAX_SLOTS: usize = 5;
const DEFAULT_SLOTS: usize = 3;
const KEYS: [&str; MAX_SLOTS] = [
    "slot_1_key",
    "slot_2_key",
    "slot_3_key",
    "slot_4_key",
    "slot_5_key",
];
/// Queued extra-slot actions remembered at most (older ones were dropped by
/// the game, e.g. by loading a save).
const MAX_PENDING: usize = 8;

/// One hero's extra slots.
type Slots = [Option<Skill>; MAX_SLOTS];

/// An extra-slot action waiting in the game's round queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Pending {
    seq: i32,
    hero: usize,
    slot: usize,
}

/// State shared with the detour (the game thread).
struct Shared {
    game: Option<Game>,
    /// Slots by hero key ([`game::hero_key`]).
    assigned: BTreeMap<String, Slots>,
    pending: VecDeque<Pending>,
}

static SHARED: Mutex<Shared> = Mutex::new(Shared {
    game: None,
    assigned: BTreeMap::new(),
    pending: VecDeque::new(),
});
static ORIG_ROUND_ACTION: AtomicUsize = AtomicUsize::new(0);

fn shared() -> std::sync::MutexGuard<'static, Shared> {
    SHARED.lock().unwrap_or_else(|e| e.into_inner())
}

fn log(msg: &str) {
    uk_qol_runtime::log(&format!("{ID}: {msg}"));
}

type RoundActionFn = extern "win64" fn(usize, usize, f32);

/// Detour of `TurnManager::_DoRoundAction(this, action, dt)`.
extern "win64" fn on_round_action(tm: usize, act: usize, dt: f32) {
    // SAFETY: set to the trampoline before the hook is enabled.
    let orig: RoundActionFn =
        unsafe { std::mem::transmute(ORIG_ROUND_ACTION.load(Ordering::Relaxed)) };
    // Decide inside the guard; the swap and the original call stay outside
    // it, so a panic can never run the action twice.
    // SAFETY: `act` is the game's action being run.
    match guard::call(ID, || unsafe { take_swap(act) }).flatten() {
        Some((field, pair)) => {
            // SAFETY: `field` is Hero+0x278 of a party hero (static memory).
            unsafe {
                let old = game::rd::<u64>(field);
                game::wr::<u64>(field, pair);
                orig(tm, act, dt);
                game::wr::<u64>(field, old);
            }
        }
        None => orig(tm, act, dt),
    }
}

/// If `act` is one of ours running for the first time: the address of the
/// hero's selected-skill pair and the pair to cast instead.
unsafe fn take_swap(act: usize) -> Option<(usize, u64)> {
    if act == 0 {
        return None;
    }
    // SAFETY: fields of a live RoundAction.
    let (ty, hero, use_skill, step, seq) = unsafe {
        (
            game::rd::<i32>(act + game::ACT_TYPE),
            game::rd::<i32>(act + game::ACT_HERO),
            game::rd::<i32>(act + game::ACT_USE_SKILL),
            game::rd::<i32>(act + game::ACT_STEP),
            game::rd::<i32>(act + game::ACT_SEQ),
        )
    };
    if ty != game::ACT_HERO_ACTS || use_skill != 1 || step != 0 {
        return None;
    }
    let mut sh = shared();
    let at = sh.pending.iter().position(|p| p.seq == seq)?;
    let p = sh.pending.remove(at)?;
    let g = sh.game?;
    if hero != p.hero as i32 {
        log(&format!(
            "action {seq}: hero {hero} isn't the queued {}",
            p.hero
        ));
        return None;
    }
    let h = g.hero(p.hero);
    // SAFETY: a hero in the static party array; read on the game thread.
    let (key, name) = unsafe { (game::hero_key(h), game::hero_name(h)) };
    let skill = sh.assigned.get(&key).and_then(|s| s[p.slot].clone());
    // SAFETY: as above.
    let pair = skill.as_ref().and_then(|s| unsafe { game::resolve(h, s) });
    let orig = unsafe { game::rd::<u64>(h + game::H_SELECTED_SKILL) };
    log(&format!(
        "slot {} runs for {name}: {} (pair {}, original {:#x}, thread {:?})",
        p.slot + 1,
        skill
            .as_ref()
            .map_or("nothing assigned", |s| s.name.as_str()),
        pair.map_or("none".into(), |p| format!("{p:#x}")),
        orig,
        std::thread::current().id(),
    ));
    // Without a castable skill the game says "no skill selected".
    Some((h + game::H_SELECTED_SKILL, pair.unwrap_or(game::NO_SKILL)))
}

pub struct ExtraSkillSlots {
    slots: usize,
    /// Slot keys pressed since the last frame.
    presses: Vec<usize>,
    /// The slot whose skill list is open in the menu: (hero key, slot).
    picker: Option<(String, usize)>,
    /// Shown names by skill def name.
    names: HashMap<String, String>,
}

impl ExtraSkillSlots {
    fn apply_settings(&mut self, s: &toml::Table) {
        self.slots = s
            .get("slots")
            .and_then(toml::Value::as_integer)
            .map_or(DEFAULT_SLOTS, |n| n.clamp(1, MAX_SLOTS as i64) as usize);
    }

    /// Queues extra slot `slot` of the selected hero, as the HUD's skill
    /// button does for the hero's own slot.
    fn activate(&mut self, slot: usize) {
        let mut sh = shared();
        let Some(g) = sh.game else { return };
        // SAFETY: game thread (frame hook), static globals.
        let Some(hero) = (unsafe { g.selected() }) else {
            log("no hero selected");
            return;
        };
        let h = g.hero(hero);
        // SAFETY: a valid party hero.
        let (key, name) = unsafe { (game::hero_key(h), game::hero_name(h)) };
        if sh
            .assigned
            .get(&key)
            .and_then(|s| s[slot].as_ref())
            .is_none()
        {
            log(&format!("slot {}: nothing assigned for {name}", slot + 1));
            return;
        }
        // SAFETY: game thread.
        if !unsafe { g.can_queue_hero_action() } {
            log(&format!(
                "slot {}: the game isn't taking actions now",
                slot + 1
            ));
            return;
        }
        // SAFETY: game thread, in game (the gate passed).
        let Some(seq) = (unsafe { g.queue_use_skill(hero) }) else {
            return;
        };
        if sh.pending.len() >= MAX_PENDING {
            sh.pending.pop_front();
        }
        sh.pending.push_back(Pending { seq, hero, slot });
        log(&format!(
            "slot {} queued for {name} (action {seq}, thread {:?})",
            slot + 1,
            std::thread::current().id()
        ));
    }

    fn display(&mut self, g: &Game, skill: &Skill) -> String {
        if let Some(n) = self.names.get(&skill.name) {
            return n.clone();
        }
        // SAFETY: called from the menu, on the game's render thread.
        let n = unsafe { g.text(&skill.name) }.unwrap_or_else(|| game::pretty(&skill.name));
        self.names.insert(skill.name.clone(), n.clone());
        n
    }

    fn draw_menu(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        ui.strong("Extra slots");
        let Some(g) = shared().game else { return };
        let heroes: Vec<usize> = (0..HEROES)
            // SAFETY: static party array.
            .filter(|&i| unsafe { game::hero_valid(g.hero(i)) })
            .collect();
        if heroes.is_empty() {
            ui.weak("Load a game to assign skills.");
            return;
        }
        ui.weak("Pick a skill for each slot. A slot's key casts it for the selected hero.");
        // SAFETY: static global.
        let selected = unsafe { g.selected() };
        for i in heroes {
            let h = g.hero(i);
            // SAFETY: a valid party hero; render thread = game thread.
            let (key, name, skills) = unsafe {
                (
                    game::hero_key(h),
                    game::hero_name(h),
                    game::assignable_skills(h),
                )
            };
            let title = if selected == Some(i) {
                format!("{name} (selected)")
            } else {
                name
            };
            ui.add_space(4.0);
            ui.label(egui::RichText::new(title).strong());
            for slot in 0..self.slots {
                let current = shared().assigned.get(&key).and_then(|s| s[slot].clone());
                let label = match &current {
                    None => "empty".to_owned(),
                    Some(s) => {
                        let n = self.display(&g, s);
                        // SAFETY: as above.
                        if unsafe { game::resolve(h, s) }.is_some() {
                            n
                        } else {
                            format!("{n} (not available)")
                        }
                    }
                };
                let open = self.picker.as_ref() == Some(&(key.clone(), slot));
                ui.horizontal(|ui| {
                    ui.label(format!("Slot {}: {label}", slot + 1));
                    if ui.button(if open { "Close" } else { "Change" }).clicked() {
                        self.picker = (!open).then(|| (key.clone(), slot));
                    }
                });
                if !open {
                    continue;
                }
                let mut pick: Option<Option<Skill>> = None;
                ui.indent(("pick", i, slot), |ui| {
                    if skills.is_empty() {
                        ui.weak("No active skills learned yet.");
                    }
                    for s in &skills {
                        let mut n = self.display(&g, s);
                        if s.equip_slot >= 0 {
                            n.push_str(" (item)");
                        }
                        if ui
                            .selectable_label(current.as_ref() == Some(s), n)
                            .clicked()
                        {
                            pick = Some(Some(s.clone()));
                        }
                    }
                    if current.is_some() && ui.button("Clear").clicked() {
                        pick = Some(None);
                    }
                });
                if let Some(p) = pick {
                    log(&format!(
                        "{key}: slot {} = {}",
                        slot + 1,
                        p.as_ref().map_or("empty", |s| s.name.as_str())
                    ));
                    shared().assigned.entry(key.clone()).or_default()[slot] = p;
                    self.picker = None;
                }
            }
        }
    }
}

impl Mod for ExtraSkillSlots {
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
        let at = |name: &str| {
            cx.patterns
                .get(name)
                .map(|rva| cx.base + *rva as usize)
                .ok_or_else(|| format!("pattern {name} not resolved"))
        };
        let g = Game {
            do_round_action: at("do_round_action")?,
            push_round_action: at("push_round_action")?,
            turn_manager: at("g_turn_manager")?,
            can_queue: at("can_queue_hero_action")?,
            selected_hero: at("g_selected_hero")?,
            heroes: at("g_heroes")?,
            text_get: at("text_get").ok(),
        };
        // SAFETY: `do_round_action` is the resolved function start; the
        // detour has its signature (this, action, dt).
        let tramp = unsafe {
            cx.hooks
                .detour(g.do_round_action, on_round_action as RoundActionFn as usize)
        }
        .map_err(|e| e.to_string())?;
        ORIG_ROUND_ACTION.store(tramp, Ordering::Relaxed);
        self.apply_settings(cx.settings);
        let mut sh = shared();
        sh.game = Some(g);
        sh.pending.clear();
        Ok(())
    }

    fn on_unload(&mut self) {
        let mut sh = shared();
        sh.game = None;
        sh.pending.clear();
        self.presses.clear();
        self.picker = None;
    }

    fn on_settings(&mut self, s: &toml::Table) {
        self.apply_settings(s);
    }

    fn on_input(&mut self, ev: &InputEvent) -> bool {
        for (slot, key) in KEYS.iter().enumerate().take(self.slots) {
            if hotkey::pressed(ID, key, ev) {
                self.presses.push(slot);
                return true;
            }
        }
        false
    }

    fn on_frame(&mut self, _cx: &mut FrameCtx) {
        for slot in std::mem::take(&mut self.presses) {
            self.activate(slot);
        }
    }

    fn on_save(&mut self, state: &mut ModSaveState) {
        state.heroes = save_heroes(&shared().assigned);
    }

    fn on_load_save(&mut self, state: &ModSaveState) {
        let mut sh = shared();
        sh.assigned = load_heroes(&state.heroes);
        sh.pending.clear();
        self.picker = None;
    }

    fn menu(&mut self, ui: &mut egui::Ui) {
        self.draw_menu(ui);
    }
}

/// Sidecar form: per hero `slots = [{ skill = "skill_x" }, {}, { skill =
/// "skill_y", item_slot = 4 }]` (an empty table is an empty slot).
fn save_heroes(assigned: &BTreeMap<String, Slots>) -> BTreeMap<String, toml::Table> {
    assigned
        .iter()
        .filter(|(_, s)| s.iter().any(Option::is_some))
        .map(|(k, slots)| {
            let arr = slots
                .iter()
                .map(|s| {
                    let mut t = toml::Table::new();
                    if let Some(s) = s {
                        t.insert("skill".into(), s.name.clone().into());
                        if s.equip_slot >= 0 {
                            t.insert("item_slot".into(), i64::from(s.equip_slot).into());
                        }
                    }
                    toml::Value::Table(t)
                })
                .collect::<Vec<_>>();
            let mut t = toml::Table::new();
            t.insert("slots".into(), toml::Value::Array(arr));
            (k.clone(), t)
        })
        .collect()
}

fn load_heroes(heroes: &BTreeMap<String, toml::Table>) -> BTreeMap<String, Slots> {
    let mut out = BTreeMap::new();
    for (k, t) in heroes {
        let Some(arr) = t.get("slots").and_then(toml::Value::as_array) else {
            continue;
        };
        let mut slots: Slots = Default::default();
        for (i, v) in arr.iter().take(MAX_SLOTS).enumerate() {
            let Some(name) = v.get("skill").and_then(toml::Value::as_str) else {
                continue;
            };
            let equip_slot = v
                .get("item_slot")
                .and_then(toml::Value::as_integer)
                .and_then(|n| i32::try_from(n).ok())
                .unwrap_or(-1);
            slots[i] = Some(Skill {
                name: name.to_owned(),
                equip_slot,
            });
        }
        out.insert(k.clone(), slots);
    }
    out
}

/// Constructor passed to `uk_qol_runtime::start`.
pub fn new() -> Box<dyn Mod> {
    Box::new(ExtraSkillSlots {
        slots: DEFAULT_SLOTS,
        presses: Vec::new(),
        picker: None,
        names: HashMap::new(),
    })
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
        assert_eq!(manifest.settings.len(), 1 + MAX_SLOTS);
        for k in KEYS {
            assert!(manifest.setting(k).is_some(), "{k}");
        }
        let pats = PatternFile::parse(m.patterns()).unwrap();
        assert_eq!(pats.patterns.len(), 7);
    }

    #[test]
    fn save_round_trip() {
        let mut a = BTreeMap::new();
        let mut s: Slots = Default::default();
        s[0] = Some(Skill {
            name: "skill_fireball".into(),
            equip_slot: -1,
        });
        s[2] = Some(Skill {
            name: "skill_holy_light".into(),
            equip_slot: 1,
        });
        a.insert("0:Ana".to_owned(), s);
        a.insert("1:Bo".to_owned(), Default::default());
        let saved = save_heroes(&a);
        // Heroes with nothing assigned aren't written.
        assert_eq!(saved.len(), 1);
        let text = toml::to_string(&saved["0:Ana"]).unwrap();
        assert!(text.contains("item_slot = 1"), "{text}");
        let back = load_heroes(&saved);
        assert_eq!(back["0:Ana"], a["0:Ana"]);
    }

    #[test]
    fn load_ignores_junk() {
        let t: toml::Table =
            "slots = [{ skill = 3 }, 7, { skill = 'skill_x', item_slot = 99999999999 }]"
                .parse()
                .unwrap();
        let mut m = BTreeMap::new();
        m.insert("0:A".to_owned(), t);
        let s = &load_heroes(&m)["0:A"];
        assert_eq!(s[0], None);
        assert_eq!(s[1], None);
        assert_eq!(s[2].as_ref().map(|s| s.equip_slot), Some(-1));
    }
}
