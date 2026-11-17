//! Access to the game's heroes, items and functions (Underkeep 1.0.5
//! layouts; addresses from `patterns.toml`). Everything here runs on the
//! game's main thread (the mod's `on_frame` and menu), between frames.
//!
//! Item ownership: an item leaves the game only through `Item_Take` (the
//! game's own "used up" path, which removes it from its hero or the
//! inventory and queues the object for deletion) and enters it only through
//! `Item_Create` followed by `Hero::EquipItem` or `Inventory::Add`.

use std::collections::BTreeMap;
use std::sync::Mutex;

use crate::state::{StoredItem, valid_def};

// Hero (0x2b8 bytes, 4 at party + 0x10).
const PARTY_HEROES: usize = 0x10;
const HERO_SIZE: usize = 0x2b8;
const HERO_VALID: usize = 0x00;
const HERO_NAME: usize = 0x10; // String
const HERO_HP: usize = 0xfc;
const HERO_ACTED: usize = 0x208;
const HERO_STATUS_COUNT: usize = 0x298;
const HERO_STATUS: usize = 0x2a0;
const STATUS_SIZE: usize = 0x2c8;
const HERO_EQUIP: usize = 0x210;
/// Equipment slots: 4 weapon, 5 shield (of 11).
pub const SLOTS: usize = 11;
pub const SLOT_WEAPON: i32 = 4;
pub const SLOT_SHIELD: i32 = 5;
/// `Hero::EquipItem` / `Unequip` mode: as from the inventory panel.
const MODE_PANEL: i32 = 1;
/// Statuses that keep a hero from acting (TurnManager checks the same two).
const STATUS_NO_ACTION: [u32; 2] = [0x0100_0005, 0x0100_0003];

// Item.
const ITEM_OBJECT: usize = 0x08; // game object; def name String at +0x78
const OBJECT_DEF_NAME: usize = 0x78;
const ITEM_COUNT: usize = 0xd0;
const ITEM_COOLDOWN: usize = 0xe8;
const ITEM_TYPE: usize = 0x100;
const ITEM_TWO_HANDED: usize = 0x430;
const ITEM_STACKABLE: usize = 0x432;
/// Item type 1: weapons (swords, daggers, maces, axes, staves, bows).
const TYPE_WEAPON: i32 = 1;

// Inventory.
const INV_COUNT: usize = 0x08;
const INV_ENTRIES: usize = 0x10;
const INV_ENTRY_SIZE: usize = 0x18;

// Engine String: {refcounted buffer or null, data, len}.
const STR_DATA: usize = 0x08;
const STR_LEN: usize = 0x10;

/// TurnManager: +0 phase (1 while monsters act), +8 queued actions.
const TM_PHASE: usize = 0x00;
const TM_COUNT: usize = 0x08;
/// Round action "this hero is done for the round" (+8: hero index).
const ACTION_HERO_DONE: i32 = 3;
const ACTION_HERO: usize = 0x08;

/// The game's immediate-mode UI context (mouse, drag state). The `cursor`
/// global points at it only during the game's UI pass and is null
/// outside it (where the mod's queue runs, between frames; the HUD
/// widgets draw inside the game's frame, where it may be set or not);
/// the context itself is in the `ui_context` global, which is what the
/// mod reads and points `cursor` at for its calls (`in_ui`). +0x58 drag phase: 0
/// none, 1 pressed on a draggable, 2 carried; +0x5c drag style: 1 for
/// items (click picks up, the next click drops); +0x60 what is carried: 0
/// a hero's equipment slot (+0x7c slot, +0x80 hero), 1 a party inventory
/// item, 2 others (floor, containers).
const CURSOR_DRAG: usize = 0x58;
const CURSOR_DRAG_STYLE: usize = 0x5c;
const CURSOR_DRAG_KIND: usize = 0x60;
const CURSOR_DRAG_SLOT: usize = 0x7c;
const CURSOR_DRAG_HERO: usize = 0x80;
/// The pressed widget's id (two qwords), set when the button goes down on
/// a draggable: the HUD hand slots use `0x1c + 2 * hero, 0`, the inventory
/// panel `panel, UI object id`.
const CURSOR_PRESS_ID: usize = 0x68;
const HAND_SLOT_ID: u64 = 0x1c;
/// Drag kinds (+0x60).
pub const KIND_EQUIPPED: i32 = 0;
pub const KIND_INVENTORY: i32 = 1;

/// The character sheet: equipment slot `k`'s rect `{x, y, w, h}` (f32, GUI
/// space, rewritten every frame while the sheet is open) at
/// `sheet + 0x14 + k * 0x28`.
const SHEET_RECTS: usize = 0x14;
const SHEET_SLOT_SIZE: usize = 0x28;
/// UI mode of the character sheet.
pub const UI_SHEET: i32 = 6;

/// `g_state` values in a game: 4 after loading a save (seen in game),
/// 3 for a game started from the menu (static analysis).
const STATES_PLAYING: [i32; 2] = [3, 4];

type EquipFn = unsafe extern "C" fn(*mut u8, *mut u8, i32, i32, u8);
type UnequipFn = unsafe extern "C" fn(*mut u8, i32, i32, u8);
type CreateFn = unsafe extern "C" fn(*const EngineString) -> *mut u8;
type TakeFn = unsafe extern "C" fn(*mut u8, u8, i32) -> u64;
type AddFn = unsafe extern "C" fn(*mut u8, *mut u8, u8) -> *mut u8;
type BattleFn = unsafe extern "C" fn() -> u64;
type PushFn = unsafe extern "C" fn(*mut u8, i32) -> *mut u8;
type DragItemFn = unsafe extern "C" fn() -> *mut u8;
type DragEndFn = unsafe extern "C" fn();

/// An engine `String` as passed by value-pointer. A null buffer marks a
/// literal (the game passes its own string constants like this), so the
/// data must outlive every object that keeps it: see [`intern`].
#[repr(C)]
pub struct EngineString {
    buf: *mut i32,
    data: *const u8,
    len: i32,
    _pad: [u32; 3],
}

static INTERNED: Mutex<BTreeMap<String, &'static [u8]>> = Mutex::new(BTreeMap::new());

/// A NUL-terminated copy of `s` that lives for the rest of the process
/// (one per distinct def name: a few dozen at most).
fn intern(s: &str) -> &'static [u8] {
    let mut m = INTERNED.lock().unwrap_or_else(|e| e.into_inner());
    m.entry(s.to_owned()).or_insert_with(|| {
        let mut v = s.as_bytes().to_vec();
        v.push(0);
        Box::leak(v.into_boxed_slice())
    })
}

/// What the mod needs from the game, resolved at load.
#[derive(Clone, Copy)]
pub struct Game {
    equip_item: EquipFn,
    unequip: UnequipFn,
    item_create: CreateFn,
    item_take: TakeFn,
    inventory_add: AddFn,
    in_battle: BattleFn,
    push_action: PushFn,
    drag_item: DragItemFn,
    drag_end: DragEndFn,
    char_sheet: usize,
    turn_manager: usize,
    party: usize,
    inventory: usize,
    selected_hero: usize,
    game_state: usize,
    ui_mode: usize,
    cursor: usize,
    ui_context: usize,
    target_mode: usize,
}

/// A game item pointer (only compared or read while it is known to be in
/// the inventory or on a hero).
pub type Item = *mut u8;

/// What's in the way of changing equipment right now.
pub fn busy_reason(r: &Readiness) -> Option<&'static str> {
    if !STATES_PLAYING.contains(&r.game_state) {
        Some("not in a game")
    } else if r.target_mode != 0 {
        Some("the game is waiting for a target")
    } else if r.drag > 1 {
        Some("an item is held on the cursor")
    } else {
        None
    }
}

/// Raw state values, for the checks and the log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Readiness {
    pub game_state: i32,
    pub ui_mode: i32,
    pub target_mode: i32,
    pub drag: i32,
    pub tm_phase: i32,
    pub tm_count: i32,
}

/// One item's facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemInfo {
    pub ptr: usize,
    pub def: String,
    pub count: i32,
    pub cooldown: i32,
    /// A weapon (fits the weapon hand and the second weapon slot).
    pub weapon: bool,
    pub two_handed: bool,
    pub stackable: bool,
}

impl ItemInfo {
    pub fn stored(&self) -> StoredItem {
        StoredItem {
            def: self.def.clone(),
            count: self.count,
            cooldown: self.cooldown,
        }
    }

    /// Why this item can't go into (or through) the second weapon slot, if so.
    pub fn slot_problem(&self) -> Option<&'static str> {
        // Quest weapons may go in: quest scripts asking for one get it back
        // in the inventory first (`quest`).
        if self.stackable {
            Some("it stacks")
        } else {
            None
        }
    }
}

pub(crate) unsafe fn rd<T: Copy>(addr: usize) -> T {
    // SAFETY: the caller passes an address inside a live game object.
    unsafe { std::ptr::read_unaligned(addr as *const T) }
}

unsafe fn wr<T: Copy>(addr: usize, v: T) {
    // SAFETY: as for `rd`.
    unsafe { std::ptr::write_unaligned(addr as *mut T, v) }
}

/// Reads an engine String (data pointer + length) as text.
unsafe fn read_string(at: usize) -> Option<String> {
    // SAFETY: `at` is an engine String inside a live object.
    unsafe {
        let data: usize = rd(at + STR_DATA);
        let len: i32 = rd(at + STR_LEN);
        if data == 0 || !(0..=256).contains(&len) {
            return None;
        }
        let bytes = std::slice::from_raw_parts(data as *const u8, len as usize);
        Some(String::from_utf8_lossy(bytes).into_owned())
    }
}

impl Game {
    /// Builds from resolved addresses (`base` + RVA from the patterns).
    ///
    /// # Safety
    /// Each address must be what its pattern names in the running game.
    pub unsafe fn new(addr: impl Fn(&str) -> Result<usize, String>) -> Result<Game, String> {
        // SAFETY: function addresses from verified patterns, with the
        // signatures documented in patterns.toml.
        unsafe {
            Ok(Game {
                equip_item: std::mem::transmute::<usize, EquipFn>(addr("hero_equip_item")?),
                unequip: std::mem::transmute::<usize, UnequipFn>(addr("hero_unequip")?),
                item_create: std::mem::transmute::<usize, CreateFn>(addr("item_create")?),
                item_take: std::mem::transmute::<usize, TakeFn>(addr("item_take")?),
                inventory_add: std::mem::transmute::<usize, AddFn>(addr("inventory_add")?),
                in_battle: std::mem::transmute::<usize, BattleFn>(addr("in_battle")?),
                push_action: std::mem::transmute::<usize, PushFn>(addr("round_action_push")?),
                drag_item: std::mem::transmute::<usize, DragItemFn>(addr("drag_item")?),
                drag_end: std::mem::transmute::<usize, DragEndFn>(addr("drag_end")?),
                char_sheet: addr("char_sheet")?,
                turn_manager: addr("turn_manager")?,
                party: addr("party")?,
                inventory: addr("party_inventory")?,
                selected_hero: addr("selected_hero")?,
                game_state: addr("game_state")?,
                ui_mode: addr("ui_mode")?,
                cursor: addr("cursor")?,
                ui_context: addr("ui_context")?,
                target_mode: addr("target_mode")?,
            })
        }
    }

    pub fn readiness(&self) -> Readiness {
        // SAFETY: globals of the running game.
        unsafe {
            let cursor: usize = rd(self.ui_context);
            Readiness {
                game_state: rd(self.game_state),
                ui_mode: rd(self.ui_mode),
                target_mode: rd(self.target_mode),
                drag: if cursor == 0 {
                    0
                } else {
                    rd(cursor + CURSOR_DRAG)
                },
                tm_phase: rd(self.turn_manager + TM_PHASE),
                tm_count: rd(self.turn_manager + TM_COUNT),
            }
        }
    }

    pub fn playing(&self) -> bool {
        // SAFETY: a global of the running game.
        STATES_PLAYING.contains(&unsafe { rd::<i32>(self.game_state) })
    }

    pub fn in_battle(&self) -> bool {
        // SAFETY: the game's own check; reads the level's monster lists.
        unsafe { (self.in_battle)() & 0xff != 0 }
    }

    pub fn selected_hero(&self) -> Option<usize> {
        // SAFETY: a global of the running game.
        let i: i32 = unsafe { rd(self.selected_hero) };
        usize::try_from(i).ok().filter(|&i| i < 4)
    }

    fn hero(&self, i: usize) -> usize {
        assert!(i < 4);
        self.party + PARTY_HEROES + i * HERO_SIZE
    }

    pub fn hero_valid(&self, i: usize) -> bool {
        // SAFETY: inside the party global.
        unsafe { rd::<u8>(self.hero(i) + HERO_VALID) != 0 }
    }

    /// The savehook's key for hero `i` (`"<hero_id>:<name>"`).
    pub fn hero_key(&self, i: usize) -> Option<String> {
        if !self.hero_valid(i) {
            return None;
        }
        // SAFETY: a hero struct of the party.
        unsafe { uk_qol_runtime::savehook::hero_key(self.hero(i)) }
    }

    pub fn hero_name(&self, i: usize) -> String {
        // SAFETY: inside the party global.
        unsafe { read_string(self.hero(i) + HERO_NAME) }
            .unwrap_or_else(|| format!("Hero {}", i + 1))
    }

    /// Whether hero `i` can still act this round (as the TurnManager checks
    /// before an attack): present, alive, not done, not stunned or asleep.
    pub fn hero_can_act(&self, i: usize) -> bool {
        if !self.hero_valid(i) {
            return false;
        }
        let h = self.hero(i);
        // SAFETY: inside the party global; the status array is the hero's.
        unsafe {
            if rd::<i32>(h + HERO_HP) < 1 || rd::<u8>(h + HERO_ACTED) != 0 {
                return false;
            }
            let n: i32 = rd(h + HERO_STATUS_COUNT);
            let arr: usize = rd(h + HERO_STATUS);
            if arr != 0 && (0..=256).contains(&n) {
                for k in 0..n as usize {
                    let ty: u32 = rd(arr + k * STATUS_SIZE);
                    if STATUS_NO_ACTION.contains(&ty) {
                        return false;
                    }
                }
            }
        }
        true
    }

    pub fn equipped(&self, i: usize, slot: i32) -> Item {
        // SAFETY: inside the party global.
        unsafe { rd(self.hero(i) + HERO_EQUIP + slot as usize * 8) }
    }

    /// # Safety
    /// `it` must be a live item (on a hero or in the inventory).
    pub unsafe fn item_info(&self, it: Item) -> Option<ItemInfo> {
        if it.is_null() {
            return None;
        }
        let a = it as usize;
        // SAFETY: forwarded.
        unsafe {
            let obj: usize = rd(a + ITEM_OBJECT);
            if obj == 0 {
                return None;
            }
            let def = read_string(obj + OBJECT_DEF_NAME).filter(|d| valid_def(d))?;
            Some(ItemInfo {
                ptr: a,
                def,
                count: rd(a + ITEM_COUNT),
                cooldown: rd(a + ITEM_COOLDOWN),
                weapon: rd::<i32>(a + ITEM_TYPE) == TYPE_WEAPON,
                two_handed: rd::<u8>(a + ITEM_TWO_HANDED) != 0,
                stackable: rd::<u8>(a + ITEM_STACKABLE) != 0,
            })
        }
    }

    /// The party inventory's items, in order.
    pub fn inventory(&self) -> Vec<Item> {
        // SAFETY: the inventory global; entries hold live items.
        unsafe {
            let n: i32 = rd(self.inventory + INV_COUNT);
            let e: usize = rd(self.inventory + INV_ENTRIES);
            if e == 0 || !(0..=100_000).contains(&n) {
                return Vec::new();
            }
            (0..n as usize)
                .map(|k| rd::<Item>(e + k * INV_ENTRY_SIZE))
                .filter(|p| !p.is_null())
                .collect()
        }
    }

    pub fn in_inventory(&self, it: Item) -> bool {
        self.inventory().contains(&it)
    }

    /// Def → count over the inventory and every hero's equipment.
    pub fn tally(&self) -> BTreeMap<String, i64> {
        let mut t = BTreeMap::new();
        let mut add = |it: Item| {
            // SAFETY: items from the inventory and equipment are live.
            if let Some(i) = unsafe { self.item_info(it) } {
                *t.entry(i.def).or_insert(0) += i64::from(i.count);
            }
        };
        for it in self.inventory() {
            add(it);
        }
        for h in 0..4 {
            if !self.hero_valid(h) {
                continue;
            }
            for s in 0..SLOTS as i32 {
                let it = self.equipped(h, s);
                if !it.is_null() {
                    add(it);
                }
            }
        }
        t
    }

    /// Builds an item from its saved form, like `Item::RestoreState`. The
    /// new item belongs to nobody until it's equipped or added.
    pub(crate) fn create(&self, s: &StoredItem) -> Result<Item, String> {
        if !valid_def(&s.def) {
            return Err(format!("bad item name {:?}", s.def));
        }
        let data = intern(&s.def);
        let name = EngineString {
            buf: std::ptr::null_mut(),
            data: data.as_ptr(),
            len: s.def.len() as i32,
            _pad: [0; 3],
        };
        // SAFETY: the game's item factory with a literal-style String whose
        // data lives forever.
        let it = unsafe { (self.item_create)(&name) };
        if it.is_null() {
            return Err(format!("the game has no item {:?}", s.def));
        }
        // SAFETY: a fresh item.
        unsafe {
            wr::<i32>(it as usize + ITEM_COUNT, s.count);
            wr::<i32>(it as usize + ITEM_COOLDOWN, s.cooldown);
        }
        Ok(it)
    }

    /// Takes a whole item out of the game (from its hero or the inventory)
    /// and queues it for deletion.
    ///
    /// # Safety
    /// `it` must be a live item that the game owns (on a hero, in the
    /// inventory) or one from [`Game::create`] not handed over yet.
    pub(crate) unsafe fn take(&self, it: Item) {
        // SAFETY: forwarded; `all` ignores the count.
        unsafe { (self.item_take)(it, 1, 0) };
    }

    /// Swaps hero `i`'s weapon with `stored`. On success returns the
    /// weapon that came out of the hand (already taken out of the game),
    /// if there was one, and the shield the swap pushed into the inventory.
    pub fn swap(&self, i: usize, stored: &StoredItem) -> Result<SwapResult, String> {
        if !self.hero_valid(i) {
            return Err("no hero there".into());
        }
        let w = self.equipped(i, SLOT_WEAPON);
        // SAFETY: the hero's equipped weapon.
        let w_info = unsafe { self.item_info(w) };
        if !w.is_null() && w_info.is_none() {
            return Err("can't read the weapon in hand".into());
        }
        if let Some(p) = w_info.as_ref().and_then(ItemInfo::slot_problem) {
            return Err(format!("the weapon in hand can't be swapped: {p}"));
        }
        let shield_before = self.equipped(i, SLOT_SHIELD);
        let r = self.create(stored)?;
        // SAFETY: the item just created.
        let r_info = unsafe { self.item_info(r) };
        let r_ok = r_info
            .as_ref()
            .is_some_and(|ri| ri.weapon && ri.slot_problem().is_none());
        if !r_ok {
            // SAFETY: ours, not handed over.
            unsafe { self.take(r) };
            return Err(format!("the stored {} can't be equipped", stored.def));
        }
        // Equip it as from the inventory panel: into the inventory first,
        // then the panel's own call (checks, scripts, stats, animation).
        // Mode 1, not the HUD hand slot's mode 3: that one reads the UI
        // context through the `cursor` global, which is null outside the
        // game's UI pass.
        // SAFETY: a fresh, non-stackable item (so it isn't merged away).
        unsafe { (self.inventory_add)(self.inventory as *mut u8, r, 0) };
        // SAFETY: a live hero and our item in the inventory, game thread.
        unsafe { (self.equip_item)(self.hero(i) as *mut u8, r, SLOT_WEAPON, MODE_PANEL, 1) };
        if self.equipped(i, SLOT_WEAPON) != r {
            // Refused (class, requirements): it's still in the inventory.
            // SAFETY: ours, in the inventory.
            unsafe { self.take(r) };
            return Err(format!("{} can't be equipped by this hero", stored.def));
        }
        let mut out = SwapResult::default();
        if let Some(wi) = w_info {
            if self.in_inventory(w) {
                out.out_of_hand = Some(wi.stored());
                // SAFETY: the old weapon, now in the inventory.
                unsafe { self.take(w) };
            } else {
                out.warning = Some(format!(
                    "{} didn't land in the inventory; left where the game put it",
                    wi.def
                ));
            }
        }
        if !shield_before.is_null()
            && self.equipped(i, SLOT_SHIELD).is_null()
            && self.in_inventory(shield_before)
        {
            // SAFETY: in the inventory.
            out.shield_out = unsafe { self.item_info(shield_before) }.map(|s| s.def);
        }
        Ok(out)
    }

    /// Puts a shield with def `def` from the inventory back on hero `i`,
    /// if the hand is free for it. Returns whether it did.
    pub fn reequip_shield(&self, i: usize, def: &str) -> bool {
        let w = self.equipped(i, SLOT_WEAPON);
        // SAFETY: the equipped weapon.
        if unsafe { self.item_info(w) }.is_some_and(|wi| wi.two_handed)
            || !self.equipped(i, SLOT_SHIELD).is_null()
        {
            return false;
        }
        let found = self.inventory().into_iter().find(|&it| {
            // SAFETY: inventory items.
            unsafe { self.item_info(it) }.is_some_and(|ii| ii.def == def && !ii.stackable)
        });
        let Some(s) = found else { return false };
        // SAFETY: a live hero and an inventory item.
        unsafe { (self.equip_item)(self.hero(i) as *mut u8, s, SLOT_SHIELD, MODE_PANEL, 1) };
        self.equipped(i, SLOT_SHIELD) == s
    }

    /// Takes inventory item `it` (expected def `def`) out of the game into
    /// a stored form. Only weapons.
    pub fn store_from_inventory(&self, it: Item, def: &str) -> Result<StoredItem, String> {
        if !self.in_inventory(it) {
            return Err(format!("{def} is no longer in the inventory"));
        }
        // SAFETY: in the inventory.
        let info = unsafe { self.item_info(it) }.ok_or("can't read the item")?;
        if info.def != def {
            return Err(format!("{def} is no longer in the inventory"));
        }
        if !info.weapon {
            return Err(format!("{def} isn't a weapon"));
        }
        if let Some(p) = info.slot_problem() {
            return Err(format!("{def} can't go in the second weapon slot: {p}"));
        }
        let s = info.stored();
        // SAFETY: in the inventory.
        unsafe { self.take(it) };
        if self.in_inventory(it) {
            return Err(format!("the game kept {def} in the inventory"));
        }
        Ok(s)
    }

    /// Moves hero `i`'s weapon in hand to the inventory (the game's unequip),
    /// then stores it.
    pub fn store_equipped(&self, i: usize) -> Result<StoredItem, String> {
        let w = self.equipped(i, SLOT_WEAPON);
        // SAFETY: the equipped weapon.
        let info = unsafe { self.item_info(w) }.ok_or("no weapon in hand")?;
        if !info.weapon {
            return Err(format!("{} isn't a weapon", info.def));
        }
        if let Some(p) = info.slot_problem() {
            return Err(format!(
                "{} can't go in the second weapon slot: {p}",
                info.def
            ));
        }
        // SAFETY: a live hero; mode 1 = as from the character sheet.
        unsafe { (self.unequip)(self.hero(i) as *mut u8, SLOT_WEAPON, MODE_PANEL, 1) };
        if self.equipped(i, SLOT_WEAPON) == w {
            return Err("the game didn't unequip it".into());
        }
        self.store_from_inventory(w, &info.def)
    }

    /// Creates `s` in the party inventory.
    pub fn put_in_inventory(&self, s: &StoredItem) -> Result<(), String> {
        let it = self.create(s)?;
        // SAFETY: a fresh item handed to the inventory.
        unsafe { (self.inventory_add)(self.inventory as *mut u8, it, 1) };
        Ok(())
    }

    /// Ends hero `i`'s turn: queues round action 3, whose handler in
    /// `TurnManager::_DoRoundAction` marks the hero as having acted and
    /// moves on to the next hero (or the monsters). No game code was found
    /// that queues this action itself.
    pub fn end_turn(&self, i: usize) {
        // SAFETY: the TurnManager's own queue; the entry is fresh.
        unsafe {
            let a = (self.push_action)(self.turn_manager as *mut u8, ACTION_HERO_DONE);
            if !a.is_null() {
                wr::<i32>(a as usize + ACTION_HERO, i as i32);
            }
        }
    }
}

/// An item the game has on the mouse cursor (picked up with a click, to be
/// dropped with the next one).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Carried {
    pub item: Item,
    /// [`KIND_EQUIPPED`], [`KIND_INVENTORY`] or another source.
    pub kind: i32,
    /// For [`KIND_EQUIPPED`]: the hero and slot it was picked up from.
    pub hero: i32,
    pub slot: i32,
}

/// An item click that hasn't become a carry yet ([`Game::pressed`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pressed {
    /// [`KIND_EQUIPPED`], [`KIND_INVENTORY`] or another source.
    pub kind: i32,
    /// The pressed widget's id.
    pub id: [u64; 2],
}

impl Game {
    fn context_object(&self) -> usize {
        // SAFETY: a pointer global of the running game.
        unsafe { rd(self.ui_context) }
    }

    /// Calls the game's UI function `f` (one that reads the context through
    /// the `cursor` global) as if inside the game's UI pass: the global is
    /// pointed at the UI context for the call, then put back.
    ///
    /// # Safety
    /// `f` must be one of the game's drag functions; game thread only.
    unsafe fn in_ui<R>(&self, f: impl FnOnce() -> R) -> Option<R> {
        let ctx = self.context_object();
        if ctx == 0 {
            return None;
        }
        // SAFETY: the game's own pointer global, set to its own context.
        unsafe {
            let prev: usize = rd(self.cursor);
            wr(self.cursor, ctx);
            let r = f();
            wr(self.cursor, prev);
            Some(r)
        }
    }

    /// The item the game has on the mouse cursor, if any.
    pub fn carried(&self) -> Option<Carried> {
        let c = self.context_object();
        if c == 0 {
            return None;
        }
        // SAFETY: the UI context lives as long as the game; the getter is
        // the game's own (it reads the same fields).
        unsafe {
            let phase: i32 = rd(c + CURSOR_DRAG);
            let style: i32 = rd(c + CURSOR_DRAG_STYLE);
            if phase < 2 || (style == 1 && phase != 2) {
                return None;
            }
            let item = self.in_ui(|| (self.drag_item)())?;
            if item.is_null() {
                return None;
            }
            Some(Carried {
                item,
                kind: rd(c + CURSOR_DRAG_KIND),
                hero: rd(c + CURSOR_DRAG_HERO),
                slot: rd(c + CURSOR_DRAG_SLOT),
            })
        }
    }

    /// An item click the game hasn't turned into a carry yet: the button
    /// went down on an item and is still held. The game picks the item up
    /// only if the button comes up over that same item; released anywhere
    /// else, it cancels (so a held drag moves nothing, in the game too). The
    /// second slot uses this to take a held drag released over it.
    pub fn pressed(&self) -> Option<Pressed> {
        let c = self.context_object();
        if c == 0 {
            return None;
        }
        // SAFETY: the UI context lives as long as the game.
        unsafe {
            let phase: i32 = rd(c + CURSOR_DRAG);
            let style: i32 = rd(c + CURSOR_DRAG_STYLE);
            if phase != 1 || style != 1 {
                return None;
            }
            Some(Pressed {
                kind: rd(c + CURSOR_DRAG_KIND),
                id: [rd(c + CURSOR_PRESS_ID), rd(c + CURSOR_PRESS_ID + 8)],
            })
        }
    }

    /// The party inventory item behind a pressed inventory entry (its UI
    /// object id, [`Pressed::id`]`[1]`), through the game's own lookup:
    /// the drag's payload is filled in as the game does when the click
    /// turns into a carry, read with `drag_item`, then put back.
    pub fn pressed_inventory_item(&self, p: &Pressed) -> Option<Item> {
        if p.kind != KIND_INVENTORY {
            return None;
        }
        let c = self.context_object();
        if c == 0 {
            return None;
        }
        let id = i32::try_from(p.id[1] as i64).ok()?;
        // SAFETY: the game's UI context, game thread; every field written is
        // restored before returning, and `drag_item` only reads.
        let it = unsafe {
            let phase: i32 = rd(c + CURSOR_DRAG);
            let slot: i32 = rd(c + CURSOR_DRAG_SLOT);
            wr(c + CURSOR_DRAG, 2i32);
            wr(c + CURSOR_DRAG_SLOT, id);
            let it = self.in_ui(|| (self.drag_item)());
            wr(c + CURSOR_DRAG_SLOT, slot);
            wr(c + CURSOR_DRAG, phase);
            it?
        };
        (!it.is_null() && self.in_inventory(it)).then_some(it)
    }

    /// The hero whose HUD hand slot a pressed widget id is (the HUD's hand
    /// slots use ids `0x1c + 2 * hero`), if it is one.
    pub fn pressed_hand_slot(p: &Pressed) -> Option<usize> {
        let h = p.id[0].checked_sub(HAND_SLOT_ID)?;
        (p.kind == KIND_EQUIPPED && p.id[1] == 0 && h % 2 == 0 && h / 2 < 4)
            .then_some(h as usize / 2)
    }

    /// Ends the game's drag without dropping anything: the item stays
    /// where it was picked up (what the game itself does after a drop).
    pub fn end_drag(&self) {
        // SAFETY: the game's own drag reset, game thread.
        unsafe { self.in_ui(|| (self.drag_end)()) };
    }

    pub fn ui_mode(&self) -> i32 {
        // SAFETY: a global of the running game.
        unsafe { rd(self.ui_mode) }
    }

    /// Equipment slot `k`'s rect on the character sheet `[x, y, w, h]` in
    /// GUI space (current only while the sheet is open).
    pub fn sheet_slot_rect(&self, k: i32) -> Option<[f32; 4]> {
        if !(0..SLOTS as i32).contains(&k) {
            return None;
        }
        let at = self.char_sheet + SHEET_RECTS + k as usize * SHEET_SLOT_SIZE;
        // SAFETY: inside a static of the exe; the read is checked.
        unsafe { uk_qol_runtime::game::read(at) }
    }
}

/// What a successful swap moved.
#[derive(Debug, Default)]
pub struct SwapResult {
    /// The weapon that was in hand, now out of the game: the new stored one.
    pub out_of_hand: Option<StoredItem>,
    /// A shield the two-handed weapon pushed into the inventory.
    pub shield_out: Option<String>,
    /// Something odd but harmless, for the log.
    pub warning: Option<String>,
}
