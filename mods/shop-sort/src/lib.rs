//! `shop-sort`: keeps the merchant's list in shops sorted (README, "Mod
//! designs").
//!
//! The game's rules: when a shop opens, the shop panel copies the shop's
//! stock into a temporary inventory (panel+0x160) shown by the merchant
//! list's grid (panel+0x188). Like every item list, the grid sorts its
//! inventory's entries by (category, entry seq) whenever it rebuilds its
//! layout (after the "inventory changed" flag, grid+0x100, is set by an add
//! or remove). Buying and selling find the item by pointer and change the
//! shop's real stock by item name, so the temporary list's order is only
//! what the player sees.
//!
//! The mod detours the grid's layout update. For the merchant grid only,
//! and only when the game is about to rebuild it anyway (the flag is set),
//! it renumbers the merchant entries' seqs in its own order ([`sort`]) and
//! lets the game's sort do the rest. Rebuilding exactly when the game does
//! keeps the shop's own index lookups right: the sell code calls the update
//! right after adding the sold item and then looks up its slot, and the buy
//! code reads the selected slot right after an update.
//!
//! Nothing is written to the game's stock, to the party or to saves.

pub mod sort;

use std::sync::atomic::{AtomicUsize, Ordering};

use uk_qol_runtime::game;
use uk_qol_runtime::guard;
use uk_qol_runtime::host::{LoadCtx, Mod};

use sort::Entry;

const ID: &str = "shop-sort";

// Game structures (1.0.5).
/// Shop panel: the merchant's inventory and its grid.
const PANEL_MERCHANT_INV: usize = 0x160;
const PANEL_MERCHANT_GRID: usize = 0x188;
/// Grid: +0x00 its Inventory*, +0x100 "inventory changed" flag.
const GRID_INVENTORY: usize = 0x00;
const GRID_DIRTY: usize = 0x100;
/// Inventory: next seq, entry count, entries (0x18 bytes each).
const INV_NEXT_SEQ: usize = 0x00;
const INV_COUNT: usize = 0x08;
const INV_ENTRIES: usize = 0x10;
const ENTRY_SIZE: usize = 0x18;
const ENTRY_ITEM: usize = 0x00;
const ENTRY_SEQ: usize = 0x0c;
/// Item fields.
const ITEM_DEF: usize = 0x08;
const ITEM_TYPE: usize = 0x100;
const ITEM_WEAPON_TYPE: usize = 0x120; // params +0
const ITEM_BUY_PRICE: usize = 0x42c; // params +0x30c
/// ItemDef: its name (String).
const DEF_NAME: usize = 0x78;

/// Sanity limit on the merchant's entry count (shops stock tens of items).
const MAX_ENTRIES: i32 = 4096;

type GridUpdateFn = unsafe extern "system" fn(usize);

static ORIG_UPDATE: AtomicUsize = AtomicUsize::new(0);
/// The shop panel global (resolved), set before the hook is enabled.
static PANEL: AtomicUsize = AtomicUsize::new(0);

/// Reads the merchant inventory's entries (index order). `None` if anything
/// looks wrong, in which case nothing is changed.
///
/// # Safety
/// `inv` must be the shop panel's merchant inventory, on the game's thread.
unsafe fn read_entries(inv: usize) -> Option<(usize, Vec<Entry>)> {
    // SAFETY: checked reads of the game's inventory and its items.
    unsafe {
        let n = game::read::<i32>(inv + INV_COUNT)?;
        if !(0..=MAX_ENTRIES).contains(&n) {
            return None;
        }
        let n = n as usize;
        let base = game::read::<usize>(inv + INV_ENTRIES)?;
        if n == 0 {
            return Some((base, Vec::new()));
        }
        if base == 0 || !game::readable(base, n * ENTRY_SIZE) {
            return None;
        }
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let e = base + i * ENTRY_SIZE;
            let item = game::read::<usize>(e + ENTRY_ITEM)?;
            if item == 0 {
                return None;
            }
            let item_type = game::read::<i32>(item + ITEM_TYPE)?;
            let weapon_type = if item_type == sort::ITEM_TYPE_WEAPON {
                game::read::<i32>(item + ITEM_WEAPON_TYPE)?
            } else {
                0
            };
            let def = game::read::<usize>(item + ITEM_DEF)?;
            let name = if def == 0 {
                String::new()
            } else {
                game::read_string(def + DEF_NAME).unwrap_or_default()
            };
            out.push(Entry {
                item_type,
                weapon_type,
                buy_price: game::read::<i32>(item + ITEM_BUY_PRICE)?,
                name,
                seq: game::read::<i32>(e + ENTRY_SEQ)?,
            });
        }
        Some((base, out))
    }
}

/// Renumbers the merchant entries' seqs in the mod's order.
///
/// # Safety
/// As for [`read_entries`].
unsafe fn renumber(inv: usize) {
    // SAFETY: forwarded.
    let Some((base, entries)) = (unsafe { read_entries(inv) }) else {
        return;
    };
    let seqs = sort::new_seqs(&entries);
    // SAFETY: the entry array was just checked readable; the game's main
    // thread owns it and nothing else runs on it during this call.
    unsafe {
        for (i, &s) in seqs.iter().enumerate() {
            std::ptr::write_volatile((base + i * ENTRY_SIZE + ENTRY_SEQ) as *mut i32, s);
        }
        // Items added later (a sale) get a seq after every renumbered one;
        // the next rebuild renumbers them anyway.
        let next = (inv + INV_NEXT_SEQ) as *mut i32;
        if std::ptr::read_volatile(next) < seqs.len() as i32 {
            std::ptr::write_volatile(next, seqs.len() as i32);
        }
    }
}

/// The grid layout update (see `patterns.toml`).
unsafe extern "system" fn grid_update(grid: usize) {
    let panel = PANEL.load(Ordering::Relaxed);
    if panel != 0 && grid == panel + PANEL_MERCHANT_GRID {
        let inv = panel + PANEL_MERCHANT_INV;
        guard::call(ID, || {
            // SAFETY: the shop panel global, read on the game's thread; the
            // merchant grid always shows the panel's merchant inventory, and
            // the check below confirms it before anything is written.
            unsafe {
                if game::read::<u8>(grid + GRID_DIRTY).unwrap_or(0) != 0
                    && game::read::<usize>(grid + GRID_INVENTORY) == Some(inv)
                {
                    renumber(inv);
                }
            }
        });
    }
    // SAFETY: set before the hook was enabled; the game's argument.
    let orig: GridUpdateFn = unsafe { std::mem::transmute(ORIG_UPDATE.load(Ordering::Relaxed)) };
    // SAFETY: the original with the game's argument.
    unsafe { orig(grid) };
}

// --- Mod --------------------------------------------------------------------

pub struct ShopSort;

impl Mod for ShopSort {
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
        let update = at("grid_update")?;
        PANEL.store(at("g_shop_panel")?, Ordering::Relaxed);
        // SAFETY: the target is the function start (verified pattern); the
        // detour has its signature and ABI. The trampoline is stored before
        // the host enables the hook.
        unsafe {
            ORIG_UPDATE.store(
                cx.hooks
                    .detour(update, grid_update as GridUpdateFn as usize)
                    .map_err(|e| e.to_string())?,
                Ordering::Relaxed,
            );
        }
        Ok(())
    }
}

/// Constructor passed to `uk_qol_runtime::start`.
pub fn new() -> Box<dyn Mod> {
    Box::new(ShopSort)
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
        assert!(manifest.enabled, "display only: on by default");
        assert!(
            manifest.defaults().is_empty(),
            "no settings: enabled is the switch"
        );
        let pats = PatternFile::parse(m.patterns()).unwrap();
        assert_eq!(pats.patterns.len(), 2);
    }

    /// A fake merchant inventory: entries and items laid out like the game's.
    struct Fake {
        inv: Vec<u8>,
        entries: Vec<u8>,
        items: Vec<Vec<u8>>,
        defs: Vec<Vec<u8>>,
        names: Vec<Vec<u8>>,
    }

    fn put<T: Copy>(buf: &mut [u8], at: usize, v: T) {
        // SAFETY: in bounds (the test's own buffers).
        unsafe { std::ptr::write_unaligned(buf.as_mut_ptr().add(at).cast::<T>(), v) };
    }

    fn fake(stock: &[(i32, i32, i32, &str)]) -> Fake {
        let mut f = Fake {
            inv: vec![0; 0x28],
            entries: vec![0; stock.len().max(1) * ENTRY_SIZE],
            items: Vec::new(),
            defs: Vec::new(),
            names: Vec::new(),
        };
        for &(_, _, _, name) in stock {
            f.names.push(name.as_bytes().to_vec());
            f.defs.push(vec![0; 0x100]);
            f.items.push(vec![0; 0x440]);
        }
        for (i, &(item_type, weapon_type, price, _)) in stock.iter().enumerate() {
            let name = f.names[i].as_ptr() as usize;
            put(&mut f.defs[i], DEF_NAME + game::STRING_DATA, name);
            put(
                &mut f.defs[i],
                DEF_NAME + game::STRING_LEN,
                f.names[i].len() as i32,
            );
            let def = f.defs[i].as_ptr() as usize;
            let item = &mut f.items[i];
            put(item, ITEM_DEF, def);
            put(item, ITEM_TYPE, item_type);
            put(item, ITEM_WEAPON_TYPE, weapon_type);
            put(item, ITEM_BUY_PRICE, price);
            let item = f.items[i].as_ptr() as usize;
            put(&mut f.entries, i * ENTRY_SIZE + ENTRY_ITEM, item);
            put(&mut f.entries, i * ENTRY_SIZE + ENTRY_SEQ, i as i32);
        }
        put(&mut f.inv, INV_NEXT_SEQ, stock.len() as i32);
        put(&mut f.inv, INV_COUNT, stock.len() as i32);
        let entries = f.entries.as_ptr() as usize;
        put(&mut f.inv, INV_ENTRIES, entries);
        f
    }

    fn seqs(f: &Fake) -> Vec<i32> {
        (0..f.items.len())
            .map(|i| {
                i32::from_le_bytes(
                    f.entries[i * ENTRY_SIZE + ENTRY_SEQ..][..4]
                        .try_into()
                        .unwrap(),
                )
            })
            .collect()
    }

    #[test]
    fn reads_and_renumbers_a_merchant_inventory() {
        let mut f = fake(&[
            (1, 2, 90, "axe_big"),
            (5, 0, 80, "armor_chain"),
            (1, 0, 20, "sword_short"),
            (1, 2, 30, "axe_small"),
        ]);
        let inv = f.inv.as_mut_ptr() as usize;
        // SAFETY: the fake is laid out like the game's.
        let (_, entries) = unsafe { read_entries(inv) }.unwrap();
        assert_eq!(entries[0].name, "axe_big");
        assert_eq!(entries[1].weapon_type, 0);
        assert_eq!(entries[3].buy_price, 30);
        // SAFETY: as above.
        unsafe { renumber(inv) };
        // Weapons: sword (0), small axe (1), big axe (2); armor last (3).
        assert_eq!(seqs(&f), [2, 3, 0, 1]);
    }

    #[test]
    fn weapon_kind_only_for_weapons() {
        // A non-weapon with a nonzero weapon_type field must not split up.
        let mut f = fake(&[(7, 9, 50, "helmet_b"), (7, 0, 10, "helmet_a")]);
        let inv = f.inv.as_mut_ptr() as usize;
        // SAFETY: the fake is laid out like the game's.
        unsafe { renumber(inv) };
        assert_eq!(seqs(&f), [1, 0]);
    }

    #[test]
    fn leaves_a_broken_inventory_alone() {
        let mut f = fake(&[(1, 0, 1, "a"), (1, 0, 2, "b")]);
        put(&mut f.entries, ENTRY_SIZE + ENTRY_ITEM, 0usize);
        put(&mut f.entries, ENTRY_SEQ, 7i32);
        let inv = f.inv.as_mut_ptr() as usize;
        // SAFETY: the fake is laid out like the game's (one null item).
        unsafe { renumber(inv) };
        assert_eq!(seqs(&f), [7, 1]);
    }
}
