//! The game's own item tooltip for the second weapon slot.
//!
//! The game shows one item tooltip at a time, through a small state machine:
//! a "show" call arms it for the current frame with a text and a position
//! (`tooltip_show`), and the tooltip pass near the end of the frame
//! (`tooltip_draw`) draws it after the hover delay, then disarms it; a frame
//! without a show call hides it. A second text next to it (`tooltip_compare`)
//! is the item it's compared with: hovering an inventory weapon on the
//! character sheet shows the weapon in hand there.
//!
//! The slot's weapon isn't a game item (the mod keeps it), so for a tooltip
//! the mod builds a temporary item from the stored def, has the game write
//! both texts (`item_tooltip`, the same function the sheet uses), and takes
//! the item out again at once (`Item_Take` queues it for deletion; it never
//! reaches the inventory or a hero). The texts are kept while the slot stays
//! hovered and rebuilt when the hover starts again or anything about the
//! weapon, the hero or the weapon in hand changes.
//!
//! The HUD pass (egui, drawn later in the frame) says what is hovered with
//! [`want`]; the detour of `tooltip_draw` takes that once per frame and arms
//! the game's tooltip just before the game's own pass, at the same position
//! the sheet uses for its items (the layout point
//! `inventory_item_tooltip_position`). The slot's warning (the weapon is kept
//! by the mod) is appended to the text.
//!
//! Everything here runs on the game thread; the mutex only satisfies Rust.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use uk_qol_runtime::guard;

use crate::game::{Game, Item, SLOT_WEAPON};
use crate::state::StoredItem;
use crate::{ID, LOSS_HINT, log};

/// The game's `String`: a reference-counted buffer (null for literals), the
/// bytes and their length (the padding covers what callees may assume).
#[repr(C)]
struct GString {
    buf: *mut i32,
    data: *const u8,
    len: i32,
    _pad: [u32; 3],
}

impl GString {
    fn empty() -> GString {
        GString {
            buf: std::ptr::null_mut(),
            data: c"".as_ptr().cast(),
            len: 0,
            _pad: [0; 3],
        }
    }

    /// A literal over `s` (which must end in an uncounted NUL and live
    /// forever).
    fn literal(s: &'static str) -> GString {
        let text = s.strip_suffix('\0').unwrap_or(s);
        GString {
            buf: std::ptr::null_mut(),
            data: s.as_ptr(),
            len: text.len() as i32,
            _pad: [0; 3],
        }
    }
}

type DrawFn = unsafe extern "system" fn(f32);
type ShowFn = unsafe extern "system" fn(*const GString, *const f32, u8);
type ItemTooltipFn =
    unsafe extern "system" fn(*mut GString, usize, usize, i32, u8, u8) -> *mut GString;
type LayoutPointFn = unsafe extern "system" fn(*mut [f32; 2], *const GString) -> *mut [f32; 2];
type ConcatFn =
    unsafe extern "system" fn(*mut GString, *const GString, *const GString) -> *mut GString;
type CopyFn = unsafe extern "system" fn(*mut GString, *const GString) -> *mut GString;
type FreeFn = unsafe extern "system" fn(*mut GString);

static ORIG_DRAW: AtomicUsize = AtomicUsize::new(0);
static SHOW: AtomicUsize = AtomicUsize::new(0);
static COMPARE: AtomicUsize = AtomicUsize::new(0);
static ITEM_TOOLTIP: AtomicUsize = AtomicUsize::new(0);
static LAYOUT_POINT: AtomicUsize = AtomicUsize::new(0);
static CONCAT: AtomicUsize = AtomicUsize::new(0);
static COPY: AtomicUsize = AtomicUsize::new(0);
static FREE: AtomicUsize = AtomicUsize::new(0);
/// The detour is in and every address resolved.
static READY: AtomicBool = AtomicBool::new(false);
/// A tooltip couldn't be built: the slot falls back to its own tooltip.
static FAILED: AtomicBool = AtomicBool::new(false);

/// The patterns this needs (all optional: without them the slot keeps its
/// own tooltip).
const PATTERNS: [&str; 8] = [
    "tooltip_draw",
    "tooltip_show",
    "tooltip_compare",
    "item_tooltip",
    "tooltip_layout_point",
    "string_concat",
    "string_copy",
    "string_free",
];

/// Where the sheet puts its item tooltips.
const POSITION_KEY: &str = "inventory_item_tooltip_position\0";
/// The slot's line under the game's text.
const HINT: &str = "{a:center}{color_text_darker}Weapon slot II (mod). Click: pick it up, then \
                    click the weapon slot to swap. Right-click: move it to the inventory.\n";

/// What the tooltip shows; a change rebuilds the texts.
#[derive(Clone, PartialEq)]
struct Key {
    hero: usize,
    stored: StoredItem,
    /// The weapon in hand (pointer), compared against.
    hand: usize,
}

struct Built {
    key: Key,
    text: GString,
    /// The weapon in hand's text, if there is one.
    compare: Option<GString>,
}

struct Shared {
    game: Option<Game>,
    /// Set by the HUD pass while the slot is hovered; taken by the draw.
    want: Option<(usize, StoredItem)>,
    built: Option<Built>,
}

// SAFETY: the game Strings in `built` are only touched on the game thread.
unsafe impl Send for Shared {}

static SHARED: Mutex<Shared> = Mutex::new(Shared {
    game: None,
    want: None,
    built: None,
});

fn shared() -> std::sync::MutexGuard<'static, Shared> {
    SHARED.lock().unwrap_or_else(|e| e.into_inner())
}

/// Whether the game's tooltip is used for the slot (otherwise the slot
/// shows its own).
pub fn active() -> bool {
    READY.load(Ordering::Relaxed) && !FAILED.load(Ordering::Relaxed)
}

/// From the HUD pass, every frame the slot of `hero` is hovered with
/// `stored` in it and nothing on the cursor.
pub fn want(hero: usize, stored: &StoredItem) {
    shared().want = Some((hero, stored.clone()));
}

/// The game handle for the detour (`None` stops it acting and drops the
/// texts).
pub fn set_game(game: Option<Game>) {
    let mut s = shared();
    s.game = game;
    s.want = None;
    if game.is_none()
        && let Some(b) = s.built.take()
    {
        // SAFETY: game thread (load/unload); our own references.
        unsafe { release(b) };
    }
}

/// Installs the draw detour if every pattern resolved (from `on_load`).
///
/// # Safety
/// `at` must give the resolved address of each pattern in [`PATTERNS`].
pub unsafe fn install(
    hooks: &mut uk_qol_runtime::hook::HookSet,
    at: impl Fn(&str) -> Option<usize>,
) {
    let mut addrs = [0usize; PATTERNS.len()];
    for (i, name) in PATTERNS.iter().enumerate() {
        match at(name) {
            Some(a) => addrs[i] = a,
            None => {
                log(&format!(
                    "pattern {name} not found; slot II shows its own tooltip"
                ));
                READY.store(false, Ordering::Relaxed);
                return;
            }
        }
    }
    let [
        draw,
        show,
        compare,
        item_tooltip,
        layout,
        concat,
        copy,
        free,
    ] = addrs;
    SHOW.store(show, Ordering::Relaxed);
    COMPARE.store(compare, Ordering::Relaxed);
    ITEM_TOOLTIP.store(item_tooltip, Ordering::Relaxed);
    LAYOUT_POINT.store(layout, Ordering::Relaxed);
    CONCAT.store(concat, Ordering::Relaxed);
    COPY.store(copy, Ordering::Relaxed);
    FREE.store(free, Ordering::Relaxed);
    // SAFETY: the resolved start of tooltip_draw; the detour keeps its one
    // float argument.
    match unsafe { hooks.detour(draw, on_draw as DrawFn as usize) } {
        Ok(t) => {
            ORIG_DRAW.store(t, Ordering::Relaxed);
            READY.store(true, Ordering::Relaxed);
        }
        Err(e) => log(&format!(
            "couldn't hook the tooltip pass ({e}); slot II shows its own tooltip"
        )),
    }
}

/// Frees the texts of `b`.
///
/// # Safety
/// Game thread, after [`install`].
unsafe fn release(b: Built) {
    // SAFETY: the resolved String_Free on Strings we own.
    unsafe {
        let free: FreeFn = std::mem::transmute(FREE.load(Ordering::Relaxed));
        let mut t = b.text;
        free(&mut t);
        if let Some(mut c) = b.compare {
            free(&mut c);
        }
    }
}

/// The tooltip text of `item` compared with `other` (0 for none) for hero
/// `hero`, as the sheet builds it.
///
/// # Safety
/// Game thread; live items.
unsafe fn item_text(item: Item, other: usize, hero: usize) -> GString {
    // SAFETY: the resolved item tooltip function with its signature.
    unsafe {
        let f: ItemTooltipFn = std::mem::transmute(ITEM_TOOLTIP.load(Ordering::Relaxed));
        let mut out = GString::empty();
        f(&mut out, item as usize, other, hero as i32, 0, 0);
        out
    }
}

/// Builds the texts for `key`: a temporary item from the stored weapon,
/// both texts, then the item is taken out of the game again.
///
/// # Safety
/// Game thread.
unsafe fn build(game: &Game, key: Key) -> Result<Built, String> {
    let it = game.create(&key.stored)?;
    // SAFETY: a fresh item nobody owns, taken right after; the hand weapon
    // is live on its hero (read this frame).
    unsafe {
        let mut text = item_text(it, key.hand, key.hero);
        let compare = (key.hand != 0).then(|| item_text(key.hand as Item, it as usize, key.hero));
        game.take(it);
        let concat: ConcatFn = std::mem::transmute(CONCAT.load(Ordering::Relaxed));
        let free: FreeFn = std::mem::transmute(FREE.load(Ordering::Relaxed));
        let hint = GString::literal(HINT_FULL.get_or_init(hint_text));
        let mut joined = GString::empty();
        concat(&mut joined, &text, &hint);
        free(&mut text);
        Ok(Built {
            key,
            text: joined,
            compare,
        })
    }
}

static HINT_FULL: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();

/// [`HINT`] plus the loss warning, NUL-terminated, for the life of the
/// process.
fn hint_text() -> &'static str {
    let s = format!("{HINT}{{a:center}}{{color_text_darker}}{LOSS_HINT}\n\0");
    Box::leak(s.into_boxed_str())
}

/// Arms the game's tooltip for this frame if the slot was hovered.
///
/// # Safety
/// Game thread, inside the frame (from the draw detour).
unsafe fn arm() {
    let mut s = shared();
    let Some(game) = s.game else { return };
    let Some((hero, stored)) = s.want.take() else {
        // Not hovered: the next hover builds afresh (stats may change).
        if let Some(b) = s.built.take() {
            // SAFETY: game thread.
            unsafe { release(b) };
        }
        return;
    };
    let hand = game.equipped(hero, SLOT_WEAPON) as usize;
    let key = Key { hero, stored, hand };
    if s.built.as_ref().is_none_or(|b| b.key != key) {
        if let Some(b) = s.built.take() {
            // SAFETY: game thread.
            unsafe { release(b) };
        }
        // SAFETY: game thread.
        match unsafe { build(&game, key) } {
            Ok(b) => s.built = Some(b),
            Err(e) => {
                log(&format!(
                    "no game tooltip for slot II ({e}); using the mod's own"
                ));
                FAILED.store(true, Ordering::Relaxed);
                return;
            }
        }
    }
    let Some(b) = &s.built else { return };
    // SAFETY: the resolved functions and globals with their signatures.
    unsafe {
        let layout: LayoutPointFn = std::mem::transmute(LAYOUT_POINT.load(Ordering::Relaxed));
        let show: ShowFn = std::mem::transmute(SHOW.load(Ordering::Relaxed));
        let copy: CopyFn = std::mem::transmute(COPY.load(Ordering::Relaxed));
        let mut pos = [0f32; 2];
        let name = GString::literal(POSITION_KEY);
        layout(&mut pos, &name);
        show(&b.text, pos.as_ptr(), 1);
        // The show call clears the compare text; set it after (a copy: the
        // game releases its reference on the next show).
        if let Some(c) = &b.compare {
            copy(COMPARE.load(Ordering::Relaxed) as *mut GString, c);
        }
    }
}

/// The game's tooltip pass (see `patterns.toml`).
unsafe extern "system" fn on_draw(dt: f32) {
    if !FAILED.load(Ordering::Relaxed) {
        // SAFETY: game thread, inside the frame.
        guard::call(ID, || unsafe { arm() });
    }
    // SAFETY: set before the hook was enabled.
    let orig: DrawFn = unsafe { std::mem::transmute(ORIG_DRAW.load(Ordering::Relaxed)) };
    // SAFETY: the original with the game's argument.
    unsafe { orig(dt) }
}
