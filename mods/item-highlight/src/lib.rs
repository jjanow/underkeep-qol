//! `item-highlight`: makes pickable and usable things in the dungeon view
//! glow (README, "Mod designs").
//!
//! # How the game draws them
//! The dungeon view is 2D: every entity (item, alcove, chest, lever,
//! monster, decoration) is a sprite. Each entity's draw function calls a
//! helper that fills a sprite record and queues it with `QueueWorldSprite`
//! (pattern `sprite_submit`), passing the entity's pick id. The queued
//! records are drawn later in one batch with the game's `DungObject` shader.
//! A record carries a color (four floats at +0x18, always white from the
//! helper) and a highlight amount (+0x3c), which the game sets to 1 only on
//! the item under the cursor or picked with the gamepad.
//!
//! # What the mod changes
//! - The hook on `QueueWorldSprite` looks the pick id up in the game's pick
//!   map (pattern `pick_lookup`), takes the entity from the record (+0x60)
//!   and its class from the entity's vtable. The vtables of `Item`,
//!   `Container`, `Chest`, `Button` (levers and buttons) and `Decoration`
//!   are found once at load from their RTTI names ([`rtti`]), so no pattern
//!   per class is needed. For the groups the player turned on, it
//!   multiplies the record's color by the highlight color and sets the
//!   highlight amount to the "glow" setting. Records the game already
//!   highlights (the hovered item) are left alone, so they keep the game's
//!   own look.
//! - A `Decoration` is any other world object (trees, signs, barrels,
//!   torches...). Its group comes from its live state, read the way the game
//!   reads it, because loot and scripts are set per placed object in the
//!   level, not per definition ([`decoration_group`]):
//!   - keyholes (`*wall_lock*` definitions) are switches;
//!   - an object the game's interaction picker accepts (not disabled,
//!     interaction not disabled, a non-empty interact area) whose interact
//!     priority is above 50 is usable. The game's scripts set the priority
//!     to 0 on objects with nothing to do and lower it to 50 (or 0) after
//!     the first use, so used-up things stop glowing. Torches and braziers
//!     (`*torch*`, `*brazier*`) only switch a light on and off and have
//!     their own group;
//!   - an object with hit points left and still in its first sprite state
//!     (barrels, crates, urns, webs, furniture) is breakable;
//!   - a usable object also needs its first sprite state: a dug grave keeps
//!     its priority but shows the empty grave. Lights are exempt (campfires
//!     sit in state 1, their sprite is only for the editor).
//! - Each group has its own color. The effects mounted on a marked light
//!   (flames, glows; floor braziers and campfires are only effects) take
//!   its color too ([`effects`]), and so do the see-through item copies of
//!   the game's "Show items behind objects" option ([`behind`]).
//! - The shader's highlight adds white. To make it add the record's color
//!   instead, the mod wraps `glShaderSource` (through the exe's import of
//!   `SDL_GL_GetProcAddress`) and changes that one term of the `DungObject`
//!   fragment shader as it is compiled. With the game's white color the
//!   result is the same as before, so nothing else changes look. The game
//!   compiles its shaders at startup: when the mod was off then, the
//!   highlight falls back to a tint with a weaker, white glow.
//!
//! Only what the game draws is marked: things hidden by walls or fog stay
//! hidden. Nothing in the game's state or files changes.

mod rtti;

use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicUsize, Ordering};

use uk_qol_runtime::host::{LoadCtx, Mod};
use uk_qol_runtime::input::InputEvent;
use uk_qol_runtime::{guard, hotkey, toast};

const ID: &str = "item-highlight";
const KEYS: [&str; 2] = ["toggle_key", "toggle_pad"];

// Sprite record (`SpriteDraw`, 1.0.5).
const D_SPRITE: usize = 0x00; // the sprite definition
const D_COLOR: usize = 0x18; // 4 x f32 RGBA
const D_HIGHLIGHT: usize = 0x3c; // f32
/// How additive the sprite is drawn (f32, 0 = normal blending), with the
/// sprite definition's own amount ([`SPRITE_ADDITIVE`]).
const D_ADDITIVE: usize = 0x28;
/// Flags of an item's second sprite (drawn under it, never highlighted by
/// the game either).
const FLAGS_UNDERLAY: u16 = 0x2;

/// The sprite definition's own color (3 x f32 RGB): the game multiplies it
/// with the record's color for the vertex color (it tints shared grey
/// sprites, for example a red robe).
const SPRITE_COLOR: usize = 0x14;
/// The sprite definition's additive amount (f32): the game blends a sprite
/// by `1 - (1 - record's) * (1 - this)` additively (flames, light halos).
const SPRITE_ADDITIVE: usize = 0x24;

// Pick record (the entity's common block).
const REC_ENTITY: usize = 0x60; // the entity object
const REC_DEF_NAME: usize = 0x80; // char* of the definition's name

// Entities.
// Interaction state, common to every entity class (the game's
// can-interact check, Entity vtable slot 9, reads these).
const ENT_DISABLED: usize = 0x18; // u8
const ENT_INTERACT_DISABLED: usize = 0x1c; // u8
const ENT_INTERACT_PRIORITY: usize = 0x48; // i32
const ENT_INTERACT_AREA_W: usize = 0xb8; // f32 (area x, y, w, h at 0xb0)
const ENT_INTERACT_AREA_H: usize = 0xbc; // f32
// Decoration.
const DECO_HP: usize = 0xd0; // i32: def `health`, 0 if none, < 0 once broken
const DECO_SPRITE_STATE: usize = 0xd4; // i32, 0 = as placed
const CONTAINER_ITEM_COUNT: usize = 0xd8; // i32
const CHEST_SPRITE_STATE: usize = 0xd0; // i32, 0 = closed
const CHEST_CLOSED: i32 = 0;

// Category bits.
const CAT_ITEMS: u8 = 1;
const CAT_CONTAINERS: u8 = 2;
const CAT_SWITCHES: u8 = 4;
const CAT_OBJECTS: u8 = 8;
const CAT_BREAKABLES: u8 = 16;
const CAT_LIGHTS: u8 = 32;
/// Number of groups (one bit each, in this order).
const N_CATS: usize = 6;
/// Interact priority the game's scripts give an object after its first use
/// when it still has a "done" text; 0 when there's nothing (left) to do.
/// Unused objects are at 99 and up.
const PRIORITY_USED: i32 = 50;

/// Each group's color setting and default, in bit order.
const COLORS: [(&str, &str); N_CATS] = [
    ("color", "#33FF33"),
    ("color_containers", "#FFC020"),
    ("color_switches", "#33CCFF"),
    ("color_objects", "#C070FF"),
    ("color_breakables", "#FF5050"),
    ("color_lights", "#FFFF80"),
];

/// Classes the mod marks, by RTTI name.
const CLASSES: [(&str, Class); 5] = [
    (".?AUItem@@", Class::Item),
    (".?AUContainer@@", Class::Container),
    (".?AUChest@@", Class::Chest),
    (".?AUButton@@", Class::Button),
    (".?AUDecoration@@", Class::Decoration),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    Item,
    Container,
    Chest,
    Button,
    /// Keyholes, usable objects, lights and breakables
    /// ([`decoration_group`]).
    Decoration,
}

static ORIG_SUBMIT: AtomicUsize = AtomicUsize::new(0);
static LOOKUP: AtomicUsize = AtomicUsize::new(0);
/// Vtable addresses, in [`CLASSES`] order (0 = not found).
static VTABLES: [AtomicUsize; 5] = [const { AtomicUsize::new(0) }; 5];
/// Highlight shown (mod active and the `shown` setting on).
static ON: AtomicBool = AtomicBool::new(false);
static CATEGORIES: AtomicU8 = AtomicU8::new(0);
/// Each group's highlight color (index = bit number), as f32 bits of
/// r, g, b (0..1, sRGB as in the setting).
static TINTS: [[AtomicU32; 3]; N_CATS] = [const { [const { AtomicU32::new(0) }; 3] }; N_CATS];
static STRENGTH: AtomicU32 = AtomicU32::new(0);
/// `debug` setting: log each kind of thing the first time it's marked.
static DEBUG: AtomicBool = AtomicBool::new(false);
static LOGGED: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

type SubmitFn = unsafe extern "system" fn(usize, u16, usize, usize, usize, i32, *mut u8);
type LookupFn = unsafe extern "system" fn(u32) -> usize;

fn log(msg: &str) {
    uk_qol_runtime::log(&format!("{ID}: {msg}"));
}

fn f32_of(a: &AtomicU32) -> f32 {
    f32::from_bits(a.load(Ordering::Relaxed))
}

/// The definition name in pick record `rec`, read with checks (for the
/// debug log only).
fn def_name_checked(rec: usize) -> String {
    use uk_qol_runtime::game::readable;
    if !readable(rec + REC_DEF_NAME, 8) {
        return "?".into();
    }
    // SAFETY: checked readable.
    let p = unsafe { *((rec + REC_DEF_NAME) as *const usize) };
    if p == 0 || !readable(p, 64) {
        return "?".into();
    }
    // SAFETY: checked readable, 64 bytes.
    let b = unsafe { std::slice::from_raw_parts(p as *const u8, 64) };
    let end = b.iter().position(|&c| c == 0).unwrap_or(64);
    String::from_utf8_lossy(&b[..end]).into_owned()
}

/// The definition name of pick record `rec` (empty if it has none).
///
/// # Safety
/// `rec` must be a live pick record.
unsafe fn def_name<'a>(rec: usize) -> &'a [u8] {
    // SAFETY: the record's name pointer is the definition's (static) name.
    let name = unsafe { *((rec + REC_DEF_NAME) as *const *const std::ffi::c_char) };
    if name.is_null() {
        return &[];
    }
    // SAFETY: a NUL-terminated name kept for the life of the definitions.
    unsafe { std::ffi::CStr::from_ptr(name) }.to_bytes()
}

fn contains(name: &[u8], part: &[u8]) -> bool {
    name.windows(part.len()).any(|w| w == part)
}

/// What the mod reads from a `Decoration` to pick its group.
#[derive(Debug, Clone, Copy, Default)]
struct DecoState {
    /// A keyhole (`wall_lock_iron`, `mine_wall_lock`...; not `block`).
    lock: bool,
    /// A torch, brazier or campfire: interacting only switches its light.
    light: bool,
    /// A light that can't be switched (`brazier1_non_switchable`...): it
    /// only says so when clicked.
    fixed: bool,
    /// An apple tree (`tree2`): it has apples left while its sprite
    /// state is 0, whatever its priority (see [`decoration_group`]).
    apple_tree: bool,
    /// The game's can-interact check passes.
    can_interact: bool,
    priority: i32,
    hp: i32,
    sprite_state: i32,
}

impl DecoState {
    /// Reads the decoration `ent` with pick record `rec`.
    ///
    /// # Safety
    /// `ent` and `rec` must be a live `Decoration` and its pick record.
    unsafe fn read(ent: usize, rec: usize) -> DecoState {
        // SAFETY (closures): fields of the caller's live Decoration.
        let b = |o: usize| unsafe { *((ent + o) as *const u8) };
        let i = |o: usize| unsafe { *((ent + o) as *const i32) };
        let f = |o: usize| unsafe { *((ent + o) as *const f32) };
        // SAFETY: the caller's record.
        let name = unsafe { def_name(rec) };
        DecoState {
            lock: contains(name, b"wall_lock"),
            light: [b"torch".as_slice(), b"brazier", b"campfire"]
                .iter()
                .any(|p| contains(name, p)),
            fixed: contains(name, b"non_switchable") || contains(name, b"nonswitchable"),
            apple_tree: name == b"tree2",
            // The game's check (Entity vtable slot 9): not disabled, a
            // non-empty interact area, interaction not disabled.
            can_interact: b(ENT_DISABLED) == 0
                && f(ENT_INTERACT_AREA_W) > 0.0
                && f(ENT_INTERACT_AREA_H) > 0.0
                && b(ENT_INTERACT_DISABLED) == 0,
            priority: i(ENT_INTERACT_PRIORITY),
            hp: i(DECO_HP),
            sprite_state: i(DECO_SPRITE_STATE),
        }
    }
}

/// The group of a decoration in state `s`, if any. `cats` are the groups
/// shown: a keyhole is never anything but a switch.
fn decoration_group(s: &DecoState, cats: u8) -> Option<u8> {
    let cat = if s.lock {
        CAT_SWITCHES
    } else if s.hp > 0 && s.sprite_state == 0 {
        CAT_BREAKABLES
    } else if s.apple_tree {
        // The tree rolls its apples again each time the level is set up,
        // and a roll of none sets priority 0, which a save with apples
        // left doesn't undo: such a tree still drops apples. Its sprite
        // (apples or bare) is always right.
        if s.can_interact && s.sprite_state == 0 {
            CAT_OBJECTS
        } else {
            return None;
        }
    } else if s.can_interact && s.priority > PRIORITY_USED && s.light {
        if s.fixed {
            return None;
        }
        // Lights keep their sprite state (campfires draw only effects and
        // sit in state 1 from the start).
        CAT_LIGHTS
    } else if s.can_interact && s.priority > PRIORITY_USED && s.sprite_state == 0 {
        // A changed sprite means used: a dug grave keeps its priority but
        // shows the empty grave.
        CAT_OBJECTS
    } else {
        return None;
    };
    (cats & cat != 0).then_some(cat)
}

/// The category to mark a sprite of entity `ent` (pick record `rec`) as,
/// if any.
///
/// # Safety
/// `ent` and `rec` must be the live entity and its pick record.
unsafe fn category(ent: usize, rec: usize) -> Option<u8> {
    // SAFETY: an entity object starts with its vtable.
    let vt = unsafe { *(ent as *const usize) };
    let i = VTABLES
        .iter()
        .position(|v| v.load(Ordering::Relaxed) == vt)?;
    // SAFETY (reads below): fields of the entity of the matched class.
    match CLASSES[i].1 {
        Class::Item => Some(CAT_ITEMS),
        Class::Container => {
            (unsafe { *((ent + CONTAINER_ITEM_COUNT) as *const i32) } > 0).then_some(CAT_CONTAINERS)
        }
        Class::Chest => (unsafe { *((ent + CHEST_SPRITE_STATE) as *const i32) } == CHEST_CLOSED)
            .then_some(CAT_CONTAINERS),
        Class::Button => Some(CAT_SWITCHES),
        Class::Decoration => {
            let cats = CATEGORIES.load(Ordering::Relaxed);
            const DECO_CATS: u8 = CAT_SWITCHES | CAT_OBJECTS | CAT_BREAKABLES | CAT_LIGHTS;
            if cats & DECO_CATS == 0 {
                return None;
            }
            // SAFETY: a live Decoration and its record.
            decoration_group(&unsafe { DecoState::read(ent, rec) }, cats)
        }
    }
}

/// Recolors sprite record `d` of the entity with `pick` id, if it's one to
/// mark.
///
/// # Safety
/// The game's arguments to `QueueWorldSprite`, on the game thread.
unsafe fn mark(flags: u16, pick: i32, d: *mut u8) {
    if !ON.load(Ordering::Relaxed) || pick == 0 || flags == FLAGS_UNDERLAY {
        return;
    }
    let f = |o: usize| unsafe { (d.add(o) as *mut f32).read_unaligned() };
    if f(D_HIGHLIGHT) > 0.0 {
        return;
    }
    // SAFETY: resolved function of this build: (u32) -> record or null.
    let lookup: LookupFn = unsafe { std::mem::transmute(LOOKUP.load(Ordering::Relaxed)) };
    let rec = unsafe { lookup(pick as u32) };
    if rec == 0 {
        return;
    }
    // SAFETY: a live pick record; its entity pointer.
    let ent = unsafe { *((rec + REC_ENTITY) as *const usize) };
    if ent == 0 {
        return;
    }
    if DEBUG.load(Ordering::Relaxed) {
        // SAFETY: the entity starts with its vtable.
        let vt = unsafe { *(ent as *const usize) };
        let i = VTABLES.iter().position(|v| v.load(Ordering::Relaxed) == vt);
        let class = i.map_or("other", |i| CLASSES[i].0);
        let mut line = format!("{class} {} (flags {flags:#x})", def_name_checked(rec));
        if i.is_some_and(|i| CLASSES[i].1 == Class::Decoration) {
            // SAFETY: a live Decoration and its record.
            let s = unsafe { DecoState::read(ent, rec) };
            line += &format!(
                " interact {} priority {} hp {} sprite state {}",
                s.can_interact, s.priority, s.hp, s.sprite_state
            );
        }
        let mut seen = LOGGED.lock().unwrap_or_else(|e| e.into_inner());
        if !seen.contains(&line) {
            log(&format!("drawn: {line}"));
            seen.push(line);
        }
    }
    let Some(cat) = (unsafe { category(ent, rec) }) else {
        return;
    };
    if CATEGORIES.load(Ordering::Relaxed) & cat == 0 {
        return;
    }
    // SAFETY: the caller's live record.
    unsafe { recolor(d, cat) };
}

/// The color of group `cat` (one bit), r, g, b in 0..1.
fn tint(cat: u8) -> [f32; 3] {
    let t = &TINTS[(cat.trailing_zeros() as usize).min(N_CATS - 1)];
    [f32_of(&t[0]), f32_of(&t[1]), f32_of(&t[2])]
}

/// Gives sprite record `d` (`SpriteDraw` layout) the look of group `cat`.
///
/// # Safety
/// `d` must be a writable sprite record about to be drawn, with a valid
/// (or null) sprite definition pointer.
unsafe fn recolor(d: *mut u8, cat: u8) {
    let strength = f32_of(&STRENGTH);
    // SAFETY: the record's sprite definition (set by the game's helper).
    let sprite = unsafe { (d.add(D_SPRITE) as *const usize).read_unaligned() };
    // SAFETY: the record's and the sprite definition's floats.
    let additive = 1.0
        - (1.0 - unsafe { (d.add(D_ADDITIVE) as *const f32).read_unaligned() })
            * (1.0
                - if sprite == 0 {
                    0.0
                } else {
                    unsafe { ((sprite + SPRITE_ADDITIVE) as *const f32).read_unaligned() }
                });
    if additive > 0.01 {
        // Additive light (flames, halos): recoloring to a bright color
        // would glare, so only shift its hue toward the color.
        for (i, t) in tint(cat).into_iter().enumerate() {
            let o = D_COLOR + i * 4;
            let c = 1.0 + (t - 1.0) * strength;
            // SAFETY: the record's color floats.
            unsafe {
                let p = d.add(o) as *mut f32;
                p.write_unaligned(p.read_unaligned() * c);
            }
        }
        return;
    }
    // With the changed shader, 2 + strength selects the recoloring (the
    // game only uses 0..1); without it, a plain tint and a white glow.
    let highlight = if shader::patched() {
        shader::MARK + strength
    } else {
        strength * 0.5
    };
    for (i, t) in tint(cat).into_iter().enumerate() {
        let o = D_COLOR + i * 4;
        let color = if shader::patched() && sprite != 0 {
            // The recoloring wants exactly the highlight color: undo the
            // sprite's own color, which the game multiplies in.
            // SAFETY: the sprite definition's color floats.
            let own = unsafe { ((sprite + SPRITE_COLOR + i * 4) as *const f32).read_unaligned() };
            t / own.max(0.05)
        } else {
            // SAFETY: the record's color floats.
            t * unsafe { (d.add(o) as *const f32).read_unaligned() }
        };
        // SAFETY: the record's color floats (alpha at +0x24 is kept).
        unsafe { (d.add(o) as *mut f32).write_unaligned(color) };
    }
    // SAFETY: the record's highlight float.
    unsafe { (d.add(D_HIGHLIGHT) as *mut f32).write_unaligned(highlight) };
}

unsafe extern "system" fn submit(
    layer: usize,
    flags: u16,
    pos: usize,
    rect: usize,
    cell: usize,
    pick: i32,
    d: *mut u8,
) {
    // SAFETY: set before the hook was enabled.
    let orig: SubmitFn = unsafe { std::mem::transmute(ORIG_SUBMIT.load(Ordering::Relaxed)) };
    // Only the recoloring runs in the guard: if it panics, the sprite is
    // queued as the game made it.
    // SAFETY: the game's arguments.
    let _ = guard::call(ID, || unsafe { mark(flags, pick, d) });
    // SAFETY: the original with the game's arguments.
    unsafe { orig(layer, flags, pos, rect, cell, pick, d) }
}

/// Effects of lights (campfire, brazier and torch flames, glows): the
/// game draws a `Decoration`'s effects in its own pass (`Decoration` vtable
/// slot 7), one sprite record per particle, built by the particle quad
/// function (pattern `effect_quad`). The mod swaps the vtable slot for
/// [`deco_effects`], which notes the decoration's group while the game
/// draws its effects, and [`effect_quad`] recolors those particles.
// The slot swap needs the exe's memory protection calls (Windows only).
#[cfg_attr(not(windows), allow(dead_code))]
mod effects {
    use super::*;

    /// `Decoration` vtable slot 7: draws the effects mounted on it.
    pub const SLOT: usize = 7;
    /// The real slot-7 function, while swapped.
    pub static ORIG_SLOT: AtomicUsize = AtomicUsize::new(0);
    pub static ORIG_QUAD: AtomicUsize = AtomicUsize::new(0);
    /// Group of the decoration whose effects are being drawn (0 = none).
    static CURRENT: AtomicU8 = AtomicU8::new(0);

    // Entity: the common block (+8) holds the pick id at +0x10.
    const ENT_COMMON: usize = 0x08;
    const COMMON_PICK: usize = 0x10;

    pub type SlotFn = unsafe extern "system" fn(usize, usize, usize, usize);
    pub type QuadFn = unsafe extern "system" fn(usize, *mut *mut u8, usize) -> usize;

    /// The group to draw decoration `ent`'s effects as, if any.
    ///
    /// # Safety
    /// `ent` must be a live `Decoration`.
    unsafe fn group(ent: usize) -> Option<u8> {
        if !ON.load(Ordering::Relaxed) {
            return None;
        }
        // SAFETY: a live entity's common block and its pick id.
        let pick =
            unsafe { *((*((ent + ENT_COMMON) as *const usize) + COMMON_PICK) as *const u32) };
        if pick == 0 {
            return None;
        }
        // SAFETY: resolved function of this build: (u32) -> record or null.
        let lookup: LookupFn = unsafe { std::mem::transmute(LOOKUP.load(Ordering::Relaxed)) };
        let rec = unsafe { lookup(pick) };
        // SAFETY: a live pick record; only this entity's own record counts.
        if rec == 0 || unsafe { *((rec + REC_ENTITY) as *const usize) } != ent {
            return None;
        }
        // SAFETY: the live entity and its record.
        let cat = unsafe { category(ent, rec) }?;
        if DEBUG.load(Ordering::Relaxed) {
            let line = format!("effects of {} ({cat:#x})", def_name_checked(rec));
            let mut seen = LOGGED.lock().unwrap_or_else(|e| e.into_inner());
            if !seen.contains(&line) {
                log(&format!("drawn: {line}"));
                seen.push(line);
            }
        }
        // Only lights: their flames are the part to see (floor braziers
        // and campfires have no sprite of their own). Other scenery's
        // effects (fog, light rays, steam) stay as the game draws them.
        (cat == CAT_LIGHTS && CATEGORIES.load(Ordering::Relaxed) & cat != 0).then_some(cat)
    }

    /// Replacement for `Decoration` vtable slot 7 (`this`, render context,
    /// flags, placement).
    pub unsafe extern "system" fn deco_effects(
        this: usize,
        ctx: usize,
        flags: usize,
        place: usize,
    ) {
        // SAFETY: set before the slot was swapped.
        let orig: SlotFn = unsafe { std::mem::transmute(ORIG_SLOT.load(Ordering::Relaxed)) };
        // SAFETY: the game calls this slot on a live Decoration.
        let cat = guard::call(ID, || unsafe { group(this) })
            .flatten()
            .unwrap_or(0);
        CURRENT.store(cat, Ordering::Relaxed);
        // SAFETY: the original with the game's arguments.
        unsafe { orig(this, ctx, flags, place) };
        CURRENT.store(0, Ordering::Relaxed);
    }

    /// Detour of the particle quad function (render context, sprite record,
    /// quad corners): recolors the record while a marked decoration's
    /// effects are drawn.
    pub unsafe extern "system" fn effect_quad(ctx: usize, d: *mut *mut u8, quad: usize) -> usize {
        // SAFETY: set before the hook was enabled.
        let orig: QuadFn = unsafe { std::mem::transmute(ORIG_QUAD.load(Ordering::Relaxed)) };
        let cat = CURRENT.load(Ordering::Relaxed);
        if cat != 0 && !d.is_null() {
            // SAFETY: the game's record on its stack, `SpriteDraw` layout.
            let _ = guard::call(ID, || unsafe { recolor(d as *mut u8, cat) });
        }
        // SAFETY: the original with the game's arguments.
        unsafe { orig(ctx, d, quad) }
    }
}

/// "Show items behind objects": with the game's option on, its overlay pass
/// (pattern `behind_overlay`) draws every item in view again, on top of
/// everything at half opacity, through the GUI sprite batch rather than the
/// world sprites. Those copies are plain white-tinted, so the mod gives them
/// the item color after the pass ([`overlay`]).
mod behind {
    use super::*;

    pub static ORIG: AtomicUsize = AtomicUsize::new(0);
    static LOGGED_COPY: AtomicBool = AtomicBool::new(false);
    /// The item color for the copies, linear r, g, b (the GUI batch takes
    /// linear colors), already mixed with white by the strength.
    pub static COLOR: [AtomicU32; 3] = [const { AtomicU32::new(0) }; 3];

    // GUI batch: +8 data, +0x14 bytes used; commands are [u32 kind]
    // [u32 size (with this header)] then the fields.
    const BATCH_DATA: usize = 0x08;
    const BATCH_USED: usize = 0x14;
    /// Sprite command: sprite +0x10, rect +0x18, color RGBA +0x30, glow +0x40.
    const CMD_SPRITE: u32 = 10;
    const CMD_SPRITE_SIZE: u32 = 0x48;
    const CMD_COLOR: usize = 0x30;
    const CMD_GLOW: usize = 0x40;

    pub type OverlayFn = unsafe extern "system" fn(usize);

    /// sRGB channel (0..1) to linear, as the game converts its colors.
    pub fn linear(c: f32) -> f32 {
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    }

    /// Whether a sprite command's color and glow are the item copies'
    /// (white, half opacity, no glow); the pass's other commands (the
    /// screen fade, the hovered item's) differ.
    pub fn is_copy(rgba: [f32; 4], glow: f32) -> bool {
        rgba == [1.0, 1.0, 1.0, 0.5] && glow == 0.0
    }

    /// Recolors the item copies the pass added to `batch` after byte
    /// `from`.
    ///
    /// # Safety
    /// `batch` must be the game's GUI batch the pass just wrote to.
    unsafe fn recolor_copies(batch: usize, from: usize) {
        // SAFETY: the batch header; the buffer may have moved in the pass.
        let (data, used) = unsafe {
            (
                *((batch + BATCH_DATA) as *const usize),
                *((batch + BATCH_USED) as *const i32),
            )
        };
        if data == 0 || used <= 0 {
            return;
        }

        let color = [0, 1, 2].map(|i| f32_of(&COLOR[i]));
        let mut at = from;
        while at + 8 <= used as usize {
            // SAFETY (reads/writes below): inside the batch's used bytes.
            let cmd = data + at;
            let (kind, size) = unsafe { (*(cmd as *const u32), *((cmd + 4) as *const u32)) };
            if size < 8 || at + size as usize > used as usize {
                return;
            }

            if kind == CMD_SPRITE && size == CMD_SPRITE_SIZE {
                let c = cmd + CMD_COLOR;
                let rgba = [0, 1, 2, 3].map(|i| unsafe { *((c + i * 4) as *const f32) });
                let glow = unsafe { *((cmd + CMD_GLOW) as *const f32) };
                if is_copy(rgba, glow) {
                    if DEBUG.load(Ordering::Relaxed) && !LOGGED_COPY.swap(true, Ordering::Relaxed) {
                        log("drawn: see-through item copies (Show items behind objects)");
                    }
                    for (i, v) in color.iter().enumerate() {
                        unsafe { *((c + i * 4) as *mut f32) = *v };
                    }
                }
            }
            at += size as usize;
        }
    }

    /// Detour of the overlay pass (its one argument is the GUI batch).
    pub unsafe extern "system" fn overlay(batch: usize) {
        // SAFETY: set before the hook was enabled.
        let orig: OverlayFn = unsafe { std::mem::transmute(ORIG.load(Ordering::Relaxed)) };
        let on = ON.load(Ordering::Relaxed) && CATEGORIES.load(Ordering::Relaxed) & CAT_ITEMS != 0;
        // SAFETY: the game's batch header.
        let from = if on {
            unsafe { *((batch + BATCH_USED) as *const i32) }
        } else {
            -1
        };
        // SAFETY: the original with the game's argument.
        unsafe { orig(batch) };
        if from >= 0 {
            // SAFETY: the batch the pass wrote to.
            let _ = guard::call(ID, || unsafe { recolor_copies(batch, from as usize) });
        }
    }
}

/// The `DungObject` shader's highlight term (see the module docs).
// Installed through the exe's import table, which only exists on Windows.
#[cfg_attr(not(windows), allow(dead_code))]
mod shader {
    use std::ffi::{CStr, c_char};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    /// The game's highlight statement in the `DungObject` fragment shader
    /// (adds white by the highlight amount, 0..1). Kept as it is for every
    /// sprite the mod doesn't mark.
    const HIGHLIGHT: &str = "result = result * highlight * 0.5 + mix( result, \
                             vec4( result.a, result.a, result.a, result.a ), highlight *0.5 ) ;";
    /// Highlight amounts from this up are the mod's: `MARK + strength`.
    pub const MARK: f32 = 2.0;
    /// For the mod's sprites: the game's result, recolored toward the
    /// vertex color (the highlight color, premultiplied like the game's) by
    /// `strength` times the texture's alpha, with the texture's light and
    /// dark parts; alpha is kept. Weighting by alpha leaves additive glows
    /// (light halos around flames, nearly transparent) as the game draws
    /// them instead of turning them into solid colored balls.
    const RECOLOR: &str = "if ( highlight >= 2.0 ) { \
                           vec4 qol_t = texture( t_color, uv ) ; \
                           float qol_l = dot( qol_t.rgb, vec3( 0.299, 0.587, 0.114 ) ) ; \
                           result = vec4( mix( result.rgb, color.rgb * ( 0.6 * qol_t.a + 0.7 * qol_l ), \
                           min( highlight - 2.0, 1.0 ) * qol_t.a ), result.a ) ; \
                           } else { ";

    pub static ORIG_GET_PROC: AtomicUsize = AtomicUsize::new(0);
    static ORIG_SOURCE: AtomicUsize = AtomicUsize::new(0);
    static PATCHED: AtomicBool = AtomicBool::new(false);

    pub type GetProcFn = unsafe extern "C" fn(*const c_char) -> usize;
    type SourceFn = unsafe extern "system" fn(u32, i32, *const *const u8, *const i32);

    /// Whether the game's shader was compiled with the colored highlight
    /// (this session; a compiled shader stays as it is).
    pub fn patched() -> bool {
        PATCHED.load(Ordering::Relaxed)
    }

    /// The changed source, if `src` is the shader to change.
    pub fn rewrite(src: &str) -> Option<String> {
        (src.matches(HIGHLIGHT).count() == 1)
            .then(|| src.replace(HIGHLIGHT, &format!("{RECOLOR}{HIGHLIGHT} }}")))
    }

    /// `glShaderSource` wrapper: passes the sources on, with the highlight
    /// term changed in the one shader that has it.
    unsafe extern "system" fn shader_source(
        shader: u32,
        count: i32,
        strings: *const *const u8,
        lengths: *const i32,
    ) {
        // SAFETY: set before this wrapper was handed out.
        let orig: SourceFn = unsafe { std::mem::transmute(ORIG_SOURCE.load(Ordering::Relaxed)) };
        let changed = uk_qol_runtime::guard::call(super::ID, || {
            let parts: Vec<Vec<u8>> = (0..count.max(0) as usize)
                .map(|i| {
                    // SAFETY: GL's contract: `count` strings, each with its
                    // length in `lengths` or NUL-terminated (null array or
                    // negative length).
                    unsafe {
                        let p = *strings.add(i);
                        let len = if lengths.is_null() || *lengths.add(i) < 0 {
                            CStr::from_ptr(p as *const c_char).to_bytes().len()
                        } else {
                            *lengths.add(i) as usize
                        };
                        std::slice::from_raw_parts(p, len).to_vec()
                    }
                })
                .collect();
            let mut hit = false;
            let parts: Vec<Vec<u8>> = parts
                .into_iter()
                .map(|b| match std::str::from_utf8(&b).ok().and_then(rewrite) {
                    Some(s) => {
                        hit = true;
                        s.into_bytes()
                    }
                    None => b,
                })
                .collect();
            hit.then_some(parts)
        })
        .flatten();
        let Some(parts) = changed else {
            // SAFETY: the original with the game's arguments.
            return unsafe { orig(shader, count, strings, lengths) };
        };
        PATCHED.store(true, Ordering::Relaxed);
        super::log("shader: highlight color added to the dungeon sprite shader");
        let ptrs: Vec<*const u8> = parts.iter().map(|b| b.as_ptr()).collect();
        let lens: Vec<i32> = parts.iter().map(|b| b.len() as i32).collect();
        // SAFETY: same count; GL copies the sources before returning.
        unsafe { orig(shader, count, ptrs.as_ptr(), lens.as_ptr()) }
    }

    /// `SDL_GL_GetProcAddress` detour: hands out the wrapper for
    /// `glShaderSource`.
    pub unsafe extern "C" fn get_proc(name: *const c_char) -> usize {
        // SAFETY: set before the import was redirected.
        let orig: GetProcFn = unsafe { std::mem::transmute(ORIG_GET_PROC.load(Ordering::Relaxed)) };
        // SAFETY: the caller's argument.
        let f = unsafe { orig(name) };
        // SAFETY: SDL takes a NUL-terminated name.
        if f != 0
            && !name.is_null()
            && unsafe { CStr::from_ptr(name) }.to_bytes() == b"glShaderSource"
        {
            ORIG_SOURCE.store(f, Ordering::Relaxed);
            return shader_source as SourceFn as usize;
        }
        f
    }
}

#[derive(Default)]
pub struct ItemHighlight {
    shown: bool,
    /// `Decoration` vtable slot 7, once swapped (see [`effects`]).
    #[cfg_attr(not(windows), allow(dead_code))]
    effects_slot: Option<usize>,
    #[cfg(windows)]
    get_proc: Option<uk_qol_runtime::iat::IatHook>,
}

impl ItemHighlight {
    fn apply(&mut self, settings: &toml::Table) {
        let b = |k: &str, d: bool| settings.get(k).and_then(|v| v.as_bool()).unwrap_or(d);
        self.shown = b("shown", true);
        ON.store(self.shown, Ordering::Relaxed);
        let mut cats = 0;
        for (k, c, d) in [
            ("items", CAT_ITEMS, true),
            ("containers", CAT_CONTAINERS, true),
            ("switches", CAT_SWITCHES, true),
            ("objects", CAT_OBJECTS, true),
            ("breakables", CAT_BREAKABLES, true),
            ("lights", CAT_LIGHTS, false),
        ] {
            if b(k, d) {
                cats |= c;
            }
        }
        CATEGORIES.store(cats, Ordering::Relaxed);
        for (tint, (key, default)) in TINTS.iter().zip(COLORS) {
            let rgb = settings
                .get(key)
                .and_then(|v| v.as_str())
                .and_then(uk_qol_runtime::host::parse_color)
                .or_else(|| uk_qol_runtime::host::parse_color(default))
                .unwrap_or([0xff; 3]);
            for (a, c) in tint.iter().zip(rgb) {
                a.store((c as f32 / 255.0).to_bits(), Ordering::Relaxed);
            }
        }
        let strength = (settings
            .get("strength")
            .and_then(|v| v.as_float())
            .unwrap_or(0.85) as f32)
            .clamp(0.0, 1.0);
        STRENGTH.store(strength.to_bits(), Ordering::Relaxed);
        // The item copies of "Show items behind objects": white mixed
        // toward the item color by the strength, in linear light.
        for (a, t) in behind::COLOR.iter().zip(tint(CAT_ITEMS)) {
            let c = 1.0 + (behind::linear(t) - 1.0) * strength;
            a.store(c.to_bits(), Ordering::Relaxed);
        }
        DEBUG.store(
            settings
                .get("debug")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            Ordering::Relaxed,
        );
    }
}

impl ItemHighlight {
    /// Hooks effect drawing (see [`effects`]): the particle quad function
    /// at `quad`, then `Decoration` vtable slot 7.
    /// The detour goes in on every load (the host removes it on unload);
    /// the slot is swapped once and stays (the replacement only passes the
    /// call on while the mod is off).
    fn hook_effects(&mut self, cx: &mut LoadCtx, quad: Option<usize>) {
        let deco = CLASSES.iter().position(|c| c.1 == Class::Decoration);
        let vt = deco.map_or(0, |i| VTABLES[i].load(Ordering::Relaxed));
        let (Some(quad), true) = (quad, vt != 0) else {
            log(
                "object effects (campfires, braziers) not colored: pattern effect_quad or Decoration not found",
            );
            return;
        };
        // SAFETY: the start of the particle quad function (verified
        // pattern); the detour has its signature and ABI.
        let hooked = unsafe {
            cx.hooks
                .detour(quad, effects::effect_quad as effects::QuadFn as usize)
        };
        let Ok(t) = hooked.map_err(|err| log(&format!("object effects not colored: {err}"))) else {
            return;
        };
        effects::ORIG_QUAD.store(t, Ordering::Relaxed);
        #[cfg(windows)]
        if self.effects_slot.is_none() {
            let slot = vt + effects::SLOT * 8;
            // SAFETY: a vtable entry in the exe's read-only data.
            let orig = unsafe { *(slot as *const usize) };
            if !(cx.base..cx.base + cx.image.bytes.len()).contains(&orig) {
                log(&format!(
                    "object effects not colored: Decoration slot {} is {orig:#x}",
                    effects::SLOT
                ));
                return;
            }
            effects::ORIG_SLOT.store(orig, Ordering::Relaxed);
            // SAFETY: the slot holds the game's function with the
            // replacement's signature; ORIG_SLOT is set first.
            match unsafe {
                uk_qol_runtime::iat::swap_pointer(
                    slot,
                    orig,
                    effects::deco_effects as effects::SlotFn as usize,
                )
            } {
                Ok(()) => self.effects_slot = Some(slot),
                Err(err) => log(&format!("object effects not colored: {err}")),
            }
        }
    }
}

impl Mod for ItemHighlight {
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
        let target = at("sprite_submit")?;
        LOOKUP.store(at("pick_lookup")?, Ordering::Relaxed);

        let sections: Vec<(u32, u32)> = cx
            .image
            .headers
            .sections
            .iter()
            .filter(|s| s.name == ".rdata" || s.name == ".data")
            .map(|s| (s.rva, s.virtual_size))
            .collect();
        let bases = [cx.base as u64, cx.image.headers.image_base];
        let mut missing = Vec::new();
        for ((name, _), slot) in CLASSES.iter().zip(&VTABLES) {
            let vt = rtti::find_vtable(cx.image.bytes, &sections, &bases, name);
            slot.store(vt.map_or(0, |r| cx.base + r as usize), Ordering::Relaxed);
            if vt.is_none() {
                missing.push(*name);
            }
        }
        if missing.len() == CLASSES.len() {
            return Err("none of the entity classes found (RTTI)".into());
        }
        if !missing.is_empty() {
            log(&format!(
                "classes not found, not highlighted: {}",
                missing.join(", ")
            ));
        }
        self.apply(cx.settings);

        let e = |e: uk_qol_runtime::hook::HookError| e.to_string();
        // SAFETY: the start of QueueWorldSprite (verified pattern); the
        // detour has its signature and ABI. The trampoline is stored before
        // the host enables the hook.
        unsafe {
            ORIG_SUBMIT.store(
                cx.hooks
                    .detour(target, submit as SubmitFn as usize)
                    .map_err(e)?,
                Ordering::Relaxed,
            );
        }
        // Optional parts: a miss leaves the rest of the highlight working.
        let optional = |name: &str| cx.patterns.get(name).map(|&rva| cx.base + rva as usize);
        match optional("behind_overlay") {
            // SAFETY: the start of the overlay pass (verified pattern); the
            // detour has its signature and ABI.
            Some(at) => match unsafe {
                cx.hooks
                    .detour(at, behind::overlay as behind::OverlayFn as usize)
            } {
                Ok(t) => behind::ORIG.store(t, Ordering::Relaxed),
                Err(err) => log(&format!("items behind objects not colored: {err}")),
            },
            None => log("items behind objects not colored (pattern behind_overlay not found)"),
        }
        self.hook_effects(cx, optional("effect_quad"));
        // The shader change: only possible before the game compiles its
        // shaders (startup). Later, `get_proc` is never asked for
        // glShaderSource again and the fallback look is used.
        #[cfg(windows)]
        if self.get_proc.is_none() && !shader::patched() {
            // SAFETY: the exe is loaded and bound; the detour has
            // SDL_GL_GetProcAddress's signature.
            match unsafe {
                uk_qol_runtime::iat::IatHook::install(
                    cx.base,
                    "SDL3.dll",
                    "SDL_GL_GetProcAddress",
                    shader::get_proc as shader::GetProcFn as usize,
                    &shader::ORIG_GET_PROC,
                )
            } {
                Ok(h) => self.get_proc = Some(h),
                Err(err) => log(&format!("no colored glow (SDL_GL_GetProcAddress: {err:?})")),
            }
        }
        Ok(())
    }

    fn on_unload(&mut self) {
        ON.store(false, Ordering::Relaxed);
        // The import stays redirected: the wrapper only changes the one
        // shader, which looks the same as before for every sprite the mod
        // doesn't mark, and removing it while the game might be fetching GL
        // functions would race with that.
    }

    fn on_settings(&mut self, settings: &toml::Table) {
        self.apply(settings);
    }

    fn on_input(&mut self, ev: &InputEvent) -> bool {
        if !KEYS.iter().any(|k| hotkey::pressed(ID, k, ev)) {
            return false;
        }
        self.shown = !self.shown;
        ON.store(self.shown, Ordering::Relaxed);
        uk_qol_runtime::bridge::request_setting(ID, "shown", toml::Value::Boolean(self.shown));
        toast::show(if self.shown {
            "Item highlight on"
        } else {
            "Item highlight off"
        });
        true
    }
}

/// Constructor passed to `uk_qol_runtime::start`.
pub fn new() -> Box<dyn Mod> {
    Box::new(ItemHighlight::default())
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
        let pats = PatternFile::parse(m.patterns()).unwrap();
        assert_eq!(pats.patterns.len(), 4);
    }

    #[test]
    fn decoration_groups() {
        let all = CAT_SWITCHES | CAT_OBJECTS | CAT_BREAKABLES | CAT_LIGHTS;
        let usable = DecoState {
            can_interact: true,
            priority: 99,
            ..Default::default()
        };
        assert_eq!(decoration_group(&usable, all), Some(CAT_OBJECTS));
        // Custom scripts (the encounter boulder, signs) are at 500.
        let custom = DecoState {
            priority: 500,
            ..usable
        };
        assert_eq!(decoration_group(&custom, all), Some(CAT_OBJECTS));
        // Used up (50), nothing to do (0), or not clickable.
        for s in [
            DecoState {
                priority: 50,
                ..usable
            },
            DecoState {
                priority: 0,
                ..usable
            },
            DecoState {
                can_interact: false,
                ..usable
            },
        ] {
            assert_eq!(decoration_group(&s, all), None, "{s:?}");
        }
        let light = DecoState {
            light: true,
            priority: 500,
            ..usable
        };
        assert_eq!(decoration_group(&light, all), Some(CAT_LIGHTS));
        assert_eq!(decoration_group(&light, all & !CAT_LIGHTS), None);
        // Barrels: hit points left and not yet broken.
        let barrel = DecoState {
            hp: 10,
            ..Default::default()
        };
        assert_eq!(decoration_group(&barrel, all), Some(CAT_BREAKABLES));
        for s in [
            DecoState {
                hp: -1,
                sprite_state: 1,
                ..barrel
            },
            DecoState {
                sprite_state: 1,
                ..barrel
            },
            DecoState { hp: 0, ..barrel },
        ] {
            assert_eq!(decoration_group(&s, all), None, "{s:?}");
        }
        // A keyhole is only ever a switch.
        let lock = DecoState {
            lock: true,
            priority: 500,
            ..usable
        };
        assert_eq!(decoration_group(&lock, all), Some(CAT_SWITCHES));
        assert_eq!(decoration_group(&lock, all & !CAT_SWITCHES), None);
        assert_eq!(decoration_group(&usable, all & !CAT_OBJECTS), None);
        // A dug grave keeps its priority; its sprite changes.
        let dug = DecoState {
            priority: 250,
            sprite_state: 1,
            ..usable
        };
        assert_eq!(decoration_group(&dug, all), None);
        // Campfires sit in sprite state 1 from the start (effects only).
        let campfire = DecoState {
            sprite_state: 1,
            ..light
        };
        assert_eq!(decoration_group(&campfire, all), Some(CAT_LIGHTS));
        // Braziers that can't be switched only say so.
        let fixed = DecoState {
            fixed: true,
            ..light
        };
        assert_eq!(decoration_group(&fixed, all), None);
        // Apple trees: apples shown means usable, even at priority 0.
        let apples = DecoState {
            apple_tree: true,
            priority: 0,
            ..usable
        };
        assert_eq!(decoration_group(&apples, all), Some(CAT_OBJECTS));
        let bare = DecoState {
            sprite_state: 1,
            ..apples
        };
        assert_eq!(decoration_group(&bare, all), None);
    }

    #[test]
    fn behind_copies() {
        assert!(behind::is_copy([1.0, 1.0, 1.0, 0.5], 0.0));
        // The hovered item's two commands and the screen fade.
        assert!(!behind::is_copy([0.5, 0.5, 0.5, 1.0], 0.0));
        assert!(!behind::is_copy([1.0, 1.0, 1.0, 1.0], 1.0));
        assert!(!behind::is_copy([0.0, 0.0, 0.0, 0.3], 0.0));
        assert!((behind::linear(1.0) - 1.0).abs() < 1e-6);
        assert!((behind::linear(0.2) - 0.0331).abs() < 1e-3);
    }

    #[test]
    fn group_colors() {
        let m = Manifest::parse(new().manifest()).unwrap();
        for (key, default) in COLORS {
            let s = m.settings.iter().find(|s| s.key == key).expect(key);
            assert_eq!(s.default.as_str(), Some(default), "{key}");
        }
    }

    #[test]
    fn shader_rewrite() {
        let src = "float highlight = fog_factor.y ;\n\
                   result = result * highlight * 0.5 + mix( result, \
                   vec4( result.a, result.a, result.a, result.a ), highlight *0.5 ) ;";
        let out = shader::rewrite(src).unwrap();
        // The game's statement is kept, in the else branch.
        assert!(out.contains("if ( highlight >= 2.0 ) {"));
        assert!(out.contains(
            "} else { result = result * highlight * 0.5 + mix( result, \
             vec4( result.a, result.a, result.a, result.a ), highlight *0.5 ) ; }"
        ));
        // Other shaders are passed on unchanged.
        assert_eq!(shader::rewrite("void main() { frag_color = c ; }"), None);
    }
}
