//! The game's party inventory and HUD state in memory (game 1.0.5; offsets
//! from static analysis, see `patterns.toml`).
//!
//! Everything here runs on the game's main thread (from `on_frame`, called
//! in the `SDL_GL_SwapWindow` hook), the thread that also changes the
//! inventory, so nothing changes under us while we read or write.

use std::collections::BTreeMap;

use crate::sort::{self, Entry};

/// `Inventory` fields.
const INV_NEXT_SEQ: usize = 0x00;
const INV_COUNT: usize = 0x08;
const INV_ENTRIES: usize = 0x10;
const INV_LISTENER_COUNT: usize = 0x18;
const INV_LISTENERS: usize = 0x20;
/// Entry: { Item*, int, int seq, byte newly_picked, padding }.
const ENTRY_SIZE: usize = 0x18;
const ENTRY_SEQ: usize = 0x0c;
/// Listener (an `InventoryView`): "inventory changed, rebuild the layout".
const LISTENER_DIRTY: usize = 0x100;

/// `Item` fields.
const ITEM_DEF: usize = 0x08;
const ITEM_COUNT: usize = 0xd0;
const ITEM_TYPE: usize = 0x100;
const ITEM_PRICE: usize = 0x428;
const ITEM_STACKABLE: usize = 0x432;
/// The item definition's name (an engine `String`: data pointer, length).
const DEF_NAME_DATA: usize = 0x80;
const DEF_NAME_LEN: usize = 0x88;

/// GUI context: press/drag state (0 idle, 1 pressed, 2 dragging).
const GUI_DRAG_STATE: usize = 0x58;

/// Sanity limit on the entry count (the game has no fixed capacity, but a
/// party carries tens of items, not thousands).
const MAX_ENTRIES: i32 = 4096;

type BlockedFn = unsafe extern "C" fn() -> u8;
type AddFn = unsafe extern "C" fn(inv: usize, item: usize, newly_picked: u8) -> usize;
type RemoveFn = unsafe extern "C" fn(inv: usize, item: usize) -> u8;

/// Resolved addresses (absolute).
#[derive(Debug, Clone, Copy)]
pub struct Game {
    pub inventory: usize,
    pub hud_mode: usize,
    pub gui_ctx: usize,
    pub game_state: usize,
    pub menu_state: usize,
    pub hud_modal: usize,
    pub input_blocked: usize,
    pub inventory_add: usize,
    pub inventory_remove: usize,
}

/// HUD panel modes (`hud_mode`) in which the inventory can be sorted: 1
/// plain HUD (the party inventory is always on screen during play), 3
/// gamepad inventory cursor, 6 character sheet. Not 2 (gamepad
/// pick-from-floor), nor the alchemy, map and other panels.
pub const SORT_MODES: &[i32] = &[1, 3, 6];

/// Outcome of a sort.
#[derive(Debug, PartialEq)]
pub enum Sorted {
    AlreadySorted { entries: usize },
    Sorted { entries: usize, merged: usize },
}

unsafe fn r32(a: usize) -> i32 {
    unsafe { std::ptr::read_volatile(a as *const i32) }
}
unsafe fn rptr(a: usize) -> usize {
    unsafe { std::ptr::read_volatile(a as *const usize) }
}
unsafe fn r8(a: usize) -> u8 {
    unsafe { std::ptr::read_volatile(a as *const u8) }
}

impl Game {
    /// Why sorting isn't allowed right now, or Ok. Mirrors the game's own
    /// "HUD takes input" check (the one in front of every inventory click):
    /// the game window focused and no console, not in the game-over state,
    /// no menu or dialog over the HUD, plus: a HUD mode with the party
    /// inventory, and the mouse not pressing or dragging anything.
    ///
    /// # Safety
    /// The addresses must be the resolved ones of this game build.
    pub unsafe fn can_sort(&self) -> Result<(), String> {
        unsafe {
            let blocked: BlockedFn = std::mem::transmute(self.input_blocked);
            if blocked() != 0 {
                return Err("game input is blocked (console or window not focused)".into());
            }
            let gs = r32(self.game_state);
            if gs == 3 {
                return Err(format!("game state {gs}"));
            }
            let menu = r32(self.menu_state);
            if menu != 0 {
                return Err(format!("a menu is open ({menu})"));
            }
            let modal = r32(self.hud_modal);
            if modal != 0 {
                return Err(format!("a dialog is open ({modal})"));
            }
            let mode = r32(self.hud_mode);
            if !SORT_MODES.contains(&mode) {
                return Err(format!("HUD mode {mode}"));
            }
            let gui = rptr(self.gui_ctx);
            if gui == 0 {
                return Err("no GUI context".into());
            }
            let drag = r32(gui + GUI_DRAG_STATE);
            if drag != 0 {
                return Err(format!("mouse press or drag in progress ({drag})"));
            }
            let n = r32(self.inventory + INV_COUNT);
            if !(0..=MAX_ENTRIES).contains(&n) {
                return Err(format!("unexpected inventory size {n}"));
            }
            Ok(())
        }
    }

    unsafe fn entries(&self) -> Result<(usize, usize), String> {
        unsafe {
            let n = r32(self.inventory + INV_COUNT);
            if !(0..=MAX_ENTRIES).contains(&n) {
                return Err(format!("unexpected inventory size {n}"));
            }
            let base = rptr(self.inventory + INV_ENTRIES);
            if n > 0 && base == 0 {
                return Err("inventory has entries but no array".into());
            }
            Ok((base, n as usize))
        }
    }

    unsafe fn item(&self, at: usize) -> Result<usize, String> {
        let item = unsafe { rptr(at) };
        if item == 0 {
            return Err("empty inventory entry".into());
        }
        Ok(item)
    }

    unsafe fn read(&self) -> Result<(Vec<Entry>, Vec<bool>), String> {
        unsafe {
            let (base, n) = self.entries()?;
            let mut out = Vec::with_capacity(n);
            let mut stack = Vec::with_capacity(n);
            for i in 0..n {
                let e = base + i * ENTRY_SIZE;
                let item = self.item(e)?;
                out.push(Entry {
                    item_type: r32(item + ITEM_TYPE),
                    price: r32(item + ITEM_PRICE),
                    name: def_name(item),
                    count: r32(item + ITEM_COUNT),
                    seq: r32(e + ENTRY_SEQ),
                });
                stack.push(r8(item + ITEM_STACKABLE) != 0);
            }
            Ok((out, stack))
        }
    }

    /// Item totals per definition name (to check that nothing is lost).
    fn totals(entries: &[Entry]) -> BTreeMap<String, i64> {
        let mut m = BTreeMap::new();
        for e in entries {
            *m.entry(e.name.clone()).or_insert(0) += e.count as i64;
        }
        m
    }

    /// One log line per entry (for testing).
    ///
    /// # Safety
    /// As for [`Game::can_sort`].
    pub unsafe fn dump(&self, label: &str) -> Vec<String> {
        let mut out = Vec::new();
        match unsafe { self.read() } {
            Ok((v, st)) => {
                out.push(format!("{label}: {} entries", v.len()));
                for (i, e) in v.iter().enumerate() {
                    out.push(format!(
                        "{label} [{i}] seq {} cat {} type {} price {} count {} stack {} {}",
                        e.seq,
                        sort::category(e.item_type),
                        e.item_type,
                        e.price,
                        e.count,
                        st[i] as u8,
                        e.name
                    ));
                }
            }
            Err(e) => out.push(format!("{label}: {e}")),
        }
        out
    }

    /// Merges duplicate stacks (through the game's own remove and add, which
    /// merges a stackable item into the existing stack of the same kind),
    /// then sorts. Call only after [`Game::can_sort`] said Ok.
    ///
    /// # Safety
    /// As for [`Game::can_sort`]; on the game's main thread.
    pub unsafe fn sort(&self, merge: bool) -> Result<Sorted, String> {
        unsafe {
            let (before, stackable) = self.read()?;
            let totals = Self::totals(&before);
            let mut merged = 0;
            if merge {
                let dups = sort::duplicate_stacks(&before, &stackable);
                if !dups.is_empty() {
                    let (base, _) = self.entries()?;
                    // Item pointers first: removing shifts the array.
                    let items: Vec<usize> = dups
                        .iter()
                        .map(|&(_, j)| self.item(base + j * ENTRY_SIZE))
                        .collect::<Result<_, _>>()?;
                    let remove: RemoveFn = std::mem::transmute(self.inventory_remove);
                    let add: AddFn = std::mem::transmute(self.inventory_add);
                    for item in items {
                        if remove(self.inventory, item) == 0 {
                            return Err("merge: the game didn't find a stack to remove".into());
                        }
                        add(self.inventory, item, 0);
                        merged += 1;
                    }
                }
            }
            let (entries, _) = self.read()?;
            if Self::totals(&entries) != totals {
                return Err("item counts changed while merging stacks (nothing sorted)".into());
            }
            let order = sort::order(&entries);
            if merged == 0 && sort::is_sorted(&entries, &order) {
                return Ok(Sorted::AlreadySorted {
                    entries: entries.len(),
                });
            }
            let (base, n) = self.entries()?;
            let raw: Vec<[u8; ENTRY_SIZE]> = (0..n)
                .map(|i| std::ptr::read((base + i * ENTRY_SIZE) as *const [u8; ENTRY_SIZE]))
                .collect();
            for (slot, &from) in order.iter().enumerate() {
                let mut e = raw[from];
                e[ENTRY_SEQ..ENTRY_SEQ + 4].copy_from_slice(&(slot as i32).to_le_bytes());
                std::ptr::write((base + slot * ENTRY_SIZE) as *mut [u8; ENTRY_SIZE], e);
            }
            // New items must still sort after the renumbered ones.
            let next = (self.inventory + INV_NEXT_SEQ) as *mut i32;
            if std::ptr::read_volatile(next) < n as i32 {
                std::ptr::write_volatile(next, n as i32);
            }
            self.mark_changed();
            Ok(Sorted::Sorted { entries: n, merged })
        }
    }

    /// Sets every listener's "inventory changed" flag, as the game's own
    /// add and remove do, so the panels rebuild their layout.
    unsafe fn mark_changed(&self) {
        unsafe {
            let n = r32(self.inventory + INV_LISTENER_COUNT);
            let list = rptr(self.inventory + INV_LISTENERS);
            if list == 0 {
                return;
            }
            for i in 0..n.clamp(0, 64) as usize {
                let l = rptr(list + i * 8);
                if l != 0 {
                    std::ptr::write_volatile((l + LISTENER_DIRTY) as *mut u8, 1);
                }
            }
        }
    }
}

/// The item's definition name, or "" if it can't be read.
unsafe fn def_name(item: usize) -> String {
    unsafe {
        let def = rptr(item + ITEM_DEF);
        if def == 0 {
            return String::new();
        }
        let p = rptr(def + DEF_NAME_DATA);
        let len = r32(def + DEF_NAME_LEN);
        if p == 0 || !(0..=256).contains(&len) {
            return String::new();
        }
        let s = std::slice::from_raw_parts(p as *const u8, len as usize);
        String::from_utf8_lossy(s).into_owned()
    }
}
