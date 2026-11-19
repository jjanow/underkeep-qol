//! `melee-accuracy`: melee hit chance grows with the weapon's stat (README,
//! "Mod designs").
//!
//! The game's rules: a ranged hit chance is
//! `DEX x 9 + ranged hit bonuses - evasion - ranged evasion`, but a melee
//! one is a flat `100 + melee hit bonuses - evasion - melee evasion`, with
//! no stat in it. The melee bonus
//! is a field of the hero (`Hero+0x160`, rebuilt by `Hero::RecalcStats`
//! from gear and mutations); the hero's melee attack function reads it once,
//! when it rolls the hit.
//!
//! The mod detours that function and, for the length of the call, adds
//! `base - 100` to the attacker's melee bonus, where `base` is the chosen
//! formula's hit chance (see [`formula`]):
//! - `stat_bonus` (default): `100 + max(0, WSTAT x per_point)`, never below
//!   the game's chance. WSTAT is the stat the game scales the attack's
//!   damage on: effective Dexterity if the attacking item has
//!   `uses_dexterity`, else effective Strength (also when unarmed).
//! - `ranged_mirror`: `(STR + DEX + 5) / 2 x 9`, the ranged rule's
//!   `DEX x 9` (game 1.0.6; `x 10` before) with the average of the two
//!   stats; 15 points give 90, as 10 DEX does at range. Plus a flat bonus while Righteous
//!   Strike is on, so Paladins don't need all four stats.
//! - `custom`: a formula the player types in.
//!
//! The adjustment is put back when the function returns, unless the
//! game rebuilt the field in the meantime (a kill's level-up or a status
//! runs `RecalcStats`), which leaves the game's fresh value in place. Only
//! the party's heroes are touched, and a nested call adds nothing more.
//!
//! Nothing is kept anywhere: turned off, the next attack is the game's own.

mod formula;

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use formula::{Formula, Vars};

use uk_qol_runtime::guard;
use uk_qol_runtime::host::{LoadCtx, Mod};
use uk_qol_runtime::savehook;
use uk_qol_runtime::toast;

const ID: &str = "melee-accuracy";
const FORMULA: &str = "formula";
const PER_POINT: &str = "per_point";
const RIGHTEOUS: &str = "righteous_strike";
const CUSTOM: &str = "custom_formula";
const DEFAULT_PER_POINT: i64 = 2;
const DEFAULT_RIGHTEOUS: i64 = 10;

// Hero fields (1.0.5).
const HERO_STR: usize = 0x10c; // i32, effective (gear and mutations in)
const HERO_DEX: usize = 0x110; // i32, effective
const HERO_END: usize = 0x114; // i32, effective
const HERO_WIS: usize = 0x118; // i32, effective
const HERO_MELEE_HIT: usize = 0x160; // i32, melee hit bonus
/// Set by casting Righteous Strike (Paladin), cleared by the hero attack
/// routine after the attack; the melee attack applies it to plain attacks
/// only (no weapon skill, its 6th argument null).
const HERO_RIGHTEOUS: usize = 0x20a; // u8
// Item: ItemParams at +0x120, uses_dexterity its byte at +4.
const ITEM_USES_DEX: usize = 0x124;

/// Where `melee_hit_chance` sits in `melee_attack` (1.0.5).
const HIT_CHECK_OFFSET: usize = 0x134;

/// The formula in use, with a short name for the log; `None` leaves every
/// attack to the game (a custom formula that doesn't parse).
static RULES: Mutex<Option<(String, Formula)>> = Mutex::new(None);
static ORIG: AtomicUsize = AtomicUsize::new(0);
/// Set while a call is adjusting a hero's bonus, so a nested call doesn't
/// add a second one.
static BUSY: AtomicBool = AtomicBool::new(false);

fn int(settings: &toml::Table, key: &str, default: i64) -> i64 {
    settings
        .get(key)
        .and_then(toml::Value::as_integer)
        .unwrap_or(default)
}

/// The formula the settings ask for, as (mode name, text).
fn formula_text(settings: &toml::Table) -> (String, String) {
    let mode = settings
        .get(FORMULA)
        .and_then(toml::Value::as_str)
        .unwrap_or("stat_bonus");
    match mode {
        "ranged_mirror" => (
            mode.into(),
            format!(
                "(STR + DEX + 5) / 2 * 9 + RS * {}",
                int(settings, RIGHTEOUS, DEFAULT_RIGHTEOUS)
            ),
        ),
        "custom" => (
            mode.into(),
            settings
                .get(CUSTOM)
                .and_then(toml::Value::as_str)
                .unwrap_or("")
                .to_owned(),
        ),
        _ => (
            "stat_bonus".into(),
            format!(
                "100 + max(0, WSTAT * {})",
                int(settings, PER_POINT, DEFAULT_PER_POINT)
            ),
        ),
    }
}

/// Parses the settings' formula into [`RULES`]. A custom formula that
/// doesn't parse turns the mod's effect off until it is fixed, with a log
/// line and a toast saying why.
fn set_rules(settings: &toml::Table) {
    let (mode, text) = formula_text(settings);
    let rules = match Formula::parse(&text) {
        Ok(f) => {
            uk_qol_runtime::log(&format!("{ID}: {mode}: base hit chance = {text}"));
            Some((mode, f))
        }
        Err(e) => {
            uk_qol_runtime::log(&format!(
                "{ID}: custom formula {text:?}: {e}; melee attacks use the game's chance"
            ));
            toast::show(format!(
                "Melee accuracy: custom formula error ({e}); using the game's hit chance"
            ));
            None
        }
    };
    *RULES.lock().unwrap_or_else(|p| p.into_inner()) = rules;
}

/// # Safety
/// `p + off` must be readable.
unsafe fn rd<T: Copy>(p: usize, off: usize) -> T {
    // SAFETY: forwarded.
    unsafe { std::ptr::read_unaligned((p + off) as *const T) }
}

/// # Safety
/// `p + off` must be writable.
unsafe fn wr(p: usize, off: usize, v: i32) {
    // SAFETY: forwarded.
    unsafe { std::ptr::write_unaligned((p + off) as *mut i32, v) }
}

/// Which stat an attack with `weapon` uses: the game's damage rule
/// (0x140185680). Returns (name, value).
///
/// # Safety
/// `hero` must be a live Hero, `weapon` 0 or a live Item.
unsafe fn attack_stat(hero: usize, weapon: usize) -> (&'static str, i32) {
    // SAFETY: forwarded.
    unsafe {
        if weapon != 0 && rd::<u8>(weapon, ITEM_USES_DEX) != 0 {
            ("DEX", rd(hero, HERO_DEX))
        } else {
            ("STR", rd(hero, HERO_STR))
        }
    }
}

/// What `formula` can read for `hero` attacking with `weapon`; `skill` is
/// the melee attack's 6th argument (the weapon skill, null for a plain
/// attack).
///
/// # Safety
/// As for [`attack_stat`].
unsafe fn vars(hero: usize, weapon: usize, skill: usize) -> Vars {
    // SAFETY: forwarded.
    unsafe {
        let f = |off| f64::from(rd::<i32>(hero, off));
        Vars {
            str_: f(HERO_STR),
            dex: f(HERO_DEX),
            end: f(HERO_END),
            wis: f(HERO_WIS),
            wstat: f64::from(attack_stat(hero, weapon).1),
            rs: if skill == 0 && rd::<u8>(hero, HERO_RIGHTEOUS) != 0 {
                1.0
            } else {
                0.0
            },
        }
    }
}

/// Adds `add` to `hero`'s melee hit field. Returns (saved, written) for
/// [`restore`], or `None` when there's nothing to add.
///
/// # Safety
/// `hero` must be a live Hero.
unsafe fn apply(hero: usize, add: i32) -> Option<(i32, i32)> {
    if add == 0 {
        return None;
    }
    // SAFETY: forwarded.
    unsafe {
        let saved: i32 = rd(hero, HERO_MELEE_HIT);
        let ours = saved.saturating_add(add);
        wr(hero, HERO_MELEE_HIT, ours);
        Some((saved, ours))
    }
}

/// Puts the saved value back if the field still holds ours; if the game
/// rebuilt it during the attack, its new value stays.
///
/// # Safety
/// `hero` must be a live Hero.
unsafe fn restore(hero: usize, saved: i32, ours: i32) {
    // SAFETY: forwarded.
    unsafe {
        if rd::<i32>(hero, HERO_MELEE_HIT) == ours {
            wr(hero, HERO_MELEE_HIT, saved);
        }
    }
}

type AttackFn = unsafe extern "system" fn(usize, usize, usize, usize, usize, usize, usize) -> usize;

/// The hero's melee attack (see `patterns.toml`): seven arguments, the
/// first the result struct it fills and returns.
unsafe extern "system" fn melee_attack(
    out: usize,
    hero: usize,
    target: usize,
    weapon: usize,
    a5: usize,
    a6: usize,
    a7: usize,
) -> usize {
    // SAFETY: set before the hook was enabled.
    let orig: AttackFn = unsafe { std::mem::transmute(ORIG.load(Ordering::Relaxed)) };
    // SAFETY (each call below): the original with the game's arguments.
    if target == 0 || BUSY.swap(true, Ordering::Relaxed) {
        return unsafe { orig(out, hero, target, weapon, a5, a6, a7) };
    }
    // Only the decision runs in the guard: if it panics, the attack is the
    // game's own. The call itself is outside, so it never runs twice.
    let plan = guard::call(ID, || {
        let h = savehook::party_heroes()
            .into_iter()
            .find(|h| h.ptr == hero)?;
        // SAFETY: one of the party's heroes; the game passes the weapon.
        let v = unsafe { vars(hero, weapon, a6) };
        let (mode, base) = {
            let rules = RULES.lock().unwrap_or_else(|p| p.into_inner());
            let (mode, f) = rules.as_ref()?;
            (mode.clone(), f.eval(&v))
        };
        let Some(base) = base else {
            uk_qol_runtime::log(&format!(
                "{ID}: {} melee attack: {mode} formula gave no number (division by zero?); game's chance",
                h.key
            ));
            return None;
        };
        // SAFETY: as above.
        let plan = unsafe { apply(hero, base - 100) };
        if let Some((saved, ours)) = plan {
            uk_qol_runtime::log(&format!(
                "{ID}: {} melee attack ({mode}): STR {} DEX {} END {} WIS {} WSTAT {} RS {} -> base {base} instead of 100 (melee bonus {saved} -> {ours})",
                h.key, v.str_, v.dex, v.end, v.wis, v.wstat, v.rs
            ));
        }
        plan
    })
    .flatten();
    let r = unsafe { orig(out, hero, target, weapon, a5, a6, a7) };
    if let Some((saved, ours)) = plan {
        // SAFETY: the same hero, still live (party heroes are static).
        unsafe { restore(hero, saved, ours) };
    }
    BUSY.store(false, Ordering::Relaxed);
    r
}

pub struct MeleeAccuracy;

impl Mod for MeleeAccuracy {
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
        let (attack, check) = (at("melee_attack")?, at("melee_hit_chance")?);
        if check.wrapping_sub(attack) != HIT_CHECK_OFFSET {
            return Err(format!(
                "melee_hit_chance is at melee_attack+{:#x}, expected +{HIT_CHECK_OFFSET:#x}",
                check.wrapping_sub(attack)
            ));
        }
        set_rules(cx.settings);
        BUSY.store(false, Ordering::Relaxed);
        // SAFETY: the target is the start of the function (verified
        // pattern); the detour has its signature and ABI. The trampoline is
        // stored before the host enables the hook.
        unsafe {
            ORIG.store(
                cx.hooks
                    .detour(attack, melee_attack as AttackFn as usize)
                    .map_err(|e| e.to_string())?,
                Ordering::Relaxed,
            );
        }
        Ok(())
    }

    fn on_settings(&mut self, settings: &toml::Table) {
        set_rules(settings);
    }
}

/// Constructor passed to `uk_qol_runtime::start`.
pub fn new() -> Box<dyn Mod> {
    Box::new(MeleeAccuracy)
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
        assert!(!manifest.enabled, "a balance change: off by default");
        let d = manifest.defaults();
        assert_eq!(
            formula_text(&d),
            ("stat_bonus".into(), "100 + max(0, WSTAT * 2)".into())
        );
        // The shipped custom example parses.
        let (_, text) = formula_text(&with(&d, FORMULA, "custom"));
        Formula::parse(&text).unwrap();
        let pats = PatternFile::parse(m.patterns()).unwrap();
        assert_eq!(pats.patterns.len(), 2);
    }

    fn with(t: &toml::Table, k: &str, v: impl Into<toml::Value>) -> toml::Table {
        let mut t = t.clone();
        t.insert(k.into(), v.into());
        t
    }

    fn base(settings: &toml::Table, v: &Vars) -> Option<i32> {
        Formula::parse(&formula_text(settings).1).unwrap().eval(v)
    }

    #[test]
    fn presets() {
        let d = new_defaults();
        let v = |str_: f64, dex: f64, wstat: f64, rs: f64| Vars {
            str_,
            dex,
            wstat,
            rs,
            ..Vars::default()
        };
        // stat_bonus: never below the game's 100.
        assert_eq!(base(&d, &v(6.0, 3.0, 6.0, 0.0)), Some(112));
        assert_eq!(base(&d, &v(-3.0, 3.0, -3.0, 1.0)), Some(100));
        assert_eq!(
            base(&with(&d, PER_POINT, 0), &v(8.0, 3.0, 8.0, 0.0)),
            Some(100)
        );
        // ranged_mirror: x 9 like ranged; 15 points = 90; +10 with RS.
        let m = with(&d, FORMULA, "ranged_mirror");
        assert_eq!(base(&m, &v(8.0, 7.0, 8.0, 0.0)), Some(90));
        assert_eq!(base(&m, &v(8.0, 6.0, 8.0, 0.0)), Some(86));
        assert_eq!(base(&m, &v(8.0, 6.0, 8.0, 1.0)), Some(96));
        assert_eq!(
            base(&with(&m, RIGHTEOUS, 0), &v(8.0, 6.0, 8.0, 1.0)),
            Some(86)
        );
        // Unknown mode: the default.
        assert_eq!(formula_text(&with(&d, FORMULA, "nope")).0, "stat_bonus");
        // custom
        let c = with(&with(&d, FORMULA, "custom"), CUSTOM, "STR * 10 + 20");
        assert_eq!(base(&c, &v(7.0, 0.0, 7.0, 0.0)), Some(90));
    }

    fn new_defaults() -> toml::Table {
        Manifest::parse(new().manifest()).unwrap().defaults()
    }

    /// A zeroed hero and item big enough for the fields used.
    fn hero(str_: i32, dex: i32, melee: i32) -> Vec<u8> {
        let mut h = vec![0u8; 0x2b8];
        h[HERO_STR..HERO_STR + 4].copy_from_slice(&str_.to_le_bytes());
        h[HERO_DEX..HERO_DEX + 4].copy_from_slice(&dex.to_le_bytes());
        h[HERO_MELEE_HIT..HERO_MELEE_HIT + 4].copy_from_slice(&melee.to_le_bytes());
        h
    }

    fn melee(h: &[u8]) -> i32 {
        i32::from_le_bytes(h[HERO_MELEE_HIT..HERO_MELEE_HIT + 4].try_into().unwrap())
    }

    #[test]
    fn stat_follows_the_weapon() {
        let h = hero(7, 4, 0);
        let mut item = vec![0u8; 0x600];
        // SAFETY: local buffers of the right size.
        unsafe {
            assert_eq!(attack_stat(h.as_ptr() as usize, 0), ("STR", 7));
            assert_eq!(
                attack_stat(h.as_ptr() as usize, item.as_ptr() as usize),
                ("STR", 7)
            );
            item[ITEM_USES_DEX] = 1;
            assert_eq!(
                attack_stat(h.as_ptr() as usize, item.as_ptr() as usize),
                ("DEX", 4)
            );
        }
    }

    #[test]
    fn vars_read_the_hero() {
        let mut h = hero(7, 4, 0);
        h[HERO_END..HERO_END + 4].copy_from_slice(&5i32.to_le_bytes());
        h[HERO_WIS..HERO_WIS + 4].copy_from_slice(&2i32.to_le_bytes());
        let p = h.as_mut_ptr() as usize;
        // SAFETY: local buffer.
        unsafe {
            let v = vars(p, 0, 0);
            assert_eq!(
                (v.str_, v.dex, v.end, v.wis, v.wstat, v.rs),
                (7.0, 4.0, 5.0, 2.0, 7.0, 0.0)
            );
            *(p as *mut u8).add(HERO_RIGHTEOUS) = 1;
            assert_eq!(vars(p, 0, 0).rs, 1.0);
            // A weapon skill: the game doesn't apply Righteous Strike.
            assert_eq!(vars(p, 0, 0x1234).rs, 0.0);
        }
    }

    #[test]
    fn apply_and_restore() {
        let mut h = hero(6, 3, -5);
        let p = h.as_mut_ptr() as usize;
        // SAFETY: local buffer.
        unsafe {
            let (saved, ours) = apply(p, 12).unwrap();
            assert_eq!((saved, ours), (-5, 7));
            assert_eq!(melee(&h), 7);
            restore(p, saved, ours);
            assert_eq!(melee(&h), -5);

            // Rebuilt by the game during the attack: its value stays.
            let (saved, ours) = apply(p, 12).unwrap();
            h[HERO_MELEE_HIT..HERO_MELEE_HIT + 4].copy_from_slice(&(-3i32).to_le_bytes());
            restore(p, saved, ours);
            assert_eq!(melee(&h), -3);

            // Nothing to add.
            assert_eq!(apply(p, 0), None);
            assert_eq!(melee(&h), -3);

            // A formula below 100 lowers it, and it comes back.
            let (saved, ours) = apply(p, -5).unwrap();
            assert_eq!((saved, ours), (-3, -8));
            restore(p, saved, ours);
            assert_eq!(melee(&h), -3);
        }
    }
}
