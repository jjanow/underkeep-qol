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
//!     (barrels, crates, urns, webs, furniture) is breakable.
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
/// Flags of an item's second sprite (drawn under it, never highlighted by
/// the game either).
const FLAGS_UNDERLAY: u16 = 0x2;

/// The sprite definition's own color (3 x f32 RGB): the game multiplies it
/// with the record's color for the vertex color (it tints shared grey
/// sprites, for example a red robe).
const SPRITE_COLOR: usize = 0x14;

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
/// Interact priority the game's scripts give an object after its first use
/// when it still has a "done" text; 0 when there's nothing (left) to do.
/// Unused objects are at 99 and up.
const PRIORITY_USED: i32 = 50;

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
/// The highlight color, as f32 bits of r, g, b.
static TINT: [AtomicU32; 3] = [const { AtomicU32::new(0) }; 3];
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
    /// A torch or brazier: interacting only switches its light.
    light: bool,
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
            light: contains(name, b"torch") || contains(name, b"brazier"),
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
    } else if s.can_interact && s.priority > PRIORITY_USED {
        if s.light { CAT_LIGHTS } else { CAT_OBJECTS }
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
    let strength = f32_of(&STRENGTH);
    // With the changed shader, 2 + strength selects the recoloring (the
    // game only uses 0..1); without it, a plain tint and a white glow.
    let highlight = if shader::patched() {
        shader::MARK + strength
    } else {
        strength * 0.5
    };
    // SAFETY: the record's sprite definition (set by the game's helper).
    let sprite = unsafe { (d.add(D_SPRITE) as *const usize).read_unaligned() };
    for (i, t) in TINT.iter().enumerate() {
        let o = D_COLOR + i * 4;
        let color = if shader::patched() && sprite != 0 {
            // The recoloring wants exactly the highlight color: undo the
            // sprite's own color, which the game multiplies in.
            // SAFETY: the sprite definition's color floats.
            let own = unsafe { ((sprite + SPRITE_COLOR + i * 4) as *const f32).read_unaligned() };
            f32_of(t) / own.max(0.05)
        } else {
            f(o) * f32_of(t)
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
    /// `strength`, with the texture's light and dark parts; alpha is kept.
    const RECOLOR: &str = "if ( highlight >= 2.0 ) { \
                           vec4 qol_t = texture( t_color, uv ) ; \
                           float qol_l = dot( qol_t.rgb, vec3( 0.299, 0.587, 0.114 ) ) ; \
                           result = vec4( mix( result.rgb, color.rgb * ( 0.6 * qol_t.a + 0.7 * qol_l ), \
                           min( highlight - 2.0, 1.0 ) ), result.a ) ; \
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
        let rgb = settings
            .get("color")
            .and_then(|v| v.as_str())
            .and_then(uk_qol_runtime::host::parse_color)
            .unwrap_or([0x33, 0xff, 0x33]);
        for (a, c) in TINT.iter().zip(rgb) {
            a.store((c as f32 / 255.0).to_bits(), Ordering::Relaxed);
        }
        let strength = settings
            .get("strength")
            .and_then(|v| v.as_float())
            .unwrap_or(0.85) as f32;
        STRENGTH.store(strength.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
        DEBUG.store(
            settings
                .get("debug")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            Ordering::Relaxed,
        );
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
        assert_eq!(pats.patterns.len(), 2);
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
