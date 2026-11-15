//! Quest scripts and the second weapon slot.
//!
//! A weapon in slot II is kept by the mod, not the game, so the game's
//! script checks don't see it. Two quest weapons are checked by scripts:
//! the Jacob encounter asks `party_has_item item_jacob_hand_axe` and then
//! `remove_party_item`s it, and the wizard's defeat asks for
//! `item_soulforge_axe` (the good ending). The game evaluates script
//! conditions in one function (`script_condition`: `party_has_item` is
//! condition 0x10, `party_has_all_items` 0x11) and runs commands in another
//! (`script_command`: `remove_party_item` is command 0x18). Both scan the
//! party inventory and the heroes' equipment for the named def.
//!
//! The mod detours both. Before one of those three opcodes runs, any hero's
//! slot II weapon that the opcode names is created in the party inventory
//! again (as the slot's "Move to inventory" does), so the game's own check
//! or removal then sees it. The slot is cleared through [`drain`] on the
//! mod's next frame or save, which tells the player.
//!
//! The detours and the mod's frame code both run on the game thread; the
//! mutex only keeps the shared copy consistent.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use uk_qol_runtime::guard;

use crate::game::Game;
use crate::state::{HEROES, StoredItem, valid_def};
use crate::{ID, log};

/// `script_condition` ids that look for a party item by def name.
const COND_PARTY_HAS_ITEM: u32 = 0x10;
const COND_PARTY_HAS_ALL_ITEMS: u32 = 0x11;
/// `script_command` id of `remove_party_item`.
const CMD_REMOVE_PARTY_ITEM: u32 = 0x18;

/// Script opcode context: +0x30 -> argument list (+0 count i32, +8 entries
/// of 0x18 bytes: engine String {refcounted buffer, data, len}).
const CTX_ARGS: usize = 0x30;
const ARGS_COUNT: usize = 0x00;
const ARGS_ENTRIES: usize = 0x08;
const ARG_SIZE: usize = 0x18;
const ARG_DATA: usize = 0x08;
const ARG_LEN: usize = 0x10;
/// More arguments than any item opcode takes: a bad read.
const MAX_ARGS: i32 = 32;

struct Shared {
    game: Option<Game>,
    /// Each hero's slot II weapon, as the mod's state had it last frame.
    slots: [Option<StoredItem>; HEROES],
    /// Weapons put back into the inventory for a script: (hero, weapon).
    returned: Vec<(usize, StoredItem)>,
}

static SHARED: Mutex<Shared> = Mutex::new(Shared {
    game: None,
    slots: [None, None, None, None],
    returned: Vec::new(),
});
static ORIG_CONDITION: AtomicUsize = AtomicUsize::new(0);
static ORIG_COMMAND: AtomicUsize = AtomicUsize::new(0);
/// Logged the first party-item check (once per session, for triage).
static SEEN: AtomicBool = AtomicBool::new(false);

fn shared() -> std::sync::MutexGuard<'static, Shared> {
    SHARED.lock().unwrap_or_else(|e| e.into_inner())
}

/// Both functions are called as (id, arg, ctx-or-arg, arg); passing all four
/// registers through and returning RAX keeps either signature intact
/// (`script_condition` returns a bool in AL, `script_command` nothing).
type ScriptFn = extern "win64" fn(u32, usize, usize, usize) -> u64;

/// Detour of `script_condition(id, ?, ctx)`.
extern "win64" fn on_condition(id: u32, a: usize, ctx: usize, d: usize) -> u64 {
    if id == COND_PARTY_HAS_ITEM || id == COND_PARTY_HAS_ALL_ITEMS {
        // SAFETY: the game's opcode context for this condition.
        guard::call(ID, || unsafe { return_named(ctx) });
    }
    // SAFETY: set to the trampoline before the hook is enabled.
    let orig: ScriptFn = unsafe { std::mem::transmute(ORIG_CONDITION.load(Ordering::Relaxed)) };
    orig(id, a, ctx, d)
}

/// Detour of `script_command(id, ctx)`.
extern "win64" fn on_command(id: u32, ctx: usize, c: usize, d: usize) -> u64 {
    if id == CMD_REMOVE_PARTY_ITEM {
        // SAFETY: the game's opcode context for this command.
        guard::call(ID, || unsafe { return_named(ctx) });
    }
    // SAFETY: set to the trampoline before the hook is enabled.
    let orig: ScriptFn = unsafe { std::mem::transmute(ORIG_COMMAND.load(Ordering::Relaxed)) };
    orig(id, ctx, c, d)
}

/// The literal def-name arguments of an opcode (variables, numbers that
/// aren't def names and unreadable entries are left out).
///
/// # Safety
/// `ctx` must be the game's opcode context.
unsafe fn script_args(ctx: usize) -> Vec<String> {
    // SAFETY: reads inside the game's script objects, null-checked.
    unsafe {
        if ctx == 0 {
            return Vec::new();
        }
        let list = crate::game::rd::<usize>(ctx + CTX_ARGS);
        if list == 0 {
            return Vec::new();
        }
        let n = crate::game::rd::<i32>(list + ARGS_COUNT);
        let entries = crate::game::rd::<usize>(list + ARGS_ENTRIES);
        if entries == 0 || !(0..=MAX_ARGS).contains(&n) {
            return Vec::new();
        }
        (0..n as usize)
            .filter_map(|k| {
                let s = entries + k * ARG_SIZE;
                let data = crate::game::rd::<usize>(s + ARG_DATA);
                let len = crate::game::rd::<i32>(s + ARG_LEN);
                if data == 0 || !(1..=128).contains(&len) {
                    return None;
                }
                let bytes = std::slice::from_raw_parts(data as *const u8, len as usize);
                std::str::from_utf8(bytes)
                    .ok()
                    .filter(|s| valid_def(s))
                    .map(str::to_owned)
            })
            .collect()
    }
}

/// Puts every slot II weapon that the opcode names back in the inventory.
///
/// # Safety
/// `ctx` must be the game's opcode context; game thread.
unsafe fn return_named(ctx: usize) {
    let mut sh = shared();
    let Some(game) = sh.game else { return };
    let first = !SEEN.swap(true, Ordering::Relaxed);
    if !first && sh.slots.iter().all(Option::is_none) {
        return;
    }
    // SAFETY: forwarded.
    let args = unsafe { script_args(ctx) };
    if first {
        log(&format!("quest item checks hooked (first one: {args:?})"));
    }
    for h in 0..HEROES {
        let Some(s) = sh.slots[h].clone() else {
            continue;
        };
        if !args.contains(&s.def) {
            continue;
        }
        match game.put_in_inventory(&s) {
            Ok(()) => {
                log(&format!(
                    "a script asks for {}: moved it from hero {}'s second weapon slot to the \
                     inventory",
                    s.def,
                    h + 1
                ));
                sh.slots[h] = None;
                sh.returned.push((h, s));
            }
            Err(e) => log(&format!(
                "a script asks for {}, but it couldn't leave the second weapon slot: {e}",
                s.def
            )),
        }
    }
}

/// Installs both detours (from `on_load`).
///
/// # Safety
/// `condition` and `command` must be the resolved starts of
/// `script_condition` and `script_command`.
pub unsafe fn install(
    hooks: &mut uk_qol_runtime::hook::HookSet,
    condition: usize,
    command: usize,
) -> Result<(), String> {
    // SAFETY: forwarded; the detours keep the four argument registers and
    // the return register as they are.
    unsafe {
        let t = hooks
            .detour(condition, on_condition as ScriptFn as usize)
            .map_err(|e| e.to_string())?;
        ORIG_CONDITION.store(t, Ordering::Relaxed);
        let t = hooks
            .detour(command, on_command as ScriptFn as usize)
            .map_err(|e| e.to_string())?;
        ORIG_COMMAND.store(t, Ordering::Relaxed);
    }
    Ok(())
}

/// The game handle for the detours (`None` stops them acting).
pub fn set_game(game: Option<Game>) {
    shared().game = game;
}

/// Copies the slots for the detours (after the mod changed them).
pub fn mirror(slots: [Option<StoredItem>; HEROES]) {
    shared().slots = slots;
}

/// Weapons the detours moved to the inventory since the last call.
pub fn drain() -> Vec<(usize, StoredItem)> {
    std::mem::take(&mut shared().returned)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An engine String as the script keeps its arguments.
    #[repr(C)]
    struct Arg {
        buf: usize,
        data: *const u8,
        len: i32,
        _pad: i32,
    }

    #[repr(C)]
    struct List {
        count: i32,
        _pad: i32,
        entries: *const Arg,
    }

    fn arg(s: &'static str) -> Arg {
        Arg {
            buf: 0,
            data: s.as_ptr(),
            len: s.len() as i32,
            _pad: 0,
        }
    }

    #[test]
    fn reads_literal_def_arguments() {
        let args = [arg("item_jacob_hand_axe"), arg("1"), arg("$var"), arg("")];
        let list = List {
            count: args.len() as i32,
            _pad: 0,
            entries: args.as_ptr(),
        };
        let mut ctx = [0u8; CTX_ARGS + 8];
        ctx[CTX_ARGS..].copy_from_slice(&(&list as *const List as usize).to_ne_bytes());
        // SAFETY: a context laid out like the game's.
        let got = unsafe { script_args(ctx.as_ptr() as usize) };
        assert_eq!(got, ["item_jacob_hand_axe", "1"]);
    }

    #[test]
    fn empty_or_bad_contexts_give_nothing() {
        // SAFETY: null context.
        assert!(unsafe { script_args(0) }.is_empty());
        let list = List {
            count: MAX_ARGS + 1,
            _pad: 0,
            entries: std::ptr::dangling(),
        };
        let mut ctx = [0u8; CTX_ARGS + 8];
        ctx[CTX_ARGS..].copy_from_slice(&(&list as *const List as usize).to_ne_bytes());
        // SAFETY: the count is rejected before any entry is read.
        assert!(unsafe { script_args(ctx.as_ptr() as usize) }.is_empty());
    }
}
