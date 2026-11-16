//! `melee-accuracy`: melee hit chance grows with the weapon's stat (README,
//! "Mod designs").
//!
//! The game's rules: a ranged hit chance is
//! `DEX x 10 + ranged hit bonuses - evasion - ranged evasion`, but a melee
//! one is a flat `100 + melee hit bonuses - evasion - melee evasion`, with
//! no stat in it. The melee bonus
//! is a field of the hero (`Hero+0x160`, rebuilt by `Hero::RecalcStats`
//! from gear and mutations); the hero's melee attack function reads it once,
//! when it rolls the hit.
//!
//! The mod detours that function and, for the length of the call, adds
//! `max(0, stat x per_point)` to the attacker's melee bonus. The stat is the
//! one the game scales the attack's damage on: effective Dexterity if the
//! attacking item has `uses_dexterity`, else effective Strength (also when
//! unarmed). The bonus is put back when the function returns, unless the
//! game rebuilt the field in the meantime (a kill's level-up or a status
//! runs `RecalcStats`), which leaves the game's fresh value in place. Only
//! the party's heroes are touched, and a nested call adds nothing more.
//!
//! Nothing is kept anywhere: turned off, the next attack is the game's own.

use std::sync::atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering};

use uk_qol_runtime::guard;
use uk_qol_runtime::host::{LoadCtx, Mod};
use uk_qol_runtime::savehook;

const ID: &str = "melee-accuracy";
const PER_POINT: &str = "per_point";
const DEFAULT_PER_POINT: i32 = 2;

// Hero fields (1.0.5).
const HERO_STR: usize = 0x10c; // i32, effective (gear and mutations in)
const HERO_DEX: usize = 0x110; // i32, effective
const HERO_MELEE_HIT: usize = 0x160; // i32, melee hit bonus
// Item: ItemParams at +0x120, uses_dexterity its byte at +4.
const ITEM_USES_DEX: usize = 0x124;

/// Where `melee_hit_chance` sits in `melee_attack` (1.0.5).
const HIT_CHECK_OFFSET: usize = 0x134;

static PER: AtomicI32 = AtomicI32::new(DEFAULT_PER_POINT);
static ORIG: AtomicUsize = AtomicUsize::new(0);
/// Set while a call is adjusting a hero's bonus, so a nested call doesn't
/// add a second one.
static BUSY: AtomicBool = AtomicBool::new(false);

/// The bonus for a stat: never negative, so the game's chance is a floor.
fn bonus(stat: i32, per_point: i32) -> i32 {
    stat.saturating_mul(per_point).max(0)
}

fn per_point(settings: &toml::Table) -> i32 {
    settings
        .get(PER_POINT)
        .and_then(toml::Value::as_integer)
        .and_then(|v| i32::try_from(v).ok())
        .unwrap_or(DEFAULT_PER_POINT)
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

/// Adds the bonus to `hero`'s melee hit field. Returns (saved, written) for
/// [`restore`], or `None` when there's nothing to add.
///
/// # Safety
/// As for [`attack_stat`].
unsafe fn apply(hero: usize, weapon: usize, per_point: i32) -> Option<(i32, i32)> {
    // SAFETY: forwarded.
    unsafe {
        let (_, stat) = attack_stat(hero, weapon);
        let add = bonus(stat, per_point);
        if add == 0 {
            return None;
        }
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
        let (name, stat) = unsafe { attack_stat(hero, weapon) };
        let per = PER.load(Ordering::Relaxed);
        // SAFETY: as above.
        let plan = unsafe { apply(hero, weapon, per) };
        if let Some((saved, ours)) = plan {
            uk_qol_runtime::log(&format!(
                "{ID}: {} melee attack: {name} {stat} -> hit +{} (melee bonus {saved} -> {ours})",
                h.key,
                ours - saved
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
        PER.store(per_point(cx.settings), Ordering::Relaxed);
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
        PER.store(per_point(settings), Ordering::Relaxed);
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
        assert_eq!(per_point(&manifest.defaults()), 2);
        let pats = PatternFile::parse(m.patterns()).unwrap();
        assert_eq!(pats.patterns.len(), 2);
    }

    #[test]
    fn bonus_never_lowers_the_chance() {
        assert_eq!(bonus(6, 2), 12);
        assert_eq!(bonus(11, 2), 22);
        assert_eq!(bonus(0, 2), 0);
        assert_eq!(bonus(-3, 2), 0);
        assert_eq!(bonus(8, 0), 0);
        assert_eq!(bonus(i32::MAX, 10), i32::MAX);
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
    fn apply_and_restore() {
        let mut h = hero(6, 3, -5);
        let p = h.as_mut_ptr() as usize;
        // SAFETY: local buffer.
        unsafe {
            let (saved, ours) = apply(p, 0, 2).unwrap();
            assert_eq!((saved, ours), (-5, 7));
            assert_eq!(melee(&h), 7);
            restore(p, saved, ours);
            assert_eq!(melee(&h), -5);

            // Rebuilt by the game during the attack: its value stays.
            let (saved, ours) = apply(p, 0, 2).unwrap();
            h[HERO_MELEE_HIT..HERO_MELEE_HIT + 4].copy_from_slice(&(-3i32).to_le_bytes());
            restore(p, saved, ours);
            assert_eq!(melee(&h), -3);

            // Nothing to add.
            assert_eq!(apply(p, 0, 0), None);
            assert_eq!(melee(&h), -3);
        }
    }
}
