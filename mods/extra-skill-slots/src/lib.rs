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
//! and message is the game's own. A skill that needs a target (an ally)
//! enters target selection instead, and the cast after the pick reads the
//! pair again (Hero::CastSkill reads Hero+0x278 itself), so then the swap is
//! kept until target selection is over (picked or cancelled). Saves happen
//! outside both windows, so they never see the swap.
//!
//! Skills are assigned in this mod's section of the overlay menu. Slots are
//! stored by skill name (item skills by equipment slot and name), per hero,
//! in the save's sidecar.
//!
//! On the HUD, each hero's extra slots are small buttons in a column right
//! of the hero's skill slot, labelled with the skill's initials: left-click
//! casts it for that hero (as its key does for the selected hero),
//! right-click (or left-click on an empty slot) assigns it with the game's
//! skill panel.

mod game;

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use uk_qol_runtime::egui;
use uk_qol_runtime::host::{FrameCtx, LoadCtx, Mod, ModSaveState};
use uk_qol_runtime::input::InputEvent;
use uk_qol_runtime::{guard, hotkey, hud};

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
    /// A swap kept while the player picks a target (see [`Targeting`]).
    targeting: Option<Targeting>,
}

/// A skill that needs a target (an ally) enters target selection instead of
/// casting, and the cast that follows the pick reads Hero+0x278 again. So
/// for such a skill the slot's pair stays in place until target selection
/// has been over for a moment, then the original goes back.
#[derive(Debug, Clone, Copy)]
struct Targeting {
    /// Hero+0x278.
    field: usize,
    orig: u64,
    swapped: u64,
    /// When target selection was first seen over.
    over_since: Option<Instant>,
}

/// How long to keep the swap after target selection ends, so the cast it
/// triggered (an event handled on a following frame) still sees it.
const TARGET_SETTLE: Duration = Duration::from_millis(500);

static SHARED: Mutex<Shared> = Mutex::new(Shared {
    game: None,
    assigned: BTreeMap::new(),
    pending: VecDeque::new(),
    targeting: None,
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
                if !keep_for_targeting(field, old, pair) {
                    game::wr::<u64>(field, old);
                }
            }
        }
        None => orig(tm, act, dt),
    }
}

/// After an extra slot's action ran: true if it entered target selection,
/// in which case the swap stays until [`settle_targeting`] undoes it.
fn keep_for_targeting(field: usize, orig: u64, swapped: u64) -> bool {
    let mut sh = shared();
    let Some(g) = sh.game else { return false };
    // SAFETY: game thread (inside the detour).
    if unsafe { g.interaction_mode() } != Some(game::MODE_TARGET) {
        return false;
    }
    log("waiting for a target; the slot's skill stays selected until then");
    sh.targeting = Some(Targeting {
        field,
        orig,
        swapped,
        over_since: None,
    });
    true
}

/// Puts the original skill back once target selection is over (each frame),
/// or right away with `now` (unload).
fn settle_targeting(now: bool) {
    let mut sh = shared();
    let (Some(g), Some(mut t)) = (sh.game, sh.targeting) else {
        return;
    };
    // SAFETY: game thread (frame hook or unload).
    let targeting = unsafe { g.interaction_mode() } == Some(game::MODE_TARGET);
    if !now {
        t.over_since = if targeting {
            None
        } else {
            Some(t.over_since.unwrap_or_else(Instant::now))
        };
        if t.over_since.is_none_or(|at| at.elapsed() < TARGET_SETTLE) {
            sh.targeting = Some(t);
            return;
        }
    }
    sh.targeting = None;
    // SAFETY: Hero+0x278 of a party hero (static memory).
    unsafe {
        if game::rd::<u64>(t.field) == t.swapped {
            game::wr::<u64>(t.field, t.orig);
        }
    }
    log("target selection over; original skill back");
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
    if sh.pending.is_empty() {
        return None;
    }
    let g = sh.game?;
    // SAFETY: the TurnManager global's sequence counter.
    prune(&mut sh.pending, unsafe { g.action_counter() });
    let at = sh.pending.iter().position(|p| p.seq == seq)?;
    let p = sh.pending.remove(at)?;
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
    /// The `hud_buttons` setting.
    hud_buttons: bool,
    /// Slot keys pressed since the last frame.
    presses: Vec<usize>,
    /// HUD buttons clicked since the last frame: (hero, slot).
    hud_casts: Vec<(usize, usize)>,
    /// The slot whose skill list is open in the menu: (hero key, slot).
    picker: Option<(String, usize)>,
    /// Move keyboard / gamepad focus into the picker next frame.
    focus_picker: bool,
    /// Shown names by skill def name.
    names: HashMap<String, String>,
    /// A slot being assigned through the game's own skill picker panel.
    panel_pick: Option<PanelPick>,
}

/// Assigning a slot with the game's skill picker panel: once the mod menu
/// is closed, the mod opens the hero's panel; the player's pick lands in the
/// hero's own slot, from where the mod moves it to the extra slot and puts
/// the original back when the panel closes.
#[derive(Debug, Clone, PartialEq)]
struct PanelPick {
    key: String,
    hero: usize,
    slot: usize,
    /// When the current stage began.
    since: Instant,
    /// None until the panel is opened; then the hero's original pair.
    orig: Option<u64>,
    /// The panel was seen open.
    seen: bool,
}

/// How long to wait for the game to take input after the menu closes
/// (a dialog may be up) before giving up on opening the panel.
const PANEL_WAIT_INPUT: Duration = Duration::from_secs(30);
/// How long to wait for the panel to show up once requested.
const PANEL_WAIT_OPEN: Duration = Duration::from_secs(2);

impl ExtraSkillSlots {
    fn apply_settings(&mut self, s: &toml::Table) {
        self.slots = s
            .get("slots")
            .and_then(toml::Value::as_integer)
            .map_or(DEFAULT_SLOTS, |n| n.clamp(1, MAX_SLOTS as i64) as usize);
        self.hud_buttons = s
            .get("hud_buttons")
            .and_then(toml::Value::as_bool)
            .unwrap_or(true);
    }

    /// Queues extra slot `slot` of the selected hero (a key press).
    fn activate(&mut self, slot: usize) {
        let Some(g) = shared().game else { return };
        // SAFETY: game thread (frame hook), static globals.
        let Some(hero) = (unsafe { g.selected() }) else {
            log("no hero selected");
            return;
        };
        self.activate_hero(hero, slot);
    }

    /// Queues extra slot `slot` of hero `hero`, as the HUD's skill button
    /// does for the hero's own slot.
    fn activate_hero(&mut self, hero: usize, slot: usize) {
        let mut sh = shared();
        let Some(g) = sh.game else { return };
        if hero >= HEROES {
            return;
        }
        let h = g.hero(hero);
        // SAFETY: inside the static hero array; game thread.
        if !unsafe { game::hero_valid(h) } {
            return;
        }
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
        // SAFETY: game thread; the TurnManager global.
        prune(&mut sh.pending, unsafe { g.action_counter() });
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

    /// Advances a pick through the game's skill panel (see [`PanelPick`]).
    fn panel_step(&mut self) {
        let Some(mut p) = self.panel_pick.take() else {
            return;
        };
        if uk_qol_runtime::overlay::is_open() {
            p.since = Instant::now();
            self.panel_pick = Some(p);
            return;
        }
        let Some(g) = shared().game else { return };
        let h = g.hero(p.hero);
        // SAFETY: game thread (frame hook); a hero in the static array.
        let same_hero = unsafe { game::hero_valid(h) && game::hero_key(h) == p.key };
        match p.orig {
            None => {
                if !same_hero || p.since.elapsed() > PANEL_WAIT_INPUT {
                    log("skill panel: gave up (the game didn't take input)");
                    return;
                }
                // SAFETY: game thread.
                if !unsafe { g.can_queue_hero_action() } {
                    self.panel_pick = Some(p);
                    return;
                }
                // SAFETY: a valid hero; game thread, in game (gate passed).
                // The hero's own slot is emptied while the panel is open, so
                // any pick (even the skill already there) shows as a change.
                unsafe {
                    p.orig = Some(game::rd::<u64>(h + game::H_SELECTED_SKILL));
                    game::wr::<u64>(h + game::H_SELECTED_SKILL, game::NO_SKILL);
                    g.open_skill_panel(p.hero);
                }
                p.since = Instant::now();
                log(&format!("skill panel opened for slot {}", p.slot + 1));
                self.panel_pick = Some(p);
            }
            Some(orig) => {
                // SAFETY: game thread.
                let open = unsafe { g.interaction_mode() } == Some(game::MODE_SKILL_PANEL);
                if open {
                    p.seen = true;
                }
                if open || (!p.seen && p.since.elapsed() <= PANEL_WAIT_OPEN) {
                    self.panel_pick = Some(p);
                    return;
                }
                if !same_hero {
                    return;
                }
                // SAFETY: a valid hero; game thread. The original goes back
                // whatever happened.
                let picked = unsafe {
                    let v = game::rd::<u64>(h + game::H_SELECTED_SKILL);
                    game::wr::<u64>(h + game::H_SELECTED_SKILL, orig);
                    v
                };
                if picked == game::NO_SKILL {
                    log("skill panel closed without a pick");
                    return;
                }
                // SAFETY: as above.
                let skill = unsafe { game::skill_of(h, picked) };
                log(&format!(
                    "{}: slot {} = {} (from the skill panel)",
                    p.key,
                    p.slot + 1,
                    skill.as_ref().map_or("empty", |s| s.name.as_str())
                ));
                if skill.is_some() {
                    shared().assigned.entry(p.key).or_default()[p.slot] = skill;
                }
            }
        }
    }

    /// Starts assigning `slot` of hero `hero` with the game's skill panel.
    fn start_panel_pick(&mut self, key: String, hero: usize, slot: usize) {
        log(&format!("{key}: assigning slot {} from the HUD", slot + 1));
        self.panel_pick = Some(PanelPick {
            key,
            hero,
            slot,
            since: Instant::now(),
            orig: None,
            seen: false,
        });
    }

    /// The HUD buttons (see the module docs).
    fn draw_hud(&mut self, ui: &mut egui::Ui) {
        let Some(g) = shared().game else { return };
        let Some(t) = hud::transform() else { return };
        for hero in 0..HEROES {
            let h = g.hero(hero);
            // SAFETY: inside the static hero array; render thread = game thread.
            if !unsafe { game::hero_valid(h) } {
                continue;
            }
            let Some(skill_slot) = hud::gui_rect(&hud::skill_slot_name(hero)) else {
                continue;
            };
            // SAFETY: as above.
            let key = unsafe { game::hero_key(h) };
            for (slot, cell) in hud_cells(skill_slot, self.slots).into_iter().enumerate() {
                let current = shared().assigned.get(&key).and_then(|s| s[slot].clone());
                let picking = self
                    .panel_pick
                    .as_ref()
                    .is_some_and(|p| p.key == key && p.slot == slot);
                let (text, fill, tip) = match &current {
                    None => (
                        "+".to_owned(),
                        egui::Color32::from_rgb(45, 45, 50),
                        format!("Extra slot {}: empty. Click to assign a skill.", slot + 1),
                    ),
                    Some(s) => {
                        let name = self.display(&g, s);
                        // SAFETY: as above.
                        let ok = unsafe { game::resolve(h, s) }.is_some();
                        (
                            initials(&name),
                            if ok {
                                egui::Color32::from_rgb(40, 70, 120)
                            } else {
                                egui::Color32::from_rgb(70, 70, 70)
                            },
                            format!(
                                "Extra slot {}: {name}{}\nLeft-click: cast. Right-click: assign another skill.",
                                slot + 1,
                                if ok { "" } else { " (not available)" }
                            ),
                        )
                    }
                };
                let fill = if picking {
                    egui::Color32::from_rgb(150, 120, 30)
                } else {
                    fill
                };
                let r = hud::button(ui, t.rect_to_points(cell), &text, fill).on_hover_text(tip);
                if r.secondary_clicked() || (r.clicked() && current.is_none()) {
                    self.start_panel_pick(key.clone(), hero, slot);
                } else if r.clicked() {
                    self.hud_casts.push((hero, slot));
                }
            }
        }
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
        let heroes: Vec<usize> = uk_qol_runtime::savehook::party_heroes()
            .iter()
            .map(|p| p.index)
            .filter(|&i| i < HEROES)
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
                let waiting = self
                    .panel_pick
                    .as_ref()
                    .is_some_and(|p| p.key == key && p.slot == slot);
                ui.horizontal(|ui| {
                    ui.label(format!("Slot {}: {label}", slot + 1));
                    if nav(ui.button(if open { "Close" } else { "Change" })).clicked() {
                        self.picker = (!open).then(|| (key.clone(), slot));
                        self.focus_picker = !open;
                    }
                    if g.panel.is_some()
                        && !waiting
                        && nav(ui.button("Game panel"))
                            .on_hover_text(
                                "Pick with the game's skill panel once this menu is closed.",
                            )
                            .clicked()
                    {
                        self.start_panel_pick(key.clone(), i, slot);
                    }
                });
                if waiting {
                    ui.weak("Close this menu to pick in the game's skill panel.");
                }
                if !open {
                    continue;
                }
                let mut pick: Option<Option<Skill>> = None;
                let focus = std::mem::take(&mut self.focus_picker);
                ui.indent(("pick", i, slot), |ui| {
                    if skills.is_empty() {
                        ui.weak("No active skills learned yet.");
                    }
                    let mut first = true;
                    for s in &skills {
                        let mut n = self.display(&g, s);
                        if s.equip_slot >= 0 {
                            n.push_str(" (item)");
                        }
                        if current.as_ref() == Some(s) {
                            n.push_str("  \u{2713}");
                        }
                        // Buttons, one per line: reachable with the pad's
                        // d-pad, and the first one takes focus on open.
                        let r = nav(ui.button(n));
                        if focus && std::mem::take(&mut first) {
                            r.request_focus();
                            r.scroll_to_me(None);
                        }
                        if r.clicked() {
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
            panel: (|| {
                Some(game::Panel {
                    interaction_mode: at("g_interaction_mode").ok()?,
                    event_init: at("ui_event_init").ok()?,
                    event_post: at("ui_event_post").ok()?,
                    event_free: at("ui_event_free").ok()?,
                })
            })(),
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
        settle_targeting(true);
        let mut sh = shared();
        // A pick in progress: put the hero's own skill back if the panel
        // left the slot empty (otherwise the player's pick stays there).
        if let (
            Some(g),
            Some(PanelPick {
                orig: Some(orig),
                hero,
                ..
            }),
        ) = (sh.game, self.panel_pick.take())
        {
            let at = g.hero(hero) + game::H_SELECTED_SKILL;
            // SAFETY: static party array; unload runs on the game thread.
            unsafe {
                if game::rd::<u64>(at) == game::NO_SKILL {
                    game::wr::<u64>(at, orig);
                }
            }
        }
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
        for (hero, slot) in std::mem::take(&mut self.hud_casts) {
            self.activate_hero(hero, slot);
        }
        self.panel_step();
        settle_targeting(false);
    }

    fn on_save(&mut self, state: &mut ModSaveState) {
        state.heroes = save_heroes(&shared().assigned);
    }

    fn on_load_save(&mut self, state: &ModSaveState) {
        let mut sh = shared();
        sh.assigned = load_heroes(&state.heroes);
        sh.pending.clear();
        // The load rewrote the heroes.
        sh.targeting = None;
        self.picker = None;
    }

    fn menu(&mut self, ui: &mut egui::Ui) {
        self.draw_menu(ui);
    }

    fn has_hud(&self) -> bool {
        self.hud_buttons && shared().game.is_some()
    }

    fn hud(&mut self, ui: &mut egui::Ui) {
        self.draw_hud(ui);
    }
}

/// Largest side of an extra slot's HUD button, in GUI units (the game's
/// slot is 29; the gap right of it about 10).
const HUD_CELL: f32 = 9.0;
/// Space between the skill slot and the buttons, and between buttons.
const HUD_GAP: f32 = 1.0;

/// The HUD buttons of `n` extra slots for a skill slot rect (GUI space): a
/// column right of the slot, as high as the slot.
fn hud_cells(skill_slot: egui::Rect, n: usize) -> Vec<egui::Rect> {
    if n == 0 {
        return Vec::new();
    }
    let side = ((skill_slot.height() - HUD_GAP * (n as f32 - 1.0)) / n as f32).min(HUD_CELL);
    (0..n)
        .map(|i| {
            egui::Rect::from_min_size(
                egui::pos2(
                    skill_slot.max.x + HUD_GAP,
                    skill_slot.min.y + i as f32 * (side + HUD_GAP),
                ),
                egui::vec2(side, side),
            )
        })
        .collect()
}

/// A short label for a skill name: the initials of its first two words, or
/// its first two letters.
fn initials(name: &str) -> String {
    let words: Vec<&str> = name.split_whitespace().collect();
    match words.as_slice() {
        [] => "?".into(),
        [one] => {
            let mut c = one.chars();
            let first: String = c.next().into_iter().flat_map(char::to_uppercase).collect();
            first
                + &c.next()
                    .map(|x| x.to_lowercase().collect::<String>())
                    .unwrap_or_default()
        }
        [a, b, ..] => [a, b]
            .iter()
            .filter_map(|w| w.chars().next())
            .flat_map(char::to_uppercase)
            .collect(),
    }
}

/// Drops remembered actions the queue can no longer hold: the game's action
/// counter went back below their sequence number (the queue was reset), so
/// a later, unrelated action could otherwise reuse the number.
fn prune(pending: &mut VecDeque<Pending>, counter: i32) {
    pending.retain(|p| p.seq <= counter);
}

/// Keeps a widget reached with the d-pad or keyboard in view.
fn nav(r: egui::Response) -> egui::Response {
    if r.gained_focus() {
        r.scroll_to_me(None);
    }
    r
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
        hud_buttons: true,
        presses: Vec::new(),
        hud_casts: Vec::new(),
        picker: None,
        focus_picker: false,
        names: HashMap::new(),
        panel_pick: None,
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
        assert_eq!(manifest.settings.len(), 2 + MAX_SLOTS);
        for k in KEYS {
            assert!(manifest.setting(k).is_some(), "{k}");
        }
        let pats = PatternFile::parse(m.patterns()).unwrap();
        assert_eq!(pats.patterns.len(), 11);
    }

    #[test]
    fn hud_cells_fit_beside_the_slot() {
        let slot = egui::Rect::from_min_size(egui::pos2(526.0, 92.0), egui::vec2(29.0, 29.0));
        for n in 1..=MAX_SLOTS {
            let cells = hud_cells(slot, n);
            assert_eq!(cells.len(), n);
            for c in &cells {
                assert!(c.min.x > slot.max.x && c.width() <= HUD_CELL + 1e-4);
                assert!(
                    c.min.y >= slot.min.y && c.max.y <= slot.max.y + 1e-3,
                    "{n}: {c:?}"
                );
            }
        }
        assert_eq!(hud_cells(slot, 3)[0].width(), HUD_CELL);
        assert!(hud_cells(slot, 0).is_empty());
    }

    #[test]
    fn initials_are_short() {
        assert_eq!(initials("Fireball"), "Fi");
        assert_eq!(initials("blood pact"), "BP");
        assert_eq!(initials("Healing Light of Dawn"), "HL");
        assert_eq!(initials(""), "?");
    }

    #[test]
    fn prune_drops_numbers_past_the_counter() {
        let p = |seq| Pending {
            seq,
            hero: 0,
            slot: 0,
        };
        let mut q: VecDeque<Pending> = [p(180), p(182)].into();
        prune(&mut q, 182);
        assert_eq!(q.len(), 2);
        prune(&mut q, 5);
        assert!(q.is_empty());
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
