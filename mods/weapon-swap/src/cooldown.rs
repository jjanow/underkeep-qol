//! Item cooldowns of the weapons in slot II.
//!
//! A weapon in slot II is kept by the mod, not the game, so the game's own
//! count-downs never see it: its ability stayed on cooldown however long it
//! sat there. The game counts item cooldowns (Item+0xe8) down by one in two
//! places, and in both it counts down every party-inventory item, so a
//! weapon in slot II now follows the inventory's rules:
//!
//! - `party_round_tick`, once per game turn (equipped items too);
//! - the rest update, once per rest step, just before it calls
//!   `hero_rest_step` for the four heroes in order. That update runs every
//!   frame while resting and steps only now and then, so the mod counts the
//!   calls of `hero_rest_step` for the first hero instead.
//!
//! The detours only count; the mod's frame (and save) applies the count to
//! the stored weapons with [`take`].

use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use crate::log;

static ORIG_ROUND: AtomicUsize = AtomicUsize::new(0);
static ORIG_REST: AtomicUsize = AtomicUsize::new(0);
/// The first hero's struct (the rest step for it is the one counted).
static HERO0: AtomicUsize = AtomicUsize::new(0);
/// Count-downs since the last [`take`].
static TICKS: AtomicU32 = AtomicU32::new(0);

/// Both take the party or a hero in RCX and nothing else; passing the four
/// argument registers through keeps them as they were.
type TickFn = extern "win64" fn(usize, usize, usize, usize) -> u64;

extern "win64" fn on_round(party: usize, b: usize, c: usize, d: usize) -> u64 {
    // SAFETY: set to the trampoline before the hook is enabled.
    let orig: TickFn = unsafe { std::mem::transmute(ORIG_ROUND.load(Ordering::Relaxed)) };
    let r = orig(party, b, c, d);
    TICKS.fetch_add(1, Ordering::Relaxed);
    r
}

extern "win64" fn on_rest(hero: usize, b: usize, c: usize, d: usize) -> u64 {
    if hero != 0 && hero == HERO0.load(Ordering::Relaxed) {
        TICKS.fetch_add(1, Ordering::Relaxed);
    }
    // SAFETY: set to the trampoline before the hook is enabled.
    let orig: TickFn = unsafe { std::mem::transmute(ORIG_REST.load(Ordering::Relaxed)) };
    orig(hero, b, c, d)
}

/// Installs the detours that are found (from `on_load`); without them a
/// weapon in slot II keeps its cooldown, as before.
///
/// # Safety
/// `round` and `rest` must be the resolved starts of `party_round_tick` and
/// `hero_rest_step`; `hero0` the first hero's struct.
pub unsafe fn install(
    hooks: &mut uk_qol_runtime::hook::HookSet,
    round: Option<usize>,
    rest: Option<usize>,
    hero0: usize,
) {
    HERO0.store(hero0, Ordering::Relaxed);
    TICKS.store(0, Ordering::Relaxed);
    let hooks_to = [
        (
            "party_round_tick",
            round,
            on_round as TickFn as usize,
            &ORIG_ROUND,
        ),
        (
            "hero_rest_step",
            rest,
            on_rest as TickFn as usize,
            &ORIG_REST,
        ),
    ];
    for (name, at, detour, orig) in hooks_to {
        let Some(at) = at else {
            log(&format!(
                "pattern {name} not found; slot II weapons won't count their cooldown down there"
            ));
            continue;
        };
        // SAFETY: forwarded; the detours keep the argument registers and the
        // return register as they are.
        match unsafe { hooks.detour(at, detour) } {
            Ok(t) => orig.store(t, Ordering::Relaxed),
            Err(e) => log(&format!("can't hook {name}: {e}")),
        }
    }
}

/// Count-downs since the last call (also used to drop them on a load).
pub fn take() -> u32 {
    TICKS.swap(0, Ordering::Relaxed)
}
