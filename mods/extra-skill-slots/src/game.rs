//! The game structures this mod reads and the functions it calls (1.0.5;
//! see `patterns.toml` for how each was found).
//!
//! - Hero (0x2b8 bytes, 4 in the party array): +0x00 valid (byte), +0x08
//!   hero_id (int), +0x10 name (String), +0x210 equipment `Item*[11]`,
//!   +0x268 skill count, +0x270 skill states (0x18 bytes each: +0x00
//!   `SkillDef*`, +0x0c level), +0x278 the selected skill: a pair of ints,
//!   (skill state index, equipment slot of an item skill or -1). An item
//!   skill's pair is (-1, slot).
//! - SkillDef: +0x00 name (String, e.g. "skill_fireball"), +0x5a4 passive.
//! - Item: +0x578 its skill (an embedded SkillDef), +0x598 non-zero if the
//!   item has a skill.
//! - RoundAction (0xe8 bytes, queued on the TurnManager): +0x00 type (4 =
//!   hero acts), +0x08 hero index (-1 = the selected one), +0x0c 1 = use the
//!   selected skill (0 = attack), +0xd0 step (0 until it first runs), +0xdc
//!   sequence number.
//! - String: { refcounted buffer, `const char*` data, int length }.

use std::ffi::c_void;

pub const HERO_SIZE: usize = 0x2b8;
pub const HEROES: usize = 4;
const EQUIP_SLOTS: i32 = 11;
const MAX_SKILLS: i32 = 256;

pub const H_VALID: usize = 0x00;
pub const H_ID: usize = 0x08;
pub const H_NAME: usize = 0x10;
pub const H_EQUIP: usize = 0x210;
pub const H_SKILL_COUNT: usize = 0x268;
pub const H_SKILLS: usize = 0x270;
pub const H_SELECTED_SKILL: usize = 0x278;
const SKILL_STATE_SIZE: usize = 0x18;
const SS_LEVEL: usize = 0x0c;
const DEF_PASSIVE: usize = 0x5a4;
const ITEM_SKILL: usize = 0x578;
const ITEM_HAS_SKILL: usize = 0x598;

pub const ACT_TYPE: usize = 0x00;
pub const ACT_HERO: usize = 0x08;
pub const ACT_USE_SKILL: usize = 0x0c;
pub const ACT_STEP: usize = 0xd0;
pub const ACT_SEQ: usize = 0xdc;
/// RoundAction type: a hero attacks or uses a skill.
pub const ACT_HERO_ACTS: i32 = 4;
/// What the game's own controls pass to the queue gate for hero actions.
const GATE_HERO_ACTION: i32 = 2;

/// A selected-skill pair as stored at Hero+0x278.
pub fn pair(index: i32, equip_slot: i32) -> u64 {
    (index as u32 as u64) | ((equip_slot as u32 as u64) << 32)
}

/// The pair for "no skill": CanCastSkill refuses it ("no skill selected").
pub const NO_SKILL: u64 = u64::MAX;

#[repr(C)]
struct GString {
    buf: *mut c_void,
    data: *const u8,
    len: i32,
}

/// # Safety
/// `addr` must be readable for `size_of::<T>()` bytes.
pub unsafe fn rd<T: Copy>(addr: usize) -> T {
    // SAFETY: forwarded from the caller.
    unsafe { (addr as *const T).read_unaligned() }
}

/// # Safety
/// `addr` must be writable for `size_of::<T>()` bytes.
pub unsafe fn wr<T: Copy>(addr: usize, v: T) {
    // SAFETY: forwarded from the caller.
    unsafe { (addr as *mut T).write_unaligned(v) }
}

/// Reads an engine String at `addr`.
///
/// # Safety
/// `addr` must point at a String.
unsafe fn gstr(addr: usize) -> String {
    // SAFETY: the caller's String; its data pointer is checked below.
    let (data, len) = unsafe { (rd::<*const u8>(addr + 8), rd::<i32>(addr + 0x10)) };
    if data.is_null() || !(0..=1024).contains(&len) {
        return String::new();
    }
    // SAFETY: a live String's data holds `len` bytes.
    let bytes = unsafe { std::slice::from_raw_parts(data, len as usize) };
    String::from_utf8_lossy(bytes).into_owned()
}

/// A skill a hero can put in a slot, as the game's skill picker offers them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    /// The skill's def name ("skill_fireball").
    pub name: String,
    /// Equipment slot of the item that grants it, or -1 for a class skill.
    pub equip_slot: i32,
}

/// Resolved addresses (absolute) of everything in `patterns.toml`.
#[derive(Debug, Clone, Copy)]
pub struct Game {
    pub do_round_action: usize,
    pub push_round_action: usize,
    pub turn_manager: usize,
    pub can_queue: usize,
    pub selected_hero: usize,
    pub heroes: usize,
    pub text_get: Option<usize>,
}

impl Game {
    pub fn hero(&self, i: usize) -> usize {
        self.heroes + i * HERO_SIZE
    }

    /// The selected hero's index, if it is a party member.
    ///
    /// # Safety
    /// The game's globals must be mapped (any time after start).
    pub unsafe fn selected(&self) -> Option<usize> {
        // SAFETY: a global int.
        let i = unsafe { rd::<i32>(self.selected_hero) };
        let i = usize::try_from(i).ok().filter(|i| *i < HEROES)?;
        // SAFETY: inside the static hero array.
        unsafe { hero_valid(self.hero(i)) }.then_some(i)
    }

    /// The game's own check before a hero-action control queues its action:
    /// false while a text field, a menu, a dialog or target selection is up.
    ///
    /// # Safety
    /// Game thread only.
    pub unsafe fn can_queue_hero_action(&self) -> bool {
        // SAFETY: the resolved function, `bool (int)`.
        let f: extern "win64" fn(i32) -> u8 = unsafe { std::mem::transmute(self.can_queue) };
        f(GATE_HERO_ACTION) != 0
    }

    /// Queues "hero `hero` uses their selected skill", as the HUD skill slot
    /// does when clicked. Returns the action's sequence number.
    ///
    /// # Safety
    /// Game thread only, in game.
    pub unsafe fn queue_use_skill(&self, hero: usize) -> Option<i32> {
        // SAFETY: the resolved function, `RoundAction* (TurnManager*, int)`.
        let push: extern "win64" fn(usize, i32) -> usize =
            unsafe { std::mem::transmute(self.push_round_action) };
        let act = push(self.turn_manager, ACT_HERO_ACTS);
        if act == 0 {
            return None;
        }
        // SAFETY: a freshly queued action.
        unsafe {
            wr::<i32>(act + ACT_HERO, hero as i32);
            wr::<i32>(act + ACT_USE_SKILL, 1);
            Some(rd::<i32>(act + ACT_SEQ))
        }
    }

    /// The text for `key` in the game's language (None if the game doesn't
    /// know it or the text lookup wasn't found).
    ///
    /// # Safety
    /// Game thread only, after the text tables are loaded.
    pub unsafe fn text(&self, key: &str) -> Option<String> {
        let f = self.text_get?;
        // SAFETY: the resolved function, `String* (String* out, const String* key)`.
        let f: extern "win64" fn(*mut GString, *const GString) -> *mut GString =
            unsafe { std::mem::transmute(f) };
        let mut z = key.as_bytes().to_vec();
        z.push(0);
        let k = GString {
            buf: std::ptr::null_mut(),
            data: z.as_ptr(),
            len: key.len() as i32,
        };
        let mut out = GString {
            buf: std::ptr::null_mut(),
            data: std::ptr::null(),
            len: 0,
        };
        f(&mut out, &k);
        // The result shares the text table's buffer; its reference is never
        // released (callers cache the text), so that buffer just stays alive.
        // SAFETY: `out` is a String the game filled in.
        let s = unsafe { gstr(&out as *const GString as usize) };
        (!s.is_empty() && s != key).then_some(s)
    }
}

/// # Safety
/// `h` must point at a hero in the party array.
pub unsafe fn hero_valid(h: usize) -> bool {
    // SAFETY: forwarded.
    unsafe { rd::<u8>(h + H_VALID) != 0 }
}

/// The hero's name.
///
/// # Safety
/// `h` must point at a valid hero.
pub unsafe fn hero_name(h: usize) -> String {
    // SAFETY: forwarded.
    unsafe { gstr(h + H_NAME) }
}

/// Stable key for per-hero state: "<hero_id>:<name>" (hero ids are only the
/// creation slot, so the name tells a replaced hero apart).
///
/// # Safety
/// `h` must point at a valid hero.
pub unsafe fn hero_key(h: usize) -> String {
    // SAFETY: forwarded.
    unsafe { format!("{}:{}", rd::<i32>(h + H_ID), gstr(h + H_NAME)) }
}

/// The hero's skill state array and its length (empty if implausible).
unsafe fn skill_states(h: usize) -> (usize, i32) {
    // SAFETY: fields of a valid hero.
    let (n, arr) = unsafe { (rd::<i32>(h + H_SKILL_COUNT), rd::<usize>(h + H_SKILLS)) };
    if arr == 0 || !(0..=MAX_SKILLS).contains(&n) {
        return (0, 0);
    }
    (arr, n)
}

/// The item in an equipment slot that grants a skill, if any.
unsafe fn skill_item(h: usize, slot: i32) -> Option<usize> {
    if !(0..EQUIP_SLOTS).contains(&slot) {
        return None;
    }
    // SAFETY: equipment pointers of a valid hero; the item is live while equipped.
    unsafe {
        let item = rd::<usize>(h + H_EQUIP + slot as usize * 8);
        (item != 0 && rd::<i32>(item + ITEM_HAS_SKILL) != 0).then_some(item)
    }
}

/// The skills the game's own skill picker offers this hero: learned active
/// class skills, then the skills of equipped items.
///
/// # Safety
/// `h` must point at a valid hero; game thread only.
pub unsafe fn assignable_skills(h: usize) -> Vec<Skill> {
    let mut out = Vec::new();
    // SAFETY: the hero's own arrays, bounds-checked above and here.
    unsafe {
        let (arr, n) = skill_states(h);
        for i in 0..n as usize {
            let st = arr + i * SKILL_STATE_SIZE;
            let def = rd::<usize>(st);
            if def == 0 || rd::<u8>(def + DEF_PASSIVE) != 0 || rd::<i32>(st + SS_LEVEL) < 1 {
                continue;
            }
            out.push(Skill {
                name: gstr(def),
                equip_slot: -1,
            });
        }
        for slot in 0..EQUIP_SLOTS {
            if let Some(item) = skill_item(h, slot) {
                out.push(Skill {
                    name: gstr(item + ITEM_SKILL),
                    equip_slot: slot,
                });
            }
        }
    }
    out.retain(|s| !s.name.is_empty());
    out
}

/// The selected-skill pair that casts `skill` for this hero now, if the
/// hero still has it (a class skill by name; an item skill by its slot,
/// checked against the name).
///
/// # Safety
/// `h` must point at a valid hero; game thread only.
pub unsafe fn resolve(h: usize, skill: &Skill) -> Option<u64> {
    // SAFETY: as in `assignable_skills`.
    unsafe {
        if skill.equip_slot >= 0 {
            let item = skill_item(h, skill.equip_slot)?;
            return (gstr(item + ITEM_SKILL) == skill.name).then(|| pair(-1, skill.equip_slot));
        }
        let (arr, n) = skill_states(h);
        (0..n as usize)
            .find(|&i| {
                let def = rd::<usize>(arr + i * SKILL_STATE_SIZE);
                def != 0 && gstr(def) == skill.name
            })
            .map(|i| pair(i as i32, -1))
    }
}

/// "skill_fire_spark" -> "Fire spark": a readable name when the game's text
/// isn't available.
pub fn pretty(name: &str) -> String {
    let s = name
        .strip_prefix("skill_")
        .unwrap_or(name)
        .replace('_', " ");
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairs() {
        assert_eq!(pair(3, -1), 0xffff_ffff_0000_0003);
        assert_eq!(pair(-1, 4), 0x0000_0004_ffff_ffff);
        assert_eq!(pair(-1, -1), NO_SKILL);
    }

    #[test]
    fn pretty_names() {
        assert_eq!(pretty("skill_fire_spark"), "Fire spark");
        assert_eq!(pretty("quickshot"), "Quickshot");
        assert_eq!(pretty(""), "");
    }
}
