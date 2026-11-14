//! Save hooks (PLAN §2): mod state follows the game's saves through
//! `<save name>.qol.toml` sidecars ([`crate::save`]). The game's own save
//! files are never read or written.
//!
//! Detours (patterns in `patterns/core.toml`), each calling the original
//! first, unguarded, and then doing its own work inside
//! [`guard::call`] (id [`GUARD_ID`]), so a panic there only turns the save
//! hooks off and never changes what the game does:
//!
//! - **SaveGame_Write** (every save: menu, quick save, autosave): if it
//!   succeeded, asks every active mod for its state ([`Host::save`]) on top of
//!   the state of the game in play, and writes the sidecar for that file name.
//! - **Game_LoadFromSlot**: if it succeeded, reads the save's sidecar (none =
//!   empty) and hands it to the mods ([`Host::load_save`]); a mod with no
//!   entry gets an empty state, so it resets.
//! - **Save_DeleteFile**: deletes that save's sidecar, if there is one.
//! - **Party_InitFromPartySetup** (a new game): mods get an empty state.
//!
//! "The state of the game in play" is the sidecar last loaded (or empty
//! after a new game). Saving starts from it, not from the target file's old
//! sidecar, so the state of a mod that is off now is carried into the new
//! save, and loading save_1 then saving as save_2 doesn't pick up save_2's
//! old state.
//!
//! # Where sidecars go
//! Locally: `<game folder>/uk_qol/saves/<account>/<save name>.qol.toml`
//! ([`crate::save::sidecar_path`]). The game writes its saves only through
//! `ISteamRemoteStorage` (Steam Cloud); writing sidecars the same way would
//! sync them too, but count against the game's cloud quota and put our files
//! in the player's cloud, so that's left as an open question (PLAN). Mod
//! state is therefore per computer.
//!
//! `<account>` is the Steam account id (the number in Steam's
//! `userdata/<account>`), read through the Steam API the game already loaded
//! (`SteamAPI_SteamUser_v023` and `SteamAPI_ISteamUser_GetSteamID` from
//! `steam_api64.dll`, low 32 bits of the SteamID) at the first save or load,
//! and `default` if that isn't available.
//!
//! # Heroes
//! A hero's `hero_id` in the save is only the party slot it was created in
//! (0..3), so it repeats when a hero is replaced. Mods key per-hero state by
//! [`hero_key`] (`"<hero_id>:<name>"`) and can check the class on load;
//! [`party_heroes`] lists the four heroes of the party.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::game;
use crate::guard;
use crate::hook::{HookError, HookSet};
use crate::host::{self, Host, ModSaveState};
use crate::save::{Sidecar, sidecar_path};

/// [`guard`] id of the save hooks' own code.
pub const GUARD_ID: &str = "core-save";

// --- Game structures (1.0.5) ------------------------------------------------

/// `SaveInfo+0x08`: the save's file name (`String`), e.g. `save_1`.
pub const SAVEINFO_FILE_NAME: usize = 0x08;
/// `g_party+0x10`: the first of the party's heroes.
pub const PARTY_HEROES: usize = 0x10;
/// Heroes in the party.
pub const HERO_COUNT: usize = 4;
/// Size of a hero struct (the stride of the party's hero array).
pub const HERO_SIZE: usize = 0x2b8;
/// `Hero+0x00`: slot in use (byte, non-zero).
pub const HERO_VALID: usize = 0x00;
/// `Hero+0x08`: `hero_id` (`i32`), the creation slot (0..3).
pub const HERO_ID: usize = 0x08;
/// `Hero+0x10`: the hero's name (`String`).
pub const HERO_NAME: usize = 0x10;
/// `Hero+0x94`: class (`i32`, see [`class_name`]).
pub const HERO_CLASS: usize = 0x94;

/// The game's class ids, in order.
pub const CLASSES: [&str; 7] = [
    "warrior",
    "paladin",
    "thief",
    "archer",
    "voidcaller",
    "wizard",
    "bloodmage",
];

/// The class name of a class id, `None` if unknown.
pub fn class_name(class: i32) -> Option<&'static str> {
    usize::try_from(class)
        .ok()
        .and_then(|c| CLASSES.get(c).copied())
}

/// The key of a hero's per-hero state in a sidecar: `"<hero_id>:<name>"`.
pub fn format_hero_key(hero_id: i32, name: &str) -> String {
    format!("{hero_id}:{name}")
}

/// [`format_hero_key`] of the hero struct at `hero`, read from game memory.
/// `None` if the struct or its name can't be read.
///
/// # Safety
/// `hero` should be a hero struct (from [`party_heroes`]); reads are checked
/// against unmapped memory but not against a wrong type. Off Windows (tests)
/// it must point to valid memory.
pub unsafe fn hero_key(hero: usize) -> Option<String> {
    // SAFETY: forwarded; each read is checked.
    unsafe {
        let id = game::read::<i32>(hero.checked_add(HERO_ID)?)?;
        let name = game::read_string(hero + HERO_NAME)?;
        Some(format_hero_key(id, &name))
    }
}

/// One hero of the party.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartyHero {
    /// Position in the party (0..3).
    pub index: usize,
    /// Address of the hero struct.
    pub ptr: usize,
    /// [`hero_key`].
    pub key: String,
    /// Class id (`Hero+0x94`; [`class_name`]).
    pub class: i32,
}

/// Address of `g_party`, resolved at startup; 0 until then or if the
/// pattern missed.
static G_PARTY: AtomicUsize = AtomicUsize::new(0);

/// The party's heroes that are in use, in party order. Empty before the save
/// hooks are installed, when the party pattern missed, or before a game is
/// started or loaded. Call from the game's thread (`on_save`,
/// `on_load_save`, a detour).
pub fn party_heroes() -> Vec<PartyHero> {
    let party = G_PARTY.load(Ordering::Acquire);
    if party == 0 {
        return Vec::new();
    }
    // SAFETY: the resolved g_party global; reads are checked.
    unsafe { party_heroes_at(party) }
}

/// [`party_heroes`] of the party struct at `party`.
///
/// # Safety
/// As for [`hero_key`].
pub unsafe fn party_heroes_at(party: usize) -> Vec<PartyHero> {
    let mut out = Vec::new();
    for index in 0..HERO_COUNT {
        let ptr = party + PARTY_HEROES + index * HERO_SIZE;
        // SAFETY: forwarded; each read is checked.
        let hero = unsafe {
            match game::read::<u8>(ptr + HERO_VALID) {
                Some(v) if v != 0 => {}
                _ => continue,
            }
            (hero_key(ptr), game::read::<i32>(ptr + HERO_CLASS))
        };
        if let (Some(key), Some(class)) = hero {
            out.push(PartyHero {
                index,
                ptr,
                key,
                class,
            });
        }
    }
    out
}

// --- Hooks ------------------------------------------------------------------

static ORIG_WRITE: AtomicUsize = AtomicUsize::new(0);
static ORIG_LOAD: AtomicUsize = AtomicUsize::new(0);
static ORIG_DELETE: AtomicUsize = AtomicUsize::new(0);
static ORIG_NEW_GAME: AtomicUsize = AtomicUsize::new(0);

/// The mod state of the game in play (see the module docs). `None` until a
/// game is loaded or started.
static CURRENT: Mutex<Option<Sidecar>> = Mutex::new(None);

type WriteFn = extern "system" fn(usize, usize, usize) -> u8;
type LoadFn = extern "system" fn(usize, u8) -> u8;
type DeleteFn = extern "system" fn(usize) -> u64;
type NewGameFn = extern "system" fn(usize);

/// Registers the save detours on `hooks` (`patterns` = the resolved core
/// patterns, `base` = the exe's base address). Each missing pattern is
/// logged and its hook skipped; the others still go in.
pub fn install(hooks: &mut HookSet, patterns: &BTreeMap<String, u32>, base: usize) {
    if let Some(&rva) = patterns.get("g_party") {
        G_PARTY.store(base + rva as usize, Ordering::Release);
    }
    let detours: [(&str, &AtomicUsize, usize); 4] = [
        (
            "savegame_write",
            &ORIG_WRITE,
            save_write as WriteFn as usize,
        ),
        (
            "game_load_from_slot",
            &ORIG_LOAD,
            load_from_slot as LoadFn as usize,
        ),
        (
            "save_delete_file",
            &ORIG_DELETE,
            delete_file as DeleteFn as usize,
        ),
        (
            "party_init_from_setup",
            &ORIG_NEW_GAME,
            new_game as NewGameFn as usize,
        ),
    ];
    let mut done = Vec::new();
    for (name, orig, detour) in detours {
        let Some(&rva) = patterns.get(name) else {
            crate::log(&format!("save hooks: {name} not found; that hook is off"));
            continue;
        };
        // SAFETY: the pattern locates the start of that function, and each
        // detour has its signature (module docs, core.toml).
        match unsafe { hooks.detour(base + rva as usize, detour) } {
            Ok(tramp) => {
                orig.store(tramp, Ordering::Release);
                done.push(name);
            }
            Err(e) => log_hook_error(name, &e),
        }
    }
    crate::log(&format!("save hooks registered: {}", done.join(", ")));
}

fn log_hook_error(name: &str, e: &HookError) {
    crate::log(&format!("save hooks: can't hook {name}: {e}"));
}

fn current() -> std::sync::MutexGuard<'static, Option<Sidecar>> {
    CURRENT.lock().unwrap_or_else(|e| e.into_inner())
}

/// Where the sidecar of `save_name` goes; `None` without a data folder.
fn path_for(save_name: &str) -> Option<std::path::PathBuf> {
    Some(sidecar_path(
        &crate::data_dir()?,
        account().as_deref(),
        save_name,
    ))
}

extern "system" fn save_write(state: usize, file: usize, custom: usize) -> u8 {
    // SAFETY: the game's `const String*` argument; reads are checked.
    let name = unsafe { game::read_string(file) };
    // SAFETY: the trampoline of SaveGame_Write, set before the hook went in.
    let orig: WriteFn = unsafe { std::mem::transmute(ORIG_WRITE.load(Ordering::Acquire)) };
    let ok = orig(state, file, custom);
    if ok != 0 {
        guard::call(GUARD_ID, || match name {
            Some(name) => after_save(&name),
            None => crate::log("save hook: can't read the save's file name; no sidecar written"),
        });
    }
    ok
}

fn after_save(name: &str) {
    let Some(path) = path_for(name) else { return };
    let mut cur = current();
    let sc = cur.get_or_insert_with(Sidecar::default);
    if host::with(|h: &mut Host| h.save(&mut sc.mods)).is_none() {
        crate::log("save hook: mod host busy or missing; saving the carried state only");
    }
    // A mod with nothing to keep has no table (it reads back as empty).
    sc.mods.retain(|_, s| *s != ModSaveState::default());
    match sc.save(&path) {
        Ok(()) => crate::log(&format!(
            "save hook: {name} saved; wrote {} ({} mod states)",
            path.display(),
            sc.mods.len()
        )),
        Err(e) => crate::log(&format!(
            "save hook: {name} saved, but writing {} failed: {e}",
            path.display()
        )),
    }
}

extern "system" fn load_from_slot(info: usize, silent: u8) -> u8 {
    // SAFETY: `SaveInfo*` argument; reads are checked.
    let name = info
        .checked_add(SAVEINFO_FILE_NAME)
        .and_then(|s| unsafe { game::read_string(s) });
    // SAFETY: the trampoline of Game_LoadFromSlot.
    let orig: LoadFn = unsafe { std::mem::transmute(ORIG_LOAD.load(Ordering::Acquire)) };
    let ok = orig(info, silent);
    if ok != 0 {
        guard::call(GUARD_ID, || after_load(name.as_deref()));
    }
    ok
}

fn after_load(name: Option<&str>) {
    let sc = match name.and_then(|n| path_for(n).map(|p| (n, p))) {
        None => {
            crate::log("load hook: can't read the save's file name; mods start empty");
            Sidecar::default()
        }
        Some((name, path)) => match Sidecar::load(&path) {
            Ok(sc) => {
                let how = if path.exists() {
                    "read"
                } else {
                    "no sidecar at"
                };
                crate::log(&format!(
                    "load hook: {name} loaded; {how} {} ({} mod states)",
                    path.display(),
                    sc.mods.len()
                ));
                sc
            }
            Err(e) => {
                crate::log(&format!(
                    "load hook: {name} loaded, but {e}; mods start empty"
                ));
                Sidecar::default()
            }
        },
    };
    let party: Vec<String> = party_heroes()
        .iter()
        .map(|h| format!("{} ({})", h.key, class_name(h.class).unwrap_or("?")))
        .collect();
    crate::log(&format!("load hook: party: {}", party.join(", ")));
    let mut cur = current();
    let sc = cur.insert(sc);
    if host::with(|h: &mut Host| h.load_save(&sc.mods)).is_none() {
        crate::log("load hook: mod host busy or missing; mods weren't told");
    }
}

extern "system" fn delete_file(file: usize) -> u64 {
    // SAFETY: `const String*` argument; reads are checked.
    let name = unsafe { game::read_string(file) };
    // SAFETY: the trampoline of Save_DeleteFile.
    let orig: DeleteFn = unsafe { std::mem::transmute(ORIG_DELETE.load(Ordering::Acquire)) };
    let r = orig(file);
    if let Some(name) = name {
        guard::call(GUARD_ID, || after_delete(&name));
    }
    r
}

/// Deletes the sidecar of `name`: only `uk_qol/saves/<account>/<sanitized
/// name>.qol.toml`, a path built here, so nothing outside our folder.
fn after_delete(name: &str) {
    let Some(path) = path_for(name) else { return };
    match std::fs::remove_file(&path) {
        Ok(()) => crate::log(&format!(
            "delete hook: {name} deleted; removed {}",
            path.display()
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => crate::log(&format!(
            "delete hook: {name} deleted, but removing {} failed: {e}",
            path.display()
        )),
    }
}

extern "system" fn new_game(setup: usize) {
    // SAFETY: the trampoline of Party_InitFromPartySetup.
    let orig: NewGameFn = unsafe { std::mem::transmute(ORIG_NEW_GAME.load(Ordering::Acquire)) };
    orig(setup);
    guard::call(GUARD_ID, || {
        let mut cur = current();
        let sc = cur.insert(Sidecar::default());
        if host::with(|h: &mut Host| h.load_save(&sc.mods)).is_none() {
            crate::log("new-game hook: mod host busy or missing; mods weren't told");
        }
        crate::log("new-game hook: new party; mod state reset");
    });
}

// --- Steam account ------------------------------------------------------------

static ACCOUNT: Mutex<Option<String>> = Mutex::new(None);

/// The Steam account id, cached after the first success; `None` (folder
/// `default`) if the Steam API isn't loaded or returns no user.
pub fn account() -> Option<String> {
    let mut a = ACCOUNT.lock().unwrap_or_else(|e| e.into_inner());
    if a.is_none() {
        *a = steam_account_id().map(|id| id.to_string());
        let shown = a
            .as_deref()
            .unwrap_or("default (Steam account not available)");
        crate::log(&format!("sidecar account folder: {shown}"));
    }
    a.clone()
}

#[cfg(windows)]
fn steam_account_id() -> Option<u32> {
    use core::ffi::c_void;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetModuleHandleW(name: *const u16) -> *mut c_void;
        fn GetProcAddress(module: *mut c_void, name: *const u8) -> *mut c_void;
    }
    let dll: Vec<u16> = "steam_api64.dll\0".encode_utf16().collect();
    // SAFETY: NUL-terminated name; doesn't load anything.
    let module = unsafe { GetModuleHandleW(dll.as_ptr()) };
    if module.is_null() {
        return None;
    }
    // SAFETY: NUL-terminated export names of a loaded module.
    let (user_fn, id_fn) = unsafe {
        (
            GetProcAddress(module, c"SteamAPI_SteamUser_v023".as_ptr().cast()),
            GetProcAddress(module, c"SteamAPI_ISteamUser_GetSteamID".as_ptr().cast()),
        )
    };
    if user_fn.is_null() || id_fn.is_null() {
        return None;
    }
    // SAFETY: the Steam flat API's signatures for these two exports.
    let user_fn: extern "C" fn() -> *mut c_void = unsafe { std::mem::transmute(user_fn) };
    let id_fn: extern "C" fn(*mut c_void) -> u64 = unsafe { std::mem::transmute(id_fn) };
    let user = user_fn();
    if user.is_null() {
        return None;
    }
    let account = id_fn(user) as u32;
    (account != 0).then_some(account)
}

#[cfg(not(windows))]
fn steam_account_id() -> Option<u32> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::tests::fake_string;

    fn put(buf: &mut [u8], at: usize, bytes: &[u8]) {
        buf[at..at + bytes.len()].copy_from_slice(bytes);
    }

    /// A fake hero struct named `name` (which must outlive it).
    fn fake_hero(buf: &mut [u8], id: i32, name: &[u8], class: i32) {
        put(buf, HERO_VALID, &[1]);
        put(buf, HERO_ID, &id.to_le_bytes());
        put(buf, HERO_NAME, &fake_string(name));
        put(buf, HERO_CLASS, &class.to_le_bytes());
    }

    #[test]
    fn keys() {
        assert_eq!(format_hero_key(2, "Žofie"), "2:Žofie");
        assert_eq!(class_name(6), Some("bloodmage"));
        assert_eq!(class_name(7), None);
        assert_eq!(class_name(-1), None);

        let name = b"Aldric".to_vec();
        let mut hero = vec![0u8; HERO_SIZE];
        fake_hero(&mut hero, 3, &name, 0);
        assert_eq!(
            unsafe { hero_key(hero.as_ptr() as usize) }.as_deref(),
            Some("3:Aldric")
        );
    }

    #[test]
    fn party() {
        let names: [&[u8]; 3] = [b"A", b"Bea", b"Cy"];
        let mut party = vec![0u8; PARTY_HEROES + HERO_COUNT * HERO_SIZE];
        for (i, (slot, class)) in [(0usize, 0), (1, 6), (3, 5)].into_iter().enumerate() {
            let at = PARTY_HEROES + slot * HERO_SIZE;
            fake_hero(&mut party[at..at + HERO_SIZE], slot as i32, names[i], class);
        }
        let heroes = unsafe { party_heroes_at(party.as_ptr() as usize) };
        let base = party.as_ptr() as usize + PARTY_HEROES;
        assert_eq!(
            heroes,
            vec![
                PartyHero {
                    index: 0,
                    ptr: base,
                    key: "0:A".into(),
                    class: 0
                },
                PartyHero {
                    index: 1,
                    ptr: base + HERO_SIZE,
                    key: "1:Bea".into(),
                    class: 6
                },
                PartyHero {
                    index: 3,
                    ptr: base + 3 * HERO_SIZE,
                    key: "3:Cy".into(),
                    class: 5
                },
            ]
        );
        // Not installed: no party.
        assert!(party_heroes().is_empty());
    }
}
