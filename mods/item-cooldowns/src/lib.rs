//! `item-cooldowns`: the cooldown left on an item's ability, in the item's
//! tooltip (README, "Mod designs").
//!
//! The game's rules (1.0.6): an item with an ability (a belt that heals, a
//! staff with a spell) carries one ability state at `Item+0xd8`, laid out
//! like a hero's skill state: +0x00 the `SkillDef*`, +0x0c its level, +0x10
//! the turns of cooldown left (`Item+0xe8`). Using the ability sets it to the
//! def's cooldown (`SkillDef+0x4c`); every turn the game takes one off for
//! each party-inventory item and each equipped item alike, and equipping
//! doesn't touch it. The game's tooltip only shows the def's full cooldown.
//!
//! The item tooltip builder is detoured (the outer item tooltip function
//! belongs to learned-recipes, and a function takes one detour). After the
//! game writes the text, if the builder was called for a real item (its
//! params at `Item+0x120` and its ability at `Item+0xd8`, 0x48 apart) whose
//! ability has a cooldown, the mod appends one line: the turns left, or
//! Ready. Ready is shown only for items the party has (inventory or a hero's
//! equipment), so shop and chest items don't get it; an item on cooldown
//! gets its number wherever it is shown (a weapon in weapon-swap's slot II
//! too). The texts are this mod's text keys (`patches/`), in the game's
//! language.
//!
//! Nothing is written to the game's state or to saves.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use uk_qol_runtime::game;
use uk_qol_runtime::guard;
use uk_qol_runtime::host::{LoadCtx, Mod};
use uk_qol_runtime::savehook;

const ID: &str = "item-cooldowns";
const SHOW_READY_KEY: &str = "show_ready";

/// Text keys added by `patches/text_common_*.toml`.
const KEY_LEFT: &str = "uk_qol_item_cooldown_left";
const KEY_READY: &str = "uk_qol_item_cooldown_ready";
/// Used when a key is missing (the mod was turned on after the game read
/// its texts, so the patches weren't applied).
const FALLBACK_LEFT: &str = "{color_text}{a:left}Cooldown left{a:right}{color_msg_danger}%1\n";
const FALLBACK_READY: &str = "{color_text}{a:left}Cooldown left{a:right}{color_msg_success}Ready\n";

// Game structures (1.0.6).
/// The builder gets the item's params and ability at these offsets.
const ITEM_PARAMS: usize = 0x120;
const ITEM_SKILL: usize = 0xd8;
/// ItemParams: the "has an ability" flag (i32; `Item+0x598`, what
/// `Hero::CanCastSkill` checks).
const PARAMS_HAS_SKILL: usize = 0x478;
/// Ability state: def, level, turns left.
const SKILL_DEF: usize = 0x00;
const SKILL_LEVEL: usize = 0x0c;
const SKILL_COOLDOWN: usize = 0x10;
/// SkillDef: the full cooldown in turns.
const DEF_COOLDOWN: usize = 0x4c;
/// Inventory: +8 count, +0x10 entries of 0x18 bytes, the Item* first.
const INV_COUNT: usize = 0x08;
const INV_ENTRIES: usize = 0x10;
const INV_ENTRY_SIZE: usize = 0x18;
/// Hero: 11 equipment slots of Item*.
const HERO_EQUIP: usize = 0x210;
const EQUIP_SLOTS: usize = 11;
/// No cooldown in the game is anywhere near this; larger values are junk.
const COOLDOWN_MAX: i32 = 100_000;

static SHOW_READY: AtomicBool = AtomicBool::new(true);

static ORIG_BUILDER: AtomicUsize = AtomicUsize::new(0);

/// Resolved addresses, set in `on_load` before the hook is enabled.
static INVENTORY: AtomicUsize = AtomicUsize::new(0);
static TEXT_GET: AtomicUsize = AtomicUsize::new(0);
static CONCAT: AtomicUsize = AtomicUsize::new(0);
static ASSIGN: AtomicUsize = AtomicUsize::new(0);
static FREE: AtomicUsize = AtomicUsize::new(0);

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
    fn literal(s: &[u8]) -> GString {
        let text = s.strip_suffix(b"\0").unwrap_or(s);
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

/// An item's ability cooldown: turns left and the full cooldown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Cooldown {
    left: i32,
    full: i32,
}

/// The item whose params and ability the builder got, if they are a real
/// item's: both inside one item, 0x48 apart.
fn item_of(params: usize, skill: usize) -> Option<usize> {
    (skill >= ITEM_SKILL && params == skill - ITEM_SKILL + ITEM_PARAMS).then(|| skill - ITEM_SKILL)
}

/// The cooldown of `item`'s ability, if it has one with a cooldown.
///
/// # Safety
/// `item` must be a live Item (checked reads).
unsafe fn cooldown_of(item: usize) -> Option<Cooldown> {
    // SAFETY: checked reads of a live item and its ability's def.
    unsafe {
        if game::read::<i32>(item + ITEM_PARAMS + PARAMS_HAS_SKILL)? == 0 {
            return None;
        }
        let skill = item + ITEM_SKILL;
        let def = game::read::<usize>(skill + SKILL_DEF)?;
        if def == 0 {
            return None;
        }
        let level = game::read::<i32>(skill + SKILL_LEVEL)?;
        // As the game's skill tooltip: no level, no cooldown.
        let full = if level >= 1 {
            game::read::<i32>(def + DEF_COOLDOWN)?
        } else {
            0
        };
        let left = game::read::<i32>(skill + SKILL_COOLDOWN)?;
        if !(0..=COOLDOWN_MAX).contains(&full) || !(0..=COOLDOWN_MAX).contains(&left) {
            return None;
        }
        (full > 0 || left > 0).then_some(Cooldown { left, full })
    }
}

/// Whether `item` is in the party inventory at `inventory` or equipped by
/// one of the heroes of `party` (0 for none).
///
/// # Safety
/// `inventory` and `party` must be the game's (or buffers laid out like
/// them); checked reads.
unsafe fn party_has(inventory: usize, party: usize, item: usize) -> bool {
    // SAFETY: checked reads.
    unsafe {
        let count = game::read::<i32>(inventory + INV_COUNT).unwrap_or(0);
        let entries = game::read::<usize>(inventory + INV_ENTRIES).unwrap_or(0);
        if count > 0 && count <= 10_000 && entries != 0 {
            let found = (0..count as usize)
                .any(|i| game::read::<usize>(entries + i * INV_ENTRY_SIZE) == Some(item));
            if found {
                return true;
            }
        }
        if party == 0 {
            return false;
        }
        (0..savehook::HERO_COUNT).any(|h| {
            let hero = party + savehook::PARTY_HEROES + h * savehook::HERO_SIZE;
            (0..EQUIP_SLOTS).any(|s| game::read::<usize>(hero + HERO_EQUIP + s * 8) == Some(item))
        })
    }
}

/// What the line says: `Some(turns left)`, `Some(0)` for Ready, or nothing.
fn line_for(cd: Cooldown, owned: bool, show_ready: bool) -> Option<i32> {
    if cd.left > 0 {
        Some(cd.left)
    } else if show_ready && owned && cd.full > 0 {
        Some(0)
    } else {
        None
    }
}

/// The line's text from its template: `%1` becomes `left`. NUL-terminated.
fn fill(template: &[u8], left: i32) -> Vec<u8> {
    let n = left.to_string();
    let mut out = Vec::with_capacity(template.len() + n.len() + 1);
    let mut rest = template;
    while let Some(i) = rest.windows(2).position(|w| w == b"%1") {
        out.extend_from_slice(&rest[..i]);
        out.extend_from_slice(n.as_bytes());
        rest = &rest[i + 2..];
    }
    out.extend_from_slice(rest);
    out.push(0);
    out
}

// --- Tooltip line -----------------------------------------------------------

type BuilderFn = unsafe extern "system" fn(
    usize,
    usize,
    usize,
    usize,
    usize,
    usize,
    usize,
    usize,
    usize,
) -> usize;
type TextGetFn = unsafe extern "system" fn(*mut GString, *const GString) -> *mut GString;
type ConcatFn =
    unsafe extern "system" fn(*mut GString, *const GString, *const GString) -> *mut GString;
type AssignFn = unsafe extern "system" fn(*mut GString, *const GString);
type FreeFn = unsafe extern "system" fn(*mut GString);

/// The template for `key` in the game's language, or `fallback`.
///
/// # Safety
/// Game thread, after `on_load`.
unsafe fn template(key: &str, fallback: &str) -> Vec<u8> {
    // SAFETY: the resolved game functions, with their signatures.
    unsafe {
        let text_get: TextGetFn = std::mem::transmute(TEXT_GET.load(Ordering::Relaxed));
        let free: FreeFn = std::mem::transmute(FREE.load(Ordering::Relaxed));
        let mut k = key.as_bytes().to_vec();
        k.push(0);
        let key_s = GString::literal(&k);
        let mut line = GString::empty();
        text_get(&mut line, &key_s);
        // A missing key comes back as the key itself (or empty).
        let got = string_bytes(&line as *const GString as usize)
            .filter(|s| !s.is_empty() && !s.windows(key.len()).any(|w| w == key.as_bytes()))
            .map(<[u8]>::to_vec);
        free(&mut line);
        got.unwrap_or_else(|| fallback.as_bytes().to_vec())
    }
}

/// Appends the line for `left` (0 = Ready) to the tooltip text `out`.
///
/// # Safety
/// Game thread; `out` is the String the builder just filled in.
unsafe fn append_line(out: usize, left: i32) {
    // SAFETY: the resolved game functions, with their signatures; `text`
    // outlives every String made over it.
    unsafe {
        let t = if left > 0 {
            template(KEY_LEFT, FALLBACK_LEFT)
        } else {
            template(KEY_READY, FALLBACK_READY)
        };
        let text = fill(&t, left);
        let concat: ConcatFn = std::mem::transmute(CONCAT.load(Ordering::Relaxed));
        let assign: AssignFn = std::mem::transmute(ASSIGN.load(Ordering::Relaxed));
        let free: FreeFn = std::mem::transmute(FREE.load(Ordering::Relaxed));
        let line = GString::literal(&text);
        let mut joined = GString::empty();
        concat(&mut joined, out as *const GString, &line);
        assign(out as *mut GString, &joined);
        free(&mut joined);
    }
}

/// The item tooltip builder (see `patterns.toml`).
#[allow(clippy::too_many_arguments)]
unsafe extern "system" fn item_tooltip_builder(
    out: usize,
    info: usize,
    params: usize,
    skill: usize,
    a5: usize,
    a6: usize,
    a7: usize,
    a8: usize,
    a9: usize,
) -> usize {
    // SAFETY: set before the hook was enabled.
    let orig: BuilderFn = unsafe { std::mem::transmute(ORIG_BUILDER.load(Ordering::Relaxed)) };
    // SAFETY: the original with the game's arguments.
    let r = unsafe { orig(out, info, params, skill, a5, a6, a7, a8, a9) };
    if r != 0 {
        guard::call(ID, || {
            let Some(item) = item_of(params, skill) else {
                return;
            };
            // SAFETY: the game passed a live item's params and ability;
            // checked reads; `r` is the tooltip String.
            unsafe {
                let Some(cd) = cooldown_of(item) else {
                    return;
                };
                let show_ready = SHOW_READY.load(Ordering::Relaxed);
                let owned = cd.left <= 0
                    && show_ready
                    && party_has(
                        INVENTORY.load(Ordering::Relaxed),
                        savehook::party().unwrap_or(0),
                        item,
                    );
                if let Some(left) = line_for(cd, owned, show_ready) {
                    append_line(r, left);
                }
            }
        });
    }
    r
}

// --- Mod --------------------------------------------------------------------

pub struct ItemCooldowns;

impl Mod for ItemCooldowns {
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
        let builder = at("item_tooltip_builder")?;
        for (slot, name) in [
            (&INVENTORY, "party_inventory"),
            (&TEXT_GET, "text_get"),
            (&CONCAT, "string_concat"),
            (&ASSIGN, "string_assign"),
            (&FREE, "string_free"),
        ] {
            slot.store(at(name)?, Ordering::Relaxed);
        }
        self.on_settings(cx.settings);
        // SAFETY: the target is a function start (verified pattern); the
        // detour has its signature and ABI. The trampoline is stored before
        // the host enables the hook.
        unsafe {
            ORIG_BUILDER.store(
                cx.hooks
                    .detour(builder, item_tooltip_builder as BuilderFn as usize)
                    .map_err(|e| e.to_string())?,
                Ordering::Relaxed,
            );
        }
        Ok(())
    }

    fn on_settings(&mut self, settings: &toml::Table) {
        SHOW_READY.store(bool_setting(settings, SHOW_READY_KEY), Ordering::Relaxed);
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
    Box::new(ItemCooldowns)
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
        assert!(bool_setting(&manifest.defaults(), SHOW_READY_KEY));
        let pats = PatternFile::parse(m.patterns()).unwrap();
        assert_eq!(pats.patterns.len(), 6);
        assert_eq!(m.patches().len(), 7);
        for (name, text) in m.patches() {
            let p = dft::patch::Patch::parse(text).unwrap_or_else(|e| panic!("{name}: {e}"));
            let lang = &name["text_common_".len()..name.len() - ".toml".len()];
            assert_eq!(p.file, format!("text/text_common_{lang}.txt"));
            for key in [KEY_LEFT, KEY_READY] {
                assert!(
                    text.contains(&format!("text_common_{lang}.{key}\"")),
                    "{name}: {key}"
                );
            }
            assert!(text.contains("%1"), "{name}: the number");
        }
    }

    /// Resolves every pattern against the game's exe when it's installed
    /// (skipped otherwise) and checks the addresses found in 1.0.6.
    #[test]
    fn patterns_resolve_on_installed_game() {
        use std::collections::BTreeMap;
        use uk_qol_runtime::host::resolve_pattern;
        use uk_qol_runtime::pattern::Image;
        let Some(home) = std::env::var_os("HOME") else {
            return;
        };
        let exe = std::path::PathBuf::from(home)
            .join(".local/share/Steam/steamapps/common/Underkeep/underkeep.exe");
        let Ok(file) = std::fs::read(&exe) else {
            return;
        };
        let build = uk_qol_runtime::build::detect_from_file(&exe)
            .unwrap()
            .sha256_hex();
        if build != "556f696519db3a948f1ef1f37e4fdc23293c330d00d93399380dd2c3606055ba" {
            return; // another game version
        }
        let mapped = uk_qol_runtime::pe::map_file(&file).unwrap();
        let img = Image::new(&mapped).unwrap();
        let expected: BTreeMap<&str, u32> = [
            ("item_tooltip_builder", 0x2495f0),
            ("party_inventory", 0x440308),
            ("text_get", 0xecd00),
            ("string_concat", 0x27de0),
            ("string_assign", 0x27a00),
            ("string_free", 0x3000),
        ]
        .into_iter()
        .collect();
        let pats = PatternFile::parse(include_str!("../patterns.toml")).unwrap();
        for p in &pats.patterns {
            let r = resolve_pattern(&img, p, Some(&build))
                .unwrap_or_else(|e| panic!("{}: {e}", p.name));
            assert_eq!(Some(&r.rva), expected.get(p.name.as_str()), "{}", p.name);
        }
    }

    #[test]
    fn patches_apply_to_a_text_document() {
        let mut doc = dft::Doc {
            version: 1,
            strings: Vec::new(),
            len_widths: Vec::new(),
            root: Vec::new(),
        };
        let table = doc.intern("text_common_en");
        let k = doc.intern("skill_stat_cooldown");
        let v = doc.intern("Cooldown{a:right}{skill_cooldown}");
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
        assert_eq!(names, ["skill_stat_cooldown", KEY_LEFT, KEY_READY]);
    }

    #[test]
    fn fill_puts_the_number_in() {
        assert_eq!(fill(b"left{a:right}%1\n", 37), b"left{a:right}37\n\0");
        assert_eq!(fill(b"Ready\n", 0), b"Ready\n\0");
        assert_eq!(fill(FALLBACK_LEFT.as_bytes(), 5).last(), Some(&0));
        let s = GString::literal(&fill(b"ab%1", 12));
        assert_eq!(s.len, 4);
    }

    #[test]
    fn only_real_items_count() {
        assert_eq!(item_of(0x1000 + 0x120, 0x1000 + 0xd8), Some(0x1000));
        // The definition caller: both on its stack, 0xb0 apart.
        assert_eq!(item_of(0x5000, 0x5000 - 0xb0), None);
        assert_eq!(item_of(0x120, 0), None);
    }

    #[test]
    fn what_the_line_says() {
        let on = Cooldown {
            left: 37,
            full: 100,
        };
        let ready = Cooldown { left: 0, full: 100 };
        assert_eq!(line_for(on, false, true), Some(37), "on cooldown: anywhere");
        assert_eq!(line_for(ready, true, true), Some(0));
        assert_eq!(line_for(ready, false, true), None, "shops and chests");
        assert_eq!(line_for(ready, true, false), None, "setting off");
        assert_eq!(line_for(Cooldown { left: 0, full: 0 }, true, true), None);
    }

    /// A fake item with an ability: params flag, state, def.
    fn fake_item(has: i32, level: i32, left: i32, def: &[u8]) -> Vec<u8> {
        let mut item = vec![0u8; ITEM_PARAMS + PARAMS_HAS_SKILL + 4];
        let p = ITEM_PARAMS + PARAMS_HAS_SKILL;
        item[p..p + 4].copy_from_slice(&has.to_le_bytes());
        let s = ITEM_SKILL;
        item[s..s + 8].copy_from_slice(&(def.as_ptr() as usize).to_le_bytes());
        item[s + SKILL_LEVEL..s + SKILL_LEVEL + 4].copy_from_slice(&level.to_le_bytes());
        item[s + SKILL_COOLDOWN..s + SKILL_COOLDOWN + 4].copy_from_slice(&left.to_le_bytes());
        item
    }

    #[test]
    fn cooldowns_and_ownership() {
        let mut def = vec![0u8; DEF_COOLDOWN + 4];
        def[DEF_COOLDOWN..].copy_from_slice(&100i32.to_le_bytes());
        let belt = fake_item(1, 1, 37, &def);
        let plain = fake_item(0, 1, 0, &def);
        let b = belt.as_ptr() as usize;
        // SAFETY: local buffers laid out like the game's.
        unsafe {
            assert_eq!(
                cooldown_of(b),
                Some(Cooldown {
                    left: 37,
                    full: 100
                })
            );
            assert_eq!(cooldown_of(plain.as_ptr() as usize), None);
            let junk = fake_item(1, 1, -5, &def);
            assert_eq!(cooldown_of(junk.as_ptr() as usize), None);

            // Inventory with the belt second; a party with nothing equipped.
            let other = 0x1234usize;
            let mut entries = [0u8; 2 * INV_ENTRY_SIZE];
            entries[..8].copy_from_slice(&other.to_le_bytes());
            entries[INV_ENTRY_SIZE..INV_ENTRY_SIZE + 8].copy_from_slice(&b.to_le_bytes());
            let mut inv = [0u8; INV_ENTRIES + 8];
            inv[INV_COUNT..INV_COUNT + 4].copy_from_slice(&2i32.to_le_bytes());
            inv[INV_ENTRIES..].copy_from_slice(&(entries.as_ptr() as usize).to_le_bytes());
            let mut party =
                vec![0u8; savehook::PARTY_HEROES + savehook::HERO_COUNT * savehook::HERO_SIZE];
            let i = inv.as_ptr() as usize;
            assert!(party_has(i, party.as_ptr() as usize, b));
            assert!(!party_has(i, party.as_ptr() as usize, 0x9999));

            // Equipped: hero 2's belt slot (8).
            inv[INV_COUNT..INV_COUNT + 4].copy_from_slice(&0i32.to_le_bytes());
            assert!(!party_has(i, party.as_ptr() as usize, b));
            let at = savehook::PARTY_HEROES + 2 * savehook::HERO_SIZE + HERO_EQUIP + 8 * 8;
            party[at..at + 8].copy_from_slice(&b.to_le_bytes());
            assert!(party_has(i, party.as_ptr() as usize, b));
            assert!(!party_has(i, 0, b));
        }
    }
}
