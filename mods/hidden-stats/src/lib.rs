//! `hidden-stats`: the stats the game keeps on each hero but doesn't show,
//! as extra rows on the hero's stats page (README, "Mod designs").
//!
//! The stats page (the character sheet's stats tab) rebuilds its list of
//! rows every frame (`stats_rebuild`, see `patterns.toml` and [`rows`]);
//! the page then draws each row as its text key's text with the value in
//! it, and the selected row's description from the key + `_desc` text.
//! The mod detours the rebuild and, after the game's rows are in, adds its
//! own at the end of the Hero, Offense and Defense sections: text keys of
//! its own (added to every language's text table by data patches,
//! [`text`]) and values read from the hero ([`stats`]). So the rows look,
//! select, scroll and describe themselves like the game's.
//!
//! Rare stats (an element bonus, an immunity) only show while the hero has
//! them, unless the setting `show_zero` is on. Nothing in the game's state,
//! files or saves changes: the rows only live in the page's list, which the
//! game empties and rebuilds every frame.

mod rows;
mod stats;
mod text;

use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use rows::{GString, Mem};
use stats::{Hero, MELEE_HIT_MA, STATS, Stat};
use uk_qol_runtime::game;
use uk_qol_runtime::guard;
use uk_qol_runtime::host::{LoadCtx, Mod};

const ID: &str = "hidden-stats";
const SHOW_ZERO: &str = "show_zero";
/// The mod whose formula changes the melee hit chance in attacks.
const MELEE_ACCURACY: &str = "melee-accuracy";

type RebuildFn = unsafe extern "system" fn(usize, usize);
type PushFn = unsafe extern "system" fn(usize, *mut u8);
type ConcatFn =
    unsafe extern "system" fn(*mut GString, *const GString, *const GString) -> *mut GString;
type ExistsFn = unsafe extern "system" fn(*const GString) -> u8;

static ORIG_REBUILD: AtomicUsize = AtomicUsize::new(0);
static PUSH: AtomicUsize = AtomicUsize::new(0);
static CONCAT: AtomicUsize = AtomicUsize::new(0);
static EXISTS: AtomicUsize = AtomicUsize::new(0);
static SHOW_ZEROS: AtomicBool = AtomicBool::new(false);
/// Whether the mod's texts are loaded: 0 not checked yet, 1 yes, 2 no.
static TEXTS: AtomicU8 = AtomicU8::new(0);

fn log(msg: &str) {
    uk_qol_runtime::log(&format!("{ID}: {msg}"));
}

/// The row keys, `uk_qol_hs_<key>` (static: the rows point at them).
fn row_key(key: &str) -> &'static str {
    static KEYS: OnceLock<Vec<(&'static str, &'static str)>> = OnceLock::new();
    let keys = KEYS.get_or_init(|| {
        STATS
            .iter()
            .map(|s| s.key)
            .chain([MELEE_HIT_MA])
            .map(|k| {
                let full: &'static str =
                    Box::leak(format!("{}{k}\0", text::PREFIX).into_boxed_str());
                (k, full.trim_end_matches('\0'))
            })
            .collect()
    });
    keys.iter().find(|(k, _)| *k == key).map_or("", |(_, f)| f)
}

/// Whether melee-accuracy is turned on in the mod config (re-read every few
/// seconds).
fn melee_accuracy_on() -> bool {
    static CACHE: Mutex<Option<(Instant, bool)>> = Mutex::new(None);
    let mut c = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((at, on)) = *c
        && at.elapsed() < Duration::from_secs(2)
    {
        return on;
    }
    let on = uk_qol_runtime::data_dir()
        .map(|d| uk_qol_runtime::config::config_path(&d))
        .and_then(|p| uk_qol_runtime::config::Config::load(&p).ok())
        // Its manifest has it off by default.
        .is_some_and(|cfg| cfg.is_enabled(MELEE_ACCURACY, false));
    *c = Some((Instant::now(), on));
    on
}

/// The game's memory, through the runtime's checked reads.
struct GameMem;

impl Mem for GameMem {
    fn read_i32(&self, at: usize) -> Option<i32> {
        // SAFETY: checked read.
        unsafe { game::read(at) }
    }
    fn read_usize(&self, at: usize) -> Option<usize> {
        // SAFETY: checked read.
        unsafe { game::read(at) }
    }
    fn read_bytes(&self, at: usize, len: usize) -> Option<Vec<u8>> {
        // SAFETY: checked read.
        unsafe { game::read_bytes(at, len) }
    }
    fn write_bytes(&self, at: usize, bytes: &[u8]) {
        // SAFETY: only called on the row vector's data, just read whole
        // (so readable); the game thread owns it and nothing else runs on
        // it during the detour.
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), at as *mut u8, bytes.len()) };
    }
}

/// The hero, through checked reads.
struct GameHero(usize);

impl Hero for GameHero {
    fn i32(&self, off: usize) -> Option<i32> {
        // SAFETY: checked read.
        unsafe { game::read(self.0 + off) }
    }
    fn u8(&self, off: usize) -> Option<u8> {
        // SAFETY: checked read.
        unsafe { game::read(self.0 + off) }
    }
    fn at_i32(&self, addr: usize) -> Option<i32> {
        // SAFETY: checked read.
        unsafe { game::read(addr) }
    }
    fn at_u8(&self, addr: usize) -> Option<u8> {
        // SAFETY: checked read.
        unsafe { game::read(addr) }
    }
    fn at_ptr(&self, addr: usize) -> Option<usize> {
        // SAFETY: checked read.
        unsafe { game::read(addr) }
    }
    fn base(&self) -> usize {
        self.0
    }
}

/// A counted game String holding `text` (the game frees it with the row).
fn game_string(text: &str) -> Option<GString> {
    let concat = CONCAT.load(Ordering::Relaxed);
    if concat == 0 || text.is_empty() {
        return None;
    }
    // Concat only copies when both parts are non-empty (else it shares the
    // one part, which is a literal of ours here): split off the last
    // character, or pad a one-character value with a leading space (right
    // aligned, it doesn't show).
    let owned;
    let t = if text.len() == 1 {
        owned = format!(" {text}");
        owned.as_str()
    } else {
        text
    };
    let cut = t.char_indices().last().map_or(0, |(i, _)| i);
    let (a, b) = (GString::literal(&t[..cut]), GString::literal(&t[cut..]));
    let mut out = GString {
        buf: 0,
        data: 0,
        len: 0,
        pad: 0,
    };
    // SAFETY: the game's String_Concat with two valid strings; it reads
    // them during the call only and writes `out`.
    unsafe {
        let f: ConcatFn = std::mem::transmute(concat);
        f(&mut out, &a, &b);
    }
    (out.buf != 0 && out.len as usize == t.len()).then_some(out)
}

/// Whether the mod's texts are in the game's tables (else the rows would
/// show raw keys: the mod was turned on after the game read its texts).
fn texts_loaded() -> bool {
    match TEXTS.load(Ordering::Relaxed) {
        1 => return true,
        2 => return false,
        _ => {}
    }
    let exists = EXISTS.load(Ordering::Relaxed);
    let ok = if exists == 0 {
        true
    } else {
        let key = GString::literal(row_key("melee_hit"));
        // SAFETY: the game's Text_Exists with a valid string.
        unsafe {
            let f: ExistsFn = std::mem::transmute(exists);
            f(&key) != 0
        }
    };
    if !ok {
        log(
            "its texts aren't loaded (turned on after the game started); restart the game to see the rows",
        );
    }
    TEXTS.store(if ok { 1 } else { 2 }, Ordering::Relaxed);
    ok
}

/// The rows to add for `hero`: (stat, value text), in section order.
fn readings(
    hero: usize,
    show_zero: bool,
    melee_mod: bool,
) -> Vec<(&'static Stat, &'static str, String)> {
    let h = GameHero(hero);
    let mut out = Vec::new();
    for s in STATS {
        let Some(r) = s.read(&h) else { continue };
        if !(r.shown || show_zero) {
            continue;
        }
        let key = if s.key == "melee_hit" && melee_mod {
            row_key(MELEE_HIT_MA)
        } else {
            row_key(s.key)
        };
        out.push((s, key, r.val.text()));
    }
    out
}

/// Adds the mod's rows to the page's freshly rebuilt list.
///
/// # Safety
/// `page` and `hero` are the rebuild's arguments, on the game thread.
unsafe fn add_rows(page: usize, hero: usize) -> Option<()> {
    let push = PUSH.load(Ordering::Relaxed);
    if push == 0 || hero == 0 || !texts_loaded() {
        return None;
    }
    let m = GameMem;
    let vec = page + rows::PAGE_ROWS;
    let l = rows::layout(&m, vec)?;
    let template = l.template.as_ref()?;
    let list = readings(
        hero,
        SHOW_ZEROS.load(Ordering::Relaxed),
        melee_accuracy_on(),
    );
    let mut counts = [0usize; 3];
    for (s, key, value) in &list {
        let Some(v) = game_string(value) else {
            continue;
        };
        let mut row = rows::make_row(template, GString::literal(key), v);
        // SAFETY: the game's push_back on the page's row vector with a row
        // laid out like its own; it moves the row's strings in.
        unsafe {
            let f: PushFn = std::mem::transmute(push);
            f(vec, row.as_mut_ptr());
        }
        counts[rows::section_index(s.section)] += 1;
    }
    let (n, data) = rows::header(&m, vec)?;
    if n != l.count + counts.iter().sum::<usize>() {
        return None;
    }
    rows::place(&m, data, &l, counts)
}

/// The stats page's row rebuild (see `patterns.toml`).
unsafe extern "system" fn stats_rebuild(page: usize, hero: usize) {
    // SAFETY: set before the hook was enabled; the game's arguments.
    unsafe {
        let orig: RebuildFn = std::mem::transmute(ORIG_REBUILD.load(Ordering::Relaxed));
        orig(page, hero);
    }
    guard::call(ID, || {
        // SAFETY: the rebuild's own arguments, on the game thread.
        unsafe { add_rows(page, hero) };
    });
}

pub struct HiddenStats;

fn apply(settings: &toml::Table) {
    SHOW_ZEROS.store(
        settings
            .get(SHOW_ZERO)
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        Ordering::Relaxed,
    );
}

impl Mod for HiddenStats {
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
        text::patches()
    }

    fn on_load(&mut self, cx: &mut LoadCtx) -> Result<(), String> {
        let at = |name: &str| {
            cx.patterns
                .get(name)
                .map(|&rva| cx.base + rva as usize)
                .ok_or_else(|| format!("pattern {name} not resolved"))
        };
        let rebuild = at("stats_rebuild")?;
        PUSH.store(at("row_push")?, Ordering::Relaxed);
        CONCAT.store(at("string_concat")?, Ordering::Relaxed);
        EXISTS.store(at("text_exists").unwrap_or(0), Ordering::Relaxed);
        TEXTS.store(0, Ordering::Relaxed);
        apply(cx.settings);
        // SAFETY: the target is the function's start (verified pattern); the
        // detour has its signature and ABI. The trampoline is stored before
        // the host enables the hook.
        unsafe {
            ORIG_REBUILD.store(
                cx.hooks
                    .detour(rebuild, stats_rebuild as RebuildFn as usize)
                    .map_err(|e| e.to_string())?,
                Ordering::Relaxed,
            );
        }
        Ok(())
    }

    fn on_settings(&mut self, settings: &toml::Table) {
        apply(settings);
    }
}

/// Constructor passed to `uk_qol_runtime::start`.
pub fn new() -> Box<dyn Mod> {
    Box::new(HiddenStats)
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
        assert_eq!(
            manifest.defaults().get(SHOW_ZERO).and_then(|v| v.as_bool()),
            Some(false)
        );
        let pats = PatternFile::parse(m.patterns()).unwrap();
        assert_eq!(pats.patterns.len(), 4);
        assert_eq!(m.patches().len(), text::LANGS.len());
    }

    #[test]
    fn row_keys() {
        assert_eq!(row_key("melee_hit"), "uk_qol_hs_melee_hit");
        assert_eq!(row_key(MELEE_HIT_MA), "uk_qol_hs_melee_hit_ma");
        assert_eq!(row_key("nope"), "");
        for s in STATS {
            assert!(!row_key(s.key).is_empty());
        }
    }
}
