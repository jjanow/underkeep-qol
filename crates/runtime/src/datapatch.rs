//! Data patches in memory (PLAN §3): the game's definition documents are
//! changed as the game parses them, never on disk.
//!
//! A detour of `DefTable_Parse(data, size, name)` (pattern in
//! `patterns/core.toml`), the function every DfT document goes through,
//! looks the resource name up in the [`PatchSet`] built at startup. When it
//! has patches, the document is parsed with the `dft` crate, each patch is
//! applied in turn ([`dft::patch::Patch`]), and the original is called with
//! the re-serialized bytes. The patched buffer is kept for the life of the
//! process (whether the game's parsed table points into its source buffer
//! is not known), and reused when the same input comes again, so reloading
//! the definitions doesn't grow memory.
//!
//! Patches come from the active mods' [`crate::host::Mod::patches`] (in load
//! order), then the player's `uk_qol/patches/*.toml` (by file name). The set
//! is fixed at startup, since the game parses its definitions once during
//! init: turning a mod on or off changes its patches from the next launch.
//!
//! # Names
//! A patch's `file` is the resource name the loader passes, as in the
//! include list of `def/def.txt`: `<folder>/<name>.txt`, stored in `base/` as
//! `<folder>_<name>.txt.rfile`. So `base/def_def_skill.txt.rfile` is
//! `def/def_skill.txt` and `base/text_text_common_en.txt.rfile` is
//! `text/text_common_en.txt` (checked in game, 1.0.5). Setting
//! `log_defs = true` under `[runtime]` in `uk_qol/config.toml` logs every
//! name the game parses.
//!
//! Saves and the player profile go through the same function; patches for
//! them are refused ([`is_protected`]).
//!
//! # Errors
//! A patch file that doesn't parse, or names a protected document, is logged
//! and dropped at startup. A patch whose ops fail on the document is logged
//! and skipped; the others still apply. If the document itself doesn't parse,
//! or the runtime's code panics, the game gets the original bytes.

use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

use dft::Doc;
use dft::patch::Patch;

use crate::game;
use crate::guard;
use crate::hook::HookSet;

/// [`guard`] id of the data-patch hook's own code.
pub const GUARD_ID: &str = "core-patch";

/// Documents patches may not touch: the save files (`save_*`) and the player
/// profile, which go through the same parser.
pub fn is_protected(file: &str) -> bool {
    let f = file.to_ascii_lowercase();
    f.starts_with("save") || f.starts_with("player_profile")
}

/// One loaded patch and where it came from (for log lines).
#[derive(Debug, Clone)]
pub struct Source {
    /// `mod <id>: <file name>` or the user file's path.
    pub origin: String,
    pub patch: Patch,
}

/// Every patch, by the document it changes.
#[derive(Debug, Default)]
pub struct PatchSet {
    pub by_file: BTreeMap<String, Vec<Source>>,
}

impl PatchSet {
    /// Builds the set from the mods' patches (mod id, file name, text) and the
    /// `*.toml` files in `user_dir` (sorted by name), logging every patch
    /// that's dropped.
    pub fn collect(
        mod_patches: &[(String, &str, &str)],
        user_dir: Option<&Path>,
        log: fn(&str),
    ) -> PatchSet {
        let mut texts: Vec<(String, Result<String, String>)> = mod_patches
            .iter()
            .map(|(id, name, text)| (format!("mod {id}: {name}"), Ok((*text).to_owned())))
            .collect();
        if let Some(dir) = user_dir {
            match std::fs::read_dir(dir) {
                Ok(rd) => {
                    let mut files: Vec<_> = rd
                        .filter_map(|e| e.ok().map(|e| e.path()))
                        .filter(|p| p.extension().is_some_and(|x| x == "toml"))
                        .collect();
                    files.sort();
                    for p in files {
                        let text = std::fs::read_to_string(&p).map_err(|e| e.to_string());
                        texts.push((p.display().to_string(), text));
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => log(&format!("data patches: can't list {}: {e}", dir.display())),
            }
        }
        let mut set = PatchSet::default();
        for (origin, text) in texts {
            let patch = match text
                .map_err(|e| format!("can't read: {e}"))
                .and_then(|t| Patch::parse(&t).map_err(|e| e.to_string()))
            {
                Ok(p) => p,
                Err(e) => {
                    log(&format!("data patches: {origin}: {e}; skipped"));
                    continue;
                }
            };
            if is_protected(&patch.file) {
                log(&format!(
                    "data patches: {origin}: `{}` is a save or profile, which patches may not change; skipped",
                    patch.file
                ));
                continue;
            }
            set.by_file
                .entry(patch.file.clone())
                .or_default()
                .push(Source { origin, patch });
        }
        set
    }

    pub fn is_empty(&self) -> bool {
        self.by_file.is_empty()
    }

    /// Number of patches.
    pub fn len(&self) -> usize {
        self.by_file.values().map(Vec::len).sum()
    }

    /// Applies the patches for `file` to `data`. `None` if there are none for
    /// it; otherwise the new bytes, or an error if the document doesn't
    /// parse. A patch that fails is logged and skipped.
    pub fn apply(&self, file: &str, data: &[u8], log: fn(&str)) -> Option<Result<Vec<u8>, String>> {
        let sources = self.by_file.get(file)?;
        let mut doc = match Doc::parse(data) {
            Ok(d) => d,
            Err(e) => return Some(Err(e.to_string())),
        };
        let mut applied = 0;
        for s in sources {
            match s.patch.apply(&mut doc) {
                Ok(()) => applied += 1,
                Err(e) => log(&format!(
                    "data patches: {}: {file}: {e}; this patch is skipped",
                    s.origin
                )),
            }
        }
        log(&format!(
            "data patches: {file}: applied {applied} of {} patches",
            sources.len()
        ));
        Some(Ok(doc.to_bytes()))
    }
}

// --- The hook -----------------------------------------------------------------

static PATCHES: OnceLock<PatchSet> = OnceLock::new();
static ORIG_PARSE: AtomicUsize = AtomicUsize::new(0);
static LOG_DEFS: AtomicBool = AtomicBool::new(false);
static FIRST_CALL: AtomicBool = AtomicBool::new(true);

/// A patched document kept for the process lifetime.
struct Cached {
    input_hash: u64,
    input_len: usize,
    out: &'static [u8],
}

/// Patched outputs per name, and the names already logged (`log_defs`).
static CACHE: Mutex<BTreeMap<String, Vec<Cached>>> = Mutex::new(BTreeMap::new());
static SEEN: Mutex<Vec<String>> = Mutex::new(Vec::new());

type ParseFn = extern "system" fn(*const u8, i32, usize) -> usize;

/// Sets the patch set and registers the `DefTable_Parse` detour on `hooks`.
/// `log_defs` logs each document name the game parses.
pub fn install(
    hooks: &mut HookSet,
    patterns: &BTreeMap<String, u32>,
    base: usize,
    set: PatchSet,
    log_defs: bool,
) {
    let n = set.len();
    let files = set.by_file.len();
    let _ = PATCHES.set(set);
    LOG_DEFS.store(log_defs, Ordering::Relaxed);
    let Some(&rva) = patterns.get("deftable_parse") else {
        crate::log("data patches: deftable_parse not found; data patches are off");
        return;
    };
    // SAFETY: the pattern locates DefTable_Parse; `parse` has its signature.
    match unsafe { hooks.detour(base + rva as usize, parse as ParseFn as usize) } {
        Ok(tramp) => {
            ORIG_PARSE.store(tramp, Ordering::Release);
            crate::log(&format!(
                "data patches: hook registered; {n} patches for {files} documents"
            ));
        }
        Err(e) => crate::log(&format!("data patches: can't hook DefTable_Parse: {e}")),
    }
}

fn hash(data: &[u8]) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    data.hash(&mut h);
    h.finish()
}

extern "system" fn parse(data: *const u8, size: i32, name: usize) -> usize {
    // SAFETY: the trampoline of DefTable_Parse, set before the hook went in.
    let orig: ParseFn = unsafe { std::mem::transmute(ORIG_PARSE.load(Ordering::Acquire)) };
    let swapped = guard::call(GUARD_ID, || patched(data, size, name)).flatten();
    match swapped {
        Some(buf) => orig(buf.as_ptr(), buf.len() as i32, name),
        None => orig(data, size, name),
    }
}

/// The patched buffer to parse instead of `data`, if there is one.
fn patched(data: *const u8, size: i32, name: usize) -> Option<&'static [u8]> {
    let first = FIRST_CALL.swap(false, Ordering::Relaxed);
    let log_defs = LOG_DEFS.load(Ordering::Relaxed);
    let set = PATCHES.get()?;
    if set.is_empty() && !first && !log_defs {
        return None;
    }
    // SAFETY: the game's `const String*` argument; reads are checked.
    let file = unsafe { game::read_string(name) }?;
    if first {
        crate::log(&format!("data patches: first document parsed: {file}"));
    }
    if log_defs {
        let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
        if !seen.contains(&file) {
            crate::log(&format!("data patches: game parses {file} ({size} bytes)"));
            seen.push(file.clone());
        }
    }
    if !set.by_file.contains_key(&file) || is_protected(&file) {
        return None;
    }
    let len = usize::try_from(size).ok().filter(|&l| l >= 4)?;
    // SAFETY: the game's buffer of `size` bytes; read is checked.
    let input = unsafe { game::read_bytes(data as usize, len) }?;
    if !input.starts_with(b"\xFFDfT") {
        crate::log(&format!(
            "data patches: {file} is not in the binary DfT form; left unpatched"
        ));
        return None;
    }
    let h = hash(&input);
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let entries = cache.entry(file.clone()).or_default();
    if let Some(c) = entries
        .iter()
        .find(|c| c.input_hash == h && c.input_len == len)
    {
        return Some(c.out);
    }
    match set.apply(&file, &input, crate::log)? {
        Ok(out) => {
            // Kept for the life of the process (see the module docs).
            let out: &'static [u8] = Box::leak(out.into_boxed_slice());
            entries.push(Cached {
                input_hash: h,
                input_len: len,
                out,
            });
            Some(out)
        }
        Err(e) => {
            crate::log(&format!(
                "data patches: {file} doesn't parse ({e}); left unpatched"
            ));
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nolog(_: &str) {}

    /// A hand-built DfT document: `title = "Old"`, `pos = [10, 20]`.
    fn doc() -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(b"\xFFDfT");
        b.extend_from_slice(&1u32.to_le_bytes()); // version
        b.extend_from_slice(&3u32.to_le_bytes()); // strings
        b.extend_from_slice(b"\x05title\x03pos\x03Old");
        b.extend_from_slice(&2u32.to_le_bytes()); // root entries
        b.extend_from_slice(b"\x00\x00\x03\x02"); // title = "Old"
        b.extend_from_slice(b"\x00\x01\x04"); // pos = table
        b.extend_from_slice(&2u32.to_le_bytes());
        b.extend_from_slice(b"\x01\x00\x01");
        b.extend_from_slice(&10i32.to_le_bytes());
        b.extend_from_slice(b"\x01\x01\x01");
        b.extend_from_slice(&20i32.to_le_bytes());
        b
    }

    fn set_of(patches: &[(&str, &str)]) -> PatchSet {
        let owned: Vec<(String, &str, &str)> = patches
            .iter()
            .map(|&(n, t)| ("test".to_owned(), n, t))
            .collect();
        PatchSet::collect(&owned, None, nolog)
    }

    fn value_of(bytes: &[u8], path: &str) -> String {
        // Read back through a `set` to itself would be indirect; inspect the doc.
        let d = Doc::parse(bytes).unwrap();
        match path {
            "title" => match &d.root[0].value {
                dft::Value::Str(i) => d.str(*i),
                v => format!("{v:?}"),
            },
            "pos[1]" => match &d.root[1].value {
                dft::Value::Table(t) => format!("{:?}", t[1].value),
                v => format!("{v:?}"),
            },
            _ => unreachable!(),
        }
    }

    #[test]
    fn applies_in_order_and_skips_failures() {
        assert!(Doc::parse(&doc()).is_ok());
        let set = set_of(&[
            (
                "a.toml",
                "file = \"def/test.txt\"\n[[op]]\nset = \"title\"\nvalue = \"New\"",
            ),
            (
                "bad-op.toml",
                "file = \"def/test.txt\"\n[[op]]\nremove = \"nope\"",
            ),
            (
                "b.toml",
                "file = \"def/test.txt\"\n[[op]]\nset = \"pos[1]\"\nvalue = 99",
            ),
            ("unparsable.toml", "file = 3"),
            ("other.toml", "file = \"def/other.txt\"\n"),
        ]);
        assert_eq!(set.len(), 4);
        assert_eq!(set.by_file["def/test.txt"].len(), 3);
        assert_eq!(set.by_file["def/test.txt"][0].origin, "mod test: a.toml");

        assert!(set.apply("def/none.txt", &doc(), nolog).is_none());
        let out = set.apply("def/test.txt", &doc(), nolog).unwrap().unwrap();
        assert_eq!(value_of(&out, "title"), "New");
        assert_eq!(value_of(&out, "pos[1]"), "Int(99)");
        // A document that doesn't parse is an error (the game gets the original).
        assert!(
            set.apply("def/test.txt", b"text def", nolog)
                .unwrap()
                .is_err()
        );
    }

    #[test]
    fn protected_documents_are_refused() {
        assert!(is_protected("save_1"));
        assert!(is_protected("save_auto"));
        assert!(is_protected("player_profile"));
        assert!(!is_protected("def/def_skill.txt"));
        let set = set_of(&[
            ("s.toml", "file = \"save_1\"\n"),
            ("p.toml", "file = \"player_profile\"\n"),
        ]);
        assert!(set.is_empty());
    }

    #[test]
    fn user_files_follow_mod_patches_sorted() {
        let dir = crate::config::tests::temp_dir("patches");
        std::fs::write(
            dir.join("b.toml"),
            "file = \"def/test.txt\"\n[[op]]\nset = \"title\"\nvalue = \"B\"",
        )
        .unwrap();
        std::fs::write(
            dir.join("a.toml"),
            "file = \"def/test.txt\"\n[[op]]\nset = \"title\"\nvalue = \"A\"",
        )
        .unwrap();
        std::fs::write(dir.join("notes.txt"), "ignored").unwrap();
        let mods = vec![(
            "m".to_owned(),
            "x.toml",
            "file = \"def/test.txt\"\n[[op]]\nset = \"title\"\nvalue = \"M\"",
        )];
        let set = PatchSet::collect(&mods, Some(&dir), nolog);
        let origins: Vec<&str> = set.by_file["def/test.txt"]
            .iter()
            .map(|s| s.origin.as_str())
            .collect();
        assert_eq!(origins.len(), 3);
        assert_eq!(origins[0], "mod m: x.toml");
        assert!(origins[1].ends_with("a.toml") && origins[2].ends_with("b.toml"));
        // The last one wins.
        let out = set.apply("def/test.txt", &doc(), nolog).unwrap().unwrap();
        assert_eq!(value_of(&out, "title"), "B");
        // A missing folder is fine.
        let none = PatchSet::collect(&[], Some(&dir.join("missing")), nolog);
        assert!(none.is_empty());
    }
}
