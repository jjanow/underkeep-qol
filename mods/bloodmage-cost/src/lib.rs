//! `bloodmage-cost`: choose per Blood Mage how spells are paid (README,
//! "Mod designs").
//!
//! The game's rule: a Blood Mage spell takes one bone (any party-inventory
//! item of item type 16) and, with no bones, costs health instead. The check
//! lives in two functions, `Hero::CanCastSkill` (may the skill be used now:
//! HUD button state, round action) and `Hero::PaySkillCost` (when it's
//! cast). Both scan the party inventory for a bone and take their own "no
//! bones" path when there is none.
//!
//! For a Blood Mage in `health` mode the mod detours both and, for the
//! duration of the original call only, sets the party inventory's item count
//! to 0, so the bone scan finds nothing and the game's own health path runs
//! (with its "red mask" cost reductions and its "not enough health" check).
//! The count is restored right after, and also at the start of
//! `Hero::LoseHP` (called only from `PaySkillCost`'s health path), so the
//! damage, death handling and anything they trigger see the real inventory.
//! Nothing else runs in between: the game logic is single-threaded, and
//! neither function touches the inventory except through that scan.
//!
//! The mode is per hero, keyed `<hero_id>:<name>`, switched with a hotkey
//! for the selected hero (a toast says the new mode), and kept in the save's
//! sidecar.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering};

use uk_qol_runtime::guard;
use uk_qol_runtime::host::{LoadCtx, Mod, ModSaveState};
use uk_qol_runtime::hotkey;
use uk_qol_runtime::input::InputEvent;
use uk_qol_runtime::toast;

const ID: &str = "bloodmage-cost";
const TOGGLE_KEY: &str = "toggle_key";

// Hero struct (0x2b8 bytes, 4 in the party), 1.0.5.
const HERO_SIZE: usize = 0x2b8;
const PARTY_SIZE: usize = 4;
const HERO_VALID: usize = 0x00; // byte
const HERO_ID: usize = 0x08; // i32
const HERO_NAME_DATA: usize = 0x18; // String {buf, data, len}: data
const HERO_NAME_LEN: usize = 0x20; // i32
const HERO_CLASS: usize = 0x94; // i32
const HERO_HP: usize = 0xfc; // i32
const CLASS_BLOODMAGE: i32 = 6;

// Party inventory: count (i32) at the resolved address, entries pointer 8
// bytes after it; entries are 0x18 bytes, the item pointer first.
const INV_ENTRIES: usize = 0x08;
const INV_ENTRY_SIZE: usize = 0x18;
const ITEM_COUNT: usize = 0xd0; // i32 stack size
const ITEM_TYPE: usize = 0x100; // i32
const ITEM_TYPE_BONE: i32 = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PayMode {
    /// The game's rule: bones first, health when out of bones.
    Bones,
    /// Always health; bones are kept.
    Health,
}

impl PayMode {
    fn parse(s: &str) -> Option<PayMode> {
        match s {
            "bones" => Some(PayMode::Bones),
            "health" => Some(PayMode::Health),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            PayMode::Bones => "bones",
            PayMode::Health => "health",
        }
    }

    fn flip(self) -> PayMode {
        match self {
            PayMode::Bones => PayMode::Health,
            PayMode::Health => PayMode::Bones,
        }
    }
}

/// Per-hero choices and the default for heroes without one.
#[derive(Debug, Default)]
struct Modes {
    by_hero: BTreeMap<String, PayMode>,
    default: Option<PayMode>,
}

impl Modes {
    fn get(&self, key: &str) -> PayMode {
        self.by_hero
            .get(key)
            .copied()
            .unwrap_or(self.default.unwrap_or(PayMode::Bones))
    }

    /// Switches `key` and returns its new mode.
    fn toggle(&mut self, key: &str) -> PayMode {
        let m = self.get(key).flip();
        self.by_hero.insert(key.to_owned(), m);
        m
    }

    fn save(&self, state: &mut ModSaveState) {
        for (k, m) in &self.by_hero {
            let mut t = toml::Table::new();
            t.insert("mode".into(), toml::Value::String(m.name().into()));
            state.heroes.insert(k.clone(), t);
        }
    }

    fn load(&mut self, state: &ModSaveState) {
        self.by_hero = state
            .heroes
            .iter()
            .filter_map(|(k, t)| {
                let m = PayMode::parse(t.get("mode")?.as_str()?)?;
                Some((k.clone(), m))
            })
            .collect();
    }
}

static MODES: Mutex<Modes> = Mutex::new(Modes {
    by_hero: BTreeMap::new(),
    default: None,
});

fn modes() -> std::sync::MutexGuard<'static, Modes> {
    MODES.lock().unwrap_or_else(|e| e.into_inner())
}

fn default_mode(settings: &toml::Table) -> PayMode {
    settings
        .get("default_mode")
        .and_then(toml::Value::as_str)
        .and_then(PayMode::parse)
        .unwrap_or(PayMode::Bones)
}

// ---------------------------------------------------------------------------
// Game memory

/// Addresses resolved in `on_load` (0 = not loaded).
static INV_COUNT: AtomicUsize = AtomicUsize::new(0);
static SELECTED: AtomicUsize = AtomicUsize::new(0);
static HEROES: AtomicUsize = AtomicUsize::new(0);
static ORIG_PAY: AtomicUsize = AtomicUsize::new(0);
static ORIG_CAN: AtomicUsize = AtomicUsize::new(0);
static ORIG_LOSE: AtomicUsize = AtomicUsize::new(0);

/// # Safety
/// `p + off` must be readable.
unsafe fn rd<T: Copy>(p: usize, off: usize) -> T {
    // SAFETY: forwarded.
    unsafe { std::ptr::read_unaligned((p + off) as *const T) }
}

/// A hero's sidecar key, `<hero_id>:<name>`.
///
/// # Safety
/// `h` must point to a live Hero.
unsafe fn hero_key(h: usize) -> String {
    // SAFETY: forwarded; the name String's data is valid for its length.
    unsafe {
        let id: i32 = rd(h, HERO_ID);
        let data: usize = rd(h, HERO_NAME_DATA);
        let len: i32 = rd(h, HERO_NAME_LEN);
        let name = if data == 0 || !(0..=256).contains(&len) {
            String::new()
        } else {
            String::from_utf8_lossy(std::slice::from_raw_parts(data as *const u8, len as usize))
                .into_owned()
        };
        format!("{id}:{name}")
    }
}

/// # Safety
/// `h` must point to a live Hero.
unsafe fn is_bloodmage(h: usize) -> bool {
    // SAFETY: forwarded.
    unsafe { rd::<i32>(h, HERO_CLASS) == CLASS_BLOODMAGE }
}

/// Whether hero `h` pays with health instead of bones.
///
/// # Safety
/// `h` must point to a live Hero.
unsafe fn pays_with_health(h: usize) -> bool {
    // SAFETY: forwarded.
    unsafe { h != 0 && is_bloodmage(h) && modes().get(&hero_key(h)) == PayMode::Health }
}

/// Total bones in the party inventory (for the log).
fn bones() -> i32 {
    let at = INV_COUNT.load(Ordering::Relaxed);
    if at == 0 {
        return 0;
    }
    // SAFETY: the party inventory is a static of the exe; the entries
    // pointer is valid for `count` entries, each item pointer is live.
    unsafe {
        let n: i32 = rd(at, 0);
        let entries: usize = rd(at, INV_ENTRIES);
        if entries == 0 {
            return 0;
        }
        (0..n.max(0) as usize)
            .map(|i| rd::<usize>(entries, i * INV_ENTRY_SIZE))
            .filter(|&it| it != 0 && rd::<i32>(it, ITEM_TYPE) == ITEM_TYPE_BONE)
            .map(|it| rd::<i32>(it, ITEM_COUNT).max(0))
            .sum()
    }
}

/// The selected hero, if any (party index and pointer).
fn selected_hero() -> Option<(usize, usize)> {
    let (sel, heroes) = (
        SELECTED.load(Ordering::Relaxed),
        HEROES.load(Ordering::Relaxed),
    );
    if sel == 0 || heroes == 0 {
        return None;
    }
    // SAFETY: statics of the exe.
    let i: i32 = unsafe { rd(sel, 0) };
    let i = usize::try_from(i).ok().filter(|&i| i < PARTY_SIZE)?;
    let h = heroes + i * HERO_SIZE;
    // SAFETY: inside the party's hero array.
    (unsafe { rd::<u8>(h, HERO_VALID) } != 0).then_some((i, h))
}

fn party_has_bloodmage() -> bool {
    let heroes = HEROES.load(Ordering::Relaxed);
    heroes != 0
        && (0..PARTY_SIZE).any(|i| {
            let h = heroes + i * HERO_SIZE;
            // SAFETY: inside the party's hero array.
            unsafe { rd::<u8>(h, HERO_VALID) != 0 && is_bloodmage(h) }
        })
}

// ---------------------------------------------------------------------------
// Hiding the bones for one call

/// Whether the inventory count is zeroed right now, and its real value.
static HIDDEN: AtomicBool = AtomicBool::new(false);
static SAVED_COUNT: AtomicI32 = AtomicI32::new(0);

/// Sets the party inventory count to 0, remembering it. Returns false (and
/// changes nothing) if it is already hidden or not resolved.
fn hide_bones() -> bool {
    let at = INV_COUNT.load(Ordering::Relaxed);
    if at == 0 || HIDDEN.load(Ordering::Relaxed) {
        return false;
    }
    let p = at as *mut i32;
    // SAFETY: the count is a dword static of the exe, written on the game
    // thread (the only one running game logic).
    unsafe {
        SAVED_COUNT.store(p.read_volatile(), Ordering::Relaxed);
        p.write_volatile(0);
    }
    HIDDEN.store(true, Ordering::Relaxed);
    true
}

/// Puts the count back if [`hide_bones`] hid it.
fn restore_bones() {
    if !HIDDEN.swap(false, Ordering::Relaxed) {
        return;
    }
    let p = INV_COUNT.load(Ordering::Relaxed) as *mut i32;
    let saved = SAVED_COUNT.load(Ordering::Relaxed);
    // SAFETY: as in `hide_bones`.
    unsafe {
        let now = p.read_volatile();
        if now != 0 {
            uk_qol_runtime::log(&format!(
                "{ID}: party inventory count changed while hidden ({now}); restoring {saved}"
            ));
        }
        p.write_volatile(saved);
    }
}

type PayFn = unsafe extern "system" fn(usize, usize, i32);
type CanFn = unsafe extern "system" fn(usize, u64) -> i32;
type LoseFn = unsafe extern "system" fn(usize, i32) -> usize;

/// `Hero::PaySkillCost(Hero*, SkillInstance*, int cost)`.
unsafe extern "system" fn pay_skill_cost(h: usize, skill: usize, cost: i32) {
    // SAFETY: set before the hook was enabled.
    let orig: PayFn = unsafe { std::mem::transmute(ORIG_PAY.load(Ordering::Relaxed)) };
    // Only the decision runs in the guard: if it panics, the game pays as
    // usual. The call itself is outside, so it never runs twice.
    // SAFETY: the game passes a live Hero.
    let health = guard::call(ID, || unsafe { pays_with_health(h) }).unwrap_or(false);
    if !health {
        // SAFETY: the original with the game's arguments.
        return unsafe { orig(h, skill, cost) };
    }
    // SAFETY: as above.
    let hp_before: i32 = unsafe { rd(h, HERO_HP) };
    let bones_before = bones();
    let hidden = hide_bones();
    // SAFETY: the original with the game's arguments.
    unsafe { orig(h, skill, cost) };
    if hidden {
        restore_bones();
    }
    // SAFETY: as above.
    let hp_after: i32 = unsafe { rd(h, HERO_HP) };
    let bones_after = bones();
    let _ = guard::call(ID, || {
        // SAFETY: as above.
        let key = unsafe { hero_key(h) };
        uk_qol_runtime::log(&format!(
            "{ID}: {key} paid with health: hp {hp_before} -> {hp_after}, \
             bones {bones_before} -> {bones_after} (cost {cost})"
        ));
    });
}

/// `Hero::CanCastSkill(Hero*, u64 skill ref) -> int` (0 = can cast).
unsafe extern "system" fn can_cast_skill(h: usize, skill: u64) -> i32 {
    // SAFETY: set before the hook was enabled.
    let orig: CanFn = unsafe { std::mem::transmute(ORIG_CAN.load(Ordering::Relaxed)) };
    // SAFETY: the game passes a live Hero.
    let health = guard::call(ID, || unsafe { pays_with_health(h) }).unwrap_or(false);
    if !health {
        // SAFETY: the original with the game's arguments.
        return unsafe { orig(h, skill) };
    }
    let hidden = hide_bones();
    // SAFETY: the original with the game's arguments.
    let r = unsafe { orig(h, skill) };
    if hidden {
        restore_bones();
    }
    r
}

/// `Hero::LoseHP(Hero*, int)`: puts the bones back before the damage, so
/// death handling and whatever it triggers see the real inventory.
unsafe extern "system" fn lose_hp(h: usize, amount: i32) -> usize {
    restore_bones();
    // SAFETY: set before the hook was enabled; the game's arguments.
    unsafe {
        let orig: LoseFn = std::mem::transmute(ORIG_LOSE.load(Ordering::Relaxed));
        orig(h, amount)
    }
}

// ---------------------------------------------------------------------------
// The mod

pub struct BloodmageCost;

impl Mod for BloodmageCost {
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
                .map(|&rva| cx.base + rva as usize)
                .ok_or_else(|| format!("pattern {name} not resolved"))
        };
        let (pay, can, lose) = (at("pay_skill_cost")?, at("can_cast_skill")?, at("lose_hp")?);
        INV_COUNT.store(at("party_inventory_count")?, Ordering::Relaxed);
        SELECTED.store(at("selected_hero")?, Ordering::Relaxed);
        HEROES.store(at("heroes")?, Ordering::Relaxed);
        modes().default = Some(default_mode(cx.settings));
        let e = |e: uk_qol_runtime::hook::HookError| e.to_string();
        // SAFETY: each target is the start of the named function (verified
        // patterns); each detour has the same signature and ABI. The
        // trampolines are stored before the host enables the hooks.
        unsafe {
            ORIG_PAY.store(
                cx.hooks
                    .detour(pay, pay_skill_cost as PayFn as usize)
                    .map_err(e)?,
                Ordering::Relaxed,
            );
            ORIG_CAN.store(
                cx.hooks
                    .detour(can, can_cast_skill as CanFn as usize)
                    .map_err(e)?,
                Ordering::Relaxed,
            );
            ORIG_LOSE.store(
                cx.hooks
                    .detour(lose, lose_hp as LoseFn as usize)
                    .map_err(e)?,
                Ordering::Relaxed,
            );
        }
        Ok(())
    }

    fn on_settings(&mut self, settings: &toml::Table) {
        modes().default = Some(default_mode(settings));
    }

    fn on_input(&mut self, ev: &InputEvent) -> bool {
        if !hotkey::pressed(ID, TOGGLE_KEY, ev) {
            return false;
        }
        let Some((i, h)) = selected_hero() else {
            return false;
        };
        // SAFETY: a valid hero of the party.
        if !unsafe { is_bloodmage(h) } {
            if party_has_bloodmage() {
                toast::show("Select a Blood Mage to switch how they pay");
            }
            return false;
        }
        // SAFETY: as above.
        let key = unsafe { hero_key(h) };
        let m = modes().toggle(&key);
        let name = key.split_once(':').map_or(key.as_str(), |(_, n)| n);
        toast::show(match m {
            PayMode::Bones => format!("{name} pays with bones (health when out of bones)"),
            PayMode::Health => format!("{name} pays with health, keeping the bones"),
        });
        uk_qol_runtime::log(&format!(
            "{ID}: hero {i} ({key}) now pays with {}; bones {}",
            m.name(),
            bones()
        ));
        true
    }

    fn on_save(&mut self, state: &mut ModSaveState) {
        modes().save(state);
    }

    fn on_load_save(&mut self, state: &ModSaveState) {
        modes().load(state);
    }
}

/// Constructor passed to `uk_qol_runtime::start`.
pub fn new() -> Box<dyn Mod> {
    Box::new(BloodmageCost)
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
        assert_eq!(manifest.settings.len(), 2);
        assert_eq!(default_mode(&manifest.defaults()), PayMode::Bones);
        let pats = PatternFile::parse(m.patterns()).unwrap();
        assert_eq!(pats.patterns.len(), 6);
    }

    #[test]
    fn modes_default_toggle_and_sidecar() {
        let mut m = Modes {
            default: Some(PayMode::Health),
            ..Modes::default()
        };
        assert_eq!(m.get("2:Brazkin"), PayMode::Health);
        assert_eq!(m.toggle("2:Brazkin"), PayMode::Bones);
        assert_eq!(m.toggle("3:Other"), PayMode::Bones);
        assert_eq!(m.toggle("3:Other"), PayMode::Health);
        let mut st = ModSaveState::default();
        m.save(&mut st);
        assert_eq!(st.heroes.len(), 2);
        assert_eq!(st.heroes["2:Brazkin"]["mode"].as_str(), Some("bones"));

        // Loading replaces every choice; unknown values are dropped.
        let mut other = Modes::default();
        other.by_hero.insert("1:Gone".into(), PayMode::Health);
        let mut bad = toml::Table::new();
        bad.insert("mode".into(), toml::Value::String("blood".into()));
        st.heroes.insert("0:Bad".into(), bad);
        other.load(&st);
        assert_eq!(other.by_hero.len(), 2);
        assert_eq!(other.get("2:Brazkin"), PayMode::Bones);
        assert_eq!(other.get("3:Other"), PayMode::Health);
        assert_eq!(other.get("1:Gone"), PayMode::Bones);
    }

    #[test]
    fn hide_and_restore_are_exact() {
        let mut count: i32 = 7;
        INV_COUNT.store(&raw mut count as usize, Ordering::Relaxed);
        assert!(hide_bones());
        // SAFETY: a local.
        assert_eq!(unsafe { (&raw const count).read_volatile() }, 0);
        assert!(!hide_bones(), "no nested hiding");
        restore_bones();
        assert_eq!(unsafe { (&raw const count).read_volatile() }, 7);
        restore_bones(); // no-op
        assert_eq!(unsafe { (&raw const count).read_volatile() }, 7);
        INV_COUNT.store(0, Ordering::Relaxed);
    }
}
