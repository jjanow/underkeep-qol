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
/// Statuses that keep a hero from acting (TurnManager checks the same two).
const STATUS_NO_ACTION: [u32; 2] = [0x0100_0005, 0x0100_0003];

// Item.
const ITEM_OBJECT: usize = 0x08; // game object; def name String at +0x78
const OBJECT_DEF_NAME: usize = 0x78;
const ITEM_COUNT: usize = 0xd0;
const ITEM_COOLDOWN: usize = 0xe8;
const ITEM_TYPE: usize = 0x100;
const ITEM_WEAPON_TYPE: usize = 0x120;
const ITEM_TWO_HANDED: usize = 0x430;
const ITEM_STACKABLE: usize = 0x432;
const ITEM_QUEST: usize = 0x433;
const TYPE_WEAPON: i32 = 1;
const WEAPON_BOW: i32 = 5;

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

/// Cursor state: +0x58 drag phase (0 = nothing held).
const CURSOR_DRAG: usize = 0x58;

/// `g_state` while playing.
const STATE_PLAYING: i32 = 3;

type EquipFn = unsafe extern "C" fn(*mut u8, *mut u8, i32, i32, u8);
type UnequipFn = unsafe extern "C" fn(*mut u8, i32, i32, u8);
type CreateFn = unsafe extern "C" fn(*const EngineString) -> *mut u8;
type TakeFn = unsafe extern "C" fn(*mut u8, u8, i32) -> u64;
type AddFn = unsafe extern "C" fn(*mut u8, *mut u8, u8) -> *mut u8;
type BattleFn = unsafe extern "C" fn() -> u64;
type PushFn = unsafe extern "C" fn(*mut u8, i32) -> *mut u8;

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
pub struct Game {
    equip_item: EquipFn,
    unequip: UnequipFn,
    item_create: CreateFn,
    item_take: TakeFn,
    inventory_add: AddFn,
    in_battle: BattleFn,
    push_action: PushFn,
    turn_manager: usize,
    party: usize,
    inventory: usize,
    selected_hero: usize,
    game_state: usize,
    ui_mode: usize,
    cursor: usize,
    target_mode: usize,
}

/// A game item pointer (only compared or read while it is known to be in
/// the inventory or on a hero).
pub type Item = *mut u8;

/// What's in the way of changing equipment right now.
pub fn busy_reason(r: &Readiness) -> Option<&'static str> {
    if r.game_state != STATE_PLAYING {
        Some("not in a game")
    } else if r.target_mode != 0 {
        Some("the game is waiting for a target")
    } else if r.drag != 0 {
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
    pub bow: bool,
    pub two_handed: bool,
    pub stackable: bool,
    pub quest: bool,
}

impl ItemInfo {
    pub fn stored(&self) -> StoredItem {
        StoredItem {
            def: self.def.clone(),
            count: self.count,
            cooldown: self.cooldown,
        }
    }

    /// Why this item can't go into (or through) the ranged slot, if so.
    pub fn slot_problem(&self) -> Option<&'static str> {
        if self.stackable {
            Some("it stacks")
        } else if self.quest {
            Some("it's a quest item")
        } else {
            None
        }
    }
}

unsafe fn rd<T: Copy>(addr: usize) -> T {
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
                turn_manager: addr("turn_manager")?,
                party: addr("party")?,
                inventory: addr("party_inventory")?,
                selected_hero: addr("selected_hero")?,
                game_state: addr("game_state")?,
                ui_mode: addr("ui_mode")?,
                cursor: addr("cursor")?,
                target_mode: addr("target_mode")?,
            })
        }
    }

    pub fn readiness(&self) -> Readiness {
        // SAFETY: globals of the running game.
        unsafe {
            let cursor: usize = rd(self.cursor);
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
        unsafe { rd::<i32>(self.game_state) == STATE_PLAYING }
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
                bow: rd::<i32>(a + ITEM_TYPE) == TYPE_WEAPON
                    && rd::<i32>(a + ITEM_WEAPON_TYPE) == WEAPON_BOW,
                two_handed: rd::<u8>(a + ITEM_TWO_HANDED) != 0,
                stackable: rd::<u8>(a + ITEM_STACKABLE) != 0,
                quest: rd::<u8>(a + ITEM_QUEST) != 0,
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
    fn create(&self, s: &StoredItem) -> Result<Item, String> {
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
    unsafe fn take(&self, it: Item) {
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
            .is_some_and(|ri| ri.slot_problem().is_none());
        let bow_involved =
            r_info.as_ref().is_some_and(|ri| ri.bow) || w_info.as_ref().is_some_and(|wi| wi.bow);
        if !r_ok || !bow_involved {
            // SAFETY: ours, not handed over.
            unsafe { self.take(r) };
            return Err(if r_ok {
                "one of the two weapons must be a bow".into()
            } else {
                format!("the stored {} can't be equipped", stored.def)
            });
        }
        // The HUD hand slot's own call: checks, scripts, stats, animation.
        // SAFETY: a live hero and our fresh item, on the game thread.
        unsafe { (self.equip_item)(self.hero(i) as *mut u8, r, SLOT_WEAPON, 3, 1) };
        if self.equipped(i, SLOT_WEAPON) != r {
            // Refused (class, requirements). It may have been left in the
            // inventory or nowhere; `take` handles both.
            // SAFETY: still ours or in the inventory.
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
        unsafe { (self.equip_item)(self.hero(i) as *mut u8, s, SLOT_SHIELD, 3, 1) };
        self.equipped(i, SLOT_SHIELD) == s
    }

    /// Takes inventory item `it` (expected def `def`) out of the game into
    /// a stored form. Only bows.
    pub fn store_from_inventory(&self, it: Item, def: &str) -> Result<StoredItem, String> {
        if !self.in_inventory(it) {
            return Err(format!("{def} is no longer in the inventory"));
        }
        // SAFETY: in the inventory.
        let info = unsafe { self.item_info(it) }.ok_or("can't read the item")?;
        if info.def != def {
            return Err(format!("{def} is no longer in the inventory"));
        }
        if !info.bow {
            return Err(format!("{def} isn't a ranged weapon"));
        }
        if let Some(p) = info.slot_problem() {
            return Err(format!("{def} can't go in the ranged slot: {p}"));
        }
        let s = info.stored();
        // SAFETY: in the inventory.
        unsafe { self.take(it) };
        if self.in_inventory(it) {
            return Err(format!("the game kept {def} in the inventory"));
        }
        Ok(s)
    }

    /// Moves hero `i`'s bow in hand to the inventory (the game's unequip),
    /// then stores it.
    pub fn store_equipped(&self, i: usize) -> Result<StoredItem, String> {
        let w = self.equipped(i, SLOT_WEAPON);
        // SAFETY: the equipped weapon.
        let info = unsafe { self.item_info(w) }.ok_or("no weapon in hand")?;
        if !info.bow {
            return Err(format!("{} isn't a ranged weapon", info.def));
        }
        if let Some(p) = info.slot_problem() {
            return Err(format!("{} can't go in the ranged slot: {p}", info.def));
        }
        // SAFETY: a live hero; mode 1 = as from the character sheet.
        unsafe { (self.unequip)(self.hero(i) as *mut u8, SLOT_WEAPON, 1, 1) };
        if self.equipped(i, SLOT_WEAPON) == w {
            return Err("the game didn't unequip it".into());
        }
        self.store_from_inventory(w, &info.def)
    }

    /// Creates `s` in the party inventory.
    pub fn to_inventory(&self, s: &StoredItem) -> Result<(), String> {
        let it = self.create(s)?;
        // SAFETY: a fresh item handed to the inventory.
        unsafe { (self.inventory_add)(self.inventory as *mut u8, it, 1) };
        Ok(())
    }

    /// Ends hero `i`'s turn the way the game does for a hero who is done:
    /// round action 3, which marks them as having acted and moves on to
    /// the next hero (or the monsters).
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
