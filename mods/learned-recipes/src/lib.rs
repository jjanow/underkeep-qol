//! `learned-recipes`: shows which recipes the party has learned (README,
//! "Mod designs").
//!
//! The game's rules: a recipe item (item_type 19) only shows a recipe's
//! ingredients. A recipe is *learned* once its id is in the party's list
//! (`Party+0xb30` count, `+0xb38` array of `String`s, saved as
//! `unlocked_recipes`), which a successful mortar craft adds to; the mortar's
//! recipe grid shows exactly that list. Buying or holding the recipe item
//! doesn't teach it. A recipe item names its recipe in its params
//! (`Item+0x120`, `recipe` at +0x2f0), and the ids match the recipe list's
//! one to one.
//!
//! Two marks, each with its own setting:
//! - **Tooltip line**: the item tooltip function is detoured; after the game
//!   builds a recipe's tooltip, the mod appends the text key
//!   `uk_qol_recipe_learned` or `uk_qol_recipe_not_learned`, looked up in
//!   the game's language (the keys are added by this mod's data patches,
//!   `patches/`), with the game's own string functions.
//! - **Icon mark**: the item grid's slot drawing (the inventory and both
//!   shop lists) is detoured; after the game draws the slots, the mod
//!   queues the game's quest check-mark sprite in the corner of each learned
//!   recipe's slot, on the same sprite batch, so it sits under the cursor,
//!   a dragged item and tooltips, and is clipped with the list.
//!
//! Nothing is written to the game's state or to saves.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use uk_qol_runtime::game;
use uk_qol_runtime::guard;
use uk_qol_runtime::host::{LoadCtx, Mod};
use uk_qol_runtime::savehook;

const ID: &str = "learned-recipes";
const ICON_MARK: &str = "icon_mark";
const TOOLTIP: &str = "tooltip";

/// Text keys added by `patches/text_common_*.toml`.
const KEY_LEARNED: &str = "uk_qol_recipe_learned";
const KEY_NOT_LEARNED: &str = "uk_qol_recipe_not_learned";
/// Used when a key is missing (the mod was turned on after the game read
/// its texts, so the patches weren't applied).
const FALLBACK_LEARNED: &str =
    "{a:center}{color_msg_success}{sprite:hud_icon_quest_completed} Learned\n\0";
const FALLBACK_NOT_LEARNED: &str = "{a:center}{color_text_darker}Not learned yet\n\0";

/// The check-mark sprite the icon mark uses (the quest log's "completed").
const MARK_SPRITE: &str = "hud_icon_quest_completed";

// Game structures (1.0.5).
const STRING_SIZE: usize = 0x18;
const PARTY_RECIPE_COUNT: usize = 0xb30; // i32
const PARTY_RECIPES: usize = 0xb38; // *String
const ITEM_TYPE: usize = 0x100; // i32
const ITEM_TYPE_RECIPE: i32 = 19;
const ITEM_RECIPE: usize = 0x120 + 0x2f0; // String: the recipe id
const GRID_SLOT_COUNT: usize = 0x30; // i32
const GRID_SLOTS: usize = 0x38; // *Slot
const SLOT_SIZE: usize = 0x30;
const SLOT_RECT: usize = 0x10; // f32 x, y, w, h
const SLOT_ITEM: usize = 0x20; // *Item
const UI_BATCH: usize = 0x630; // **Batch
const SPRITE_ENTRY: usize = 0x28;
const SPRITE_NAME: usize = 0x08; // String
/// The table's own limit (the registration refuses more).
const SPRITE_MAX: i32 = 20000;
/// Sprite: +0x0c / +0x10 the pivot (f32, pixels).
const SPRITE_PIVOT: usize = 0x0c;

/// Settings, read by the detours.
static SHOW_MARK: AtomicBool = AtomicBool::new(true);
static SHOW_TOOLTIP: AtomicBool = AtomicBool::new(true);

static ORIG_TOOLTIP: AtomicUsize = AtomicUsize::new(0);
static ORIG_GRID: AtomicUsize = AtomicUsize::new(0);

/// Resolved addresses, set in `on_load` before the hooks are enabled.
static DRAW_SPRITE: AtomicUsize = AtomicUsize::new(0);
static G_UI: AtomicUsize = AtomicUsize::new(0);
static G_SPRITES: AtomicUsize = AtomicUsize::new(0);
static G_SPRITE_COUNT: AtomicUsize = AtomicUsize::new(0);
static TEXT_GET: AtomicUsize = AtomicUsize::new(0);
static CONCAT: AtomicUsize = AtomicUsize::new(0);
static ASSIGN: AtomicUsize = AtomicUsize::new(0);
static FREE: AtomicUsize = AtomicUsize::new(0);

/// Index of [`MARK_SPRITE`] in the sprite table + 1 (0 = not looked up
/// yet; `usize::MAX` = not found, logged once).
static MARK_INDEX: AtomicUsize = AtomicUsize::new(0);

/// The game's `String`: a reference-counted buffer (null for literals), the
/// bytes and their length.
#[repr(C)]
struct GString {
    buf: *mut u8,
    data: *const u8,
    len: i32,
}

impl GString {
    fn empty() -> GString {
        GString {
            buf: std::ptr::null_mut(),
            data: c"".as_ptr().cast(),
            len: 0,
        }
    }

    /// A literal over `s`, which must end in a NUL that isn't counted and
    /// outlive the string.
    fn literal(s: &str) -> GString {
        let text = s.strip_suffix('\0').unwrap_or(s);
        GString {
            buf: std::ptr::null_mut(),
            data: s.as_ptr(),
            len: text.len() as i32,
        }
    }
}

fn bool_setting(settings: &toml::Table, key: &str) -> bool {
    settings
        .get(key)
        .and_then(toml::Value::as_bool)
        .unwrap_or(true)
}

/// The bytes of the game `String` at `s`, if they look sane.
///
/// # Safety
/// `s` must point at a game `String` (checked reads otherwise).
unsafe fn string_bytes<'a>(s: usize) -> Option<&'a [u8]> {
    // SAFETY: checked reads.
    let (data, len) = unsafe {
        (
            game::read::<usize>(s + game::STRING_DATA)?,
            game::read::<i32>(s + game::STRING_LEN)?,
        )
    };
    let len = usize::try_from(len).ok()?;
    if len > game::STRING_MAX || (len > 0 && !game::readable(data, len)) {
        return None;
    }
    if len == 0 {
        return Some(&[]);
    }
    // SAFETY: just checked readable.
    Some(unsafe { std::slice::from_raw_parts(data as *const u8, len) })
}

/// The recipe id of `item` if it is a recipe item.
///
/// # Safety
/// `item` must be 0 or a live Item.
unsafe fn recipe_of<'a>(item: usize) -> Option<&'a [u8]> {
    if item == 0 {
        return None;
    }
    // SAFETY: checked read of a live item.
    if unsafe { game::read::<i32>(item + ITEM_TYPE)? } != ITEM_TYPE_RECIPE {
        return None;
    }
    // SAFETY: the item's recipe String.
    unsafe { string_bytes(item + ITEM_RECIPE) }.filter(|r| !r.is_empty())
}

/// Whether `party`'s learned list holds `recipe` (compared like the game's
/// unlock check: same length and bytes, ignoring ASCII case).
///
/// # Safety
/// `party` must be the game's party struct (or a buffer laid out like it).
unsafe fn learned_in(party: usize, recipe: &[u8]) -> bool {
    // SAFETY: checked reads.
    let (count, list) = unsafe {
        (
            game::read::<i32>(party + PARTY_RECIPE_COUNT).unwrap_or(0),
            game::read::<usize>(party + PARTY_RECIPES).unwrap_or(0),
        )
    };
    if count <= 0 || count > 10_000 || list == 0 {
        return false;
    }
    (0..count as usize).any(|i| {
        // SAFETY: the i-th String of the list.
        unsafe { string_bytes(list + i * STRING_SIZE) }
            .is_some_and(|s| s.eq_ignore_ascii_case(recipe))
    })
}

/// Whether the party has learned `recipe`.
fn learned(recipe: &[u8]) -> bool {
    // SAFETY: the resolved party global, read on the game's thread.
    savehook::party().is_some_and(|p| unsafe { learned_in(p, recipe) })
}

// --- Tooltip line -----------------------------------------------------------

type TooltipFn = unsafe extern "system" fn(usize, usize, usize, usize, usize, usize) -> usize;
type TextGetFn = unsafe extern "system" fn(*mut GString, *const GString) -> *mut GString;
type ConcatFn =
    unsafe extern "system" fn(*mut GString, *const GString, *const GString) -> *mut GString;
type AssignFn = unsafe extern "system" fn(*mut GString, *const GString);
type FreeFn = unsafe extern "system" fn(*mut GString);

/// Appends the learned / not learned line to the tooltip text `out`.
///
/// # Safety
/// Game thread; `out` is the String the tooltip function just filled in.
unsafe fn append_line(out: usize, is_learned: bool) {
    // SAFETY: the resolved game functions, with their signatures.
    unsafe {
        let text_get: TextGetFn = std::mem::transmute(TEXT_GET.load(Ordering::Relaxed));
        let concat: ConcatFn = std::mem::transmute(CONCAT.load(Ordering::Relaxed));
        let assign: AssignFn = std::mem::transmute(ASSIGN.load(Ordering::Relaxed));
        let free: FreeFn = std::mem::transmute(FREE.load(Ordering::Relaxed));
        let (key, fallback) = if is_learned {
            (concat!("uk_qol_recipe_learned", "\0"), FALLBACK_LEARNED)
        } else {
            (
                concat!("uk_qol_recipe_not_learned", "\0"),
                FALLBACK_NOT_LEARNED,
            )
        };
        let key = GString::literal(key);
        let mut line = GString::empty();
        text_get(&mut line, &key);
        // A missing key comes back as the key itself (or empty).
        let k = key_bytes(is_learned);
        let missing = string_bytes(&line as *const GString as usize)
            .is_none_or(|s| s.is_empty() || s.windows(k.len()).any(|w| w == k));
        if missing {
            free(&mut line);
            line = GString::literal(fallback);
        }
        let mut joined = GString::empty();
        concat(&mut joined, out as *const GString, &line);
        assign(out as *mut GString, &joined);
        free(&mut joined);
        free(&mut line);
    }
}

fn key_bytes(is_learned: bool) -> &'static [u8] {
    if is_learned {
        KEY_LEARNED.as_bytes()
    } else {
        KEY_NOT_LEARNED.as_bytes()
    }
}

/// The item tooltip function (see `patterns.toml`).
unsafe extern "system" fn item_tooltip(
    out: usize,
    item: usize,
    compare: usize,
    mode: usize,
    a5: usize,
    a6: usize,
) -> usize {
    // SAFETY: set before the hook was enabled.
    let orig: TooltipFn = unsafe { std::mem::transmute(ORIG_TOOLTIP.load(Ordering::Relaxed)) };
    // SAFETY: the original with the game's arguments.
    let r = unsafe { orig(out, item, compare, mode, a5, a6) };
    if SHOW_TOOLTIP.load(Ordering::Relaxed) && r != 0 {
        guard::call(ID, || {
            // SAFETY: the game passed a live item; `r` is its tooltip String.
            unsafe {
                if let Some(recipe) = recipe_of(item) {
                    append_line(r, learned(recipe));
                }
            }
        });
    }
    r
}

// --- Icon mark --------------------------------------------------------------

type GridFn = unsafe extern "system" fn(usize, usize, usize, usize, usize);
type DrawSpriteFn =
    unsafe extern "system" fn(usize, usize, *const f32, f32, *const f32, *const f32, f32);

/// The loaded check-mark sprite, if the table has it.
///
/// # Safety
/// Game thread, after the HUD's sprites are registered.
unsafe fn mark_sprite() -> Option<usize> {
    let table = G_SPRITES.load(Ordering::Relaxed);
    let idx = match MARK_INDEX.load(Ordering::Relaxed) {
        usize::MAX => return None,
        0 => {
            // SAFETY: the resolved count global.
            let count = unsafe { game::read::<i32>(G_SPRITE_COUNT.load(Ordering::Relaxed)) }
                .unwrap_or(0)
                .clamp(0, SPRITE_MAX) as usize;
            let found = (0..count).find(|&i| {
                // SAFETY: the i-th table entry's name.
                unsafe { string_bytes(table + i * SPRITE_ENTRY + SPRITE_NAME) }
                    .is_some_and(|n| n == MARK_SPRITE.as_bytes())
            });
            match found {
                Some(i) => {
                    uk_qol_runtime::log(&format!("{ID}: check mark is sprite {i} of {count}"));
                    MARK_INDEX.store(i + 1, Ordering::Relaxed);
                    i
                }
                None => {
                    if count == 0 {
                        return None; // not registered yet; try again later
                    }
                    uk_qol_runtime::log(&format!(
                        "{ID}: no sprite {MARK_SPRITE} among {count}; no icon marks"
                    ));
                    MARK_INDEX.store(usize::MAX, Ordering::Relaxed);
                    return None;
                }
            }
        }
        n => n - 1,
    };
    // SAFETY: checked read of the entry's sprite pointer.
    unsafe { game::read::<usize>(table + idx * SPRITE_ENTRY) }.filter(|&s| s != 0)
}

/// Where the mark's pivot goes for a slot at (x, y, w, h): the top-right
/// corner of the slot's icon square, 1 unit in, given the sprite's pivot and
/// size. Inventory slots are square; a shop list's rows are wide, with the
/// icon in a square at their left, so the mark stays off the price.
fn mark_pos(rect: [f32; 4], pivot: [f32; 2], size: [f32; 2]) -> [f32; 2] {
    let [x, y, w, h] = rect;
    let side = w.min(h);
    [x + side - 1.0 - (size[0] - pivot[0]), y + 1.0 + pivot[1]]
}

/// Queues a check mark on every learned recipe in `grid`.
///
/// # Safety
/// Game thread, right after the game drew `grid`'s slots.
unsafe fn mark_grid(grid: usize) {
    // SAFETY: checked reads of the grid and the game's globals.
    unsafe {
        let count = game::read::<i32>(grid + GRID_SLOT_COUNT).unwrap_or(0);
        let slots = game::read::<usize>(grid + GRID_SLOTS).unwrap_or(0);
        if count <= 0 || count > 10_000 || slots == 0 {
            return;
        }
        let Some(party) = savehook::party() else {
            return;
        };
        let mut sprite = None;
        let mut batch = 0;
        for i in 0..count as usize {
            let slot = slots + i * SLOT_SIZE;
            let Some(item) = game::read::<usize>(slot + SLOT_ITEM) else {
                continue;
            };
            let Some(recipe) = recipe_of(item) else {
                continue;
            };
            if !learned_in(party, recipe) {
                continue;
            }
            if sprite.is_none() {
                sprite = mark_sprite();
                let ui = game::read::<usize>(G_UI.load(Ordering::Relaxed)).unwrap_or(0);
                let pb = if ui == 0 {
                    None
                } else {
                    game::read::<usize>(ui + UI_BATCH)
                };
                batch = pb.and_then(|p| game::read::<usize>(p)).unwrap_or(0);
                if sprite.is_none() || batch == 0 {
                    return;
                }
            }
            let Some(s) = sprite else { return };
            let Some(rect) = game::read::<[f32; 4]>(slot + SLOT_RECT) else {
                continue;
            };
            let Some(pivot) = game::read::<[f32; 2]>(s + SPRITE_PIVOT) else {
                return;
            };
            let Some(size) = game::read::<[f32; 2]>(s + 4) else {
                return;
            };
            let pos = mark_pos(rect, pivot, size);
            let scale = [1.0f32, 1.0];
            let rgba = [1.0f32, 1.0, 1.0, 1.0];
            let draw: DrawSpriteFn = std::mem::transmute(DRAW_SPRITE.load(Ordering::Relaxed));
            draw(
                batch,
                s,
                pos.as_ptr(),
                0.0,
                scale.as_ptr(),
                rgba.as_ptr(),
                0.0,
            );
        }
    }
}

/// The item grid's slot drawing (see `patterns.toml`).
unsafe extern "system" fn grid_draw_slots(
    grid: usize,
    hover: usize,
    mode: usize,
    hero: usize,
    cb: usize,
) {
    // SAFETY: set before the hook was enabled.
    let orig: GridFn = unsafe { std::mem::transmute(ORIG_GRID.load(Ordering::Relaxed)) };
    // SAFETY: the original with the game's arguments.
    unsafe { orig(grid, hover, mode, hero, cb) };
    if SHOW_MARK.load(Ordering::Relaxed) && grid != 0 {
        // SAFETY: the game just drew this grid, on its thread.
        guard::call(ID, || unsafe { mark_grid(grid) });
    }
}

// --- Mod --------------------------------------------------------------------

pub struct LearnedRecipes;

impl Mod for LearnedRecipes {
    fn id(&self) -> &'static str {
        ID
    }

    fn manifest(&self) -> &'static str {
        include_str!("../mod.toml")
    }

    fn patterns(&self) -> &'static str {
        include_str!("../patterns.toml")
    }

    fn patches(&self) -> &'static [(&'static str, &'static str)] {
        PATCHES
    }

    fn on_load(&mut self, cx: &mut LoadCtx) -> Result<(), String> {
        let at = |name: &str| {
            cx.patterns
                .get(name)
                .map(|&rva| cx.base + rva as usize)
                .ok_or_else(|| format!("pattern {name} not resolved"))
        };
        let tooltip = at("item_tooltip")?;
        let grid = at("grid_draw_slots")?;
        for (slot, name) in [
            (&DRAW_SPRITE, "draw_sprite"),
            (&G_UI, "g_ui"),
            (&G_SPRITES, "g_sprites"),
            (&G_SPRITE_COUNT, "g_sprite_count"),
            (&TEXT_GET, "text_get"),
            (&CONCAT, "string_concat"),
            (&ASSIGN, "string_assign"),
            (&FREE, "string_free"),
        ] {
            slot.store(at(name)?, Ordering::Relaxed);
        }
        self.on_settings(cx.settings);
        MARK_INDEX.store(0, Ordering::Relaxed);
        // SAFETY: both targets are function starts (verified patterns); the
        // detours have their signatures and ABI. The trampolines are stored
        // before the host enables the hooks.
        unsafe {
            ORIG_TOOLTIP.store(
                cx.hooks
                    .detour(tooltip, item_tooltip as TooltipFn as usize)
                    .map_err(|e| e.to_string())?,
                Ordering::Relaxed,
            );
            ORIG_GRID.store(
                cx.hooks
                    .detour(grid, grid_draw_slots as GridFn as usize)
                    .map_err(|e| e.to_string())?,
                Ordering::Relaxed,
            );
        }
        Ok(())
    }

    fn on_settings(&mut self, settings: &toml::Table) {
        SHOW_MARK.store(bool_setting(settings, ICON_MARK), Ordering::Relaxed);
        SHOW_TOOLTIP.store(bool_setting(settings, TOOLTIP), Ordering::Relaxed);
    }
}

static PATCHES: &[(&str, &str)] = &[
    (
        "text_common_en.toml",
        include_str!("../patches/text_common_en.toml"),
    ),
    (
        "text_common_cs.toml",
        include_str!("../patches/text_common_cs.toml"),
    ),
    (
        "text_common_de.toml",
        include_str!("../patches/text_common_de.toml"),
    ),
    (
        "text_common_es.toml",
        include_str!("../patches/text_common_es.toml"),
    ),
    (
        "text_common_fr.toml",
        include_str!("../patches/text_common_fr.toml"),
    ),
    (
        "text_common_pl.toml",
        include_str!("../patches/text_common_pl.toml"),
    ),
    (
        "text_common_ru.toml",
        include_str!("../patches/text_common_ru.toml"),
    ),
];

/// Constructor passed to `uk_qol_runtime::start`.
pub fn new() -> Box<dyn Mod> {
    Box::new(LearnedRecipes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use uk_qol_runtime::host::{Manifest, PatternFile};

    #[test]
    fn manifest_patterns_and_patches_parse() {
        let m = new();
        let manifest = Manifest::parse(m.manifest()).unwrap();
        assert_eq!(manifest.id, m.id());
        assert!(manifest.enabled, "display only: on by default");
        let d = manifest.defaults();
        assert!(bool_setting(&d, ICON_MARK) && bool_setting(&d, TOOLTIP));
        let pats = PatternFile::parse(m.patterns()).unwrap();
        assert_eq!(pats.patterns.len(), 10);
        assert_eq!(m.patches().len(), 7);
        for (name, text) in m.patches() {
            let p = dft::patch::Patch::parse(text).unwrap_or_else(|e| panic!("{name}: {e}"));
            let lang = &name["text_common_".len()..name.len() - ".toml".len()];
            assert_eq!(p.file, format!("text/text_common_{lang}.txt"));
            for key in [KEY_LEARNED, KEY_NOT_LEARNED] {
                assert!(
                    text.contains(&format!("text_common_{lang}.{key}\"")),
                    "{name}: {key}"
                );
            }
        }
    }

    #[test]
    fn patches_apply_to_a_text_document() {
        // A hand-built text document like the game's: one table per language.
        let mut doc = dft::Doc {
            version: 1,
            strings: Vec::new(),
            len_widths: Vec::new(),
            root: Vec::new(),
        };
        let table = doc.intern("text_common_en");
        let k = doc.intern("item_type_recipe");
        let v = doc.intern("Recipe");
        doc.root.push(dft::Entry::new(
            dft::Key::Name(table),
            dft::Value::Table(vec![dft::Entry::new(dft::Key::Name(k), dft::Value::Str(v))]),
        ));
        let p = dft::patch::Patch::parse(PATCHES[0].1).unwrap();
        p.apply(&mut doc).unwrap();
        let dft::Value::Table(t) = &doc.root[0].value else {
            panic!()
        };
        let names: Vec<String> = t.iter().map(|e| doc.key_name(e.key)).collect();
        assert_eq!(names, ["item_type_recipe", KEY_LEARNED, KEY_NOT_LEARNED]);
    }

    #[test]
    fn fallbacks_are_nul_terminated_literals() {
        for f in [FALLBACK_LEARNED, FALLBACK_NOT_LEARNED] {
            let s = GString::literal(f);
            assert_eq!(s.len as usize, f.len() - 1);
            assert!(f.ends_with("\n\0"));
        }
    }

    /// A fake game String over `bytes`.
    fn gs(bytes: &[u8]) -> [u8; STRING_SIZE] {
        let mut s = [0u8; STRING_SIZE];
        s[game::STRING_DATA..game::STRING_DATA + 8]
            .copy_from_slice(&(bytes.as_ptr() as usize).to_le_bytes());
        s[game::STRING_LEN..game::STRING_LEN + 4]
            .copy_from_slice(&(bytes.len() as i32).to_le_bytes());
        s
    }

    #[test]
    fn learned_list_and_recipe_items() {
        let ids: [&[u8]; 2] = [b"recipe_minor_healing", b"item_heal_potion"];
        let list: Vec<u8> = ids.iter().flat_map(|b| gs(b)).collect();
        let mut party = vec![0u8; PARTY_RECIPES + 8];
        party[PARTY_RECIPE_COUNT..PARTY_RECIPE_COUNT + 4].copy_from_slice(&2i32.to_le_bytes());
        party[PARTY_RECIPES..PARTY_RECIPES + 8]
            .copy_from_slice(&(list.as_ptr() as usize).to_le_bytes());
        let p = party.as_ptr() as usize;
        // SAFETY: local buffers laid out like the game's.
        unsafe {
            assert!(learned_in(p, b"item_heal_potion"));
            assert!(learned_in(p, b"RECIPE_minor_healing"));
            assert!(!learned_in(p, b"item_mana_potion"));
            assert!(!learned_in(p, b"item_heal"));

            let id = b"item_heal_potion";
            let mut item = vec![0u8; ITEM_RECIPE + STRING_SIZE];
            item[ITEM_RECIPE..].copy_from_slice(&gs(id));
            let it = item.as_ptr() as usize;
            assert_eq!(recipe_of(it), None, "not a recipe item");
            item[ITEM_TYPE..ITEM_TYPE + 4].copy_from_slice(&ITEM_TYPE_RECIPE.to_le_bytes());
            assert_eq!(recipe_of(it), Some(&id[..]));
            assert_eq!(recipe_of(0), None);

            // An empty list.
            party[PARTY_RECIPE_COUNT..PARTY_RECIPE_COUNT + 4].copy_from_slice(&0i32.to_le_bytes());
            assert!(!learned_in(p, b"item_heal_potion"));
        }
    }

    #[test]
    fn mark_sits_in_the_top_right_corner() {
        // A 29x29 slot at (100, 50); the 10x12 check with its pivot at (5, 6).
        let [x, y] = mark_pos([100.0, 50.0, 29.0, 29.0], [5.0, 6.0], [10.0, 12.0]);
        // Right edge 1 in from the slot's: pivot x + (10 - 5) = 128.
        assert_eq!(x + 5.0, 128.0);
        // Top edge 1 in: pivot y - 6 = 51.
        assert_eq!(y - 6.0, 51.0);
        // A wide shop row: the same corner of its icon square, not the row's.
        let [x2, y2] = mark_pos([100.0, 50.0, 140.0, 29.0], [5.0, 6.0], [10.0, 12.0]);
        assert_eq!([x2, y2], [x, y]);
    }
}
