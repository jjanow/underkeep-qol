//! Save hooks (PLAN §2): mod state follows the game's saves through
//! sidecars ([`crate::save`]), one per save. The game's own save files are
//! never read or written.
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
//!   entry gets an empty state, so it resets. A sidecar that exists but
//!   can't be read (newer format, broken) is never overwritten by a save to
//!   the same name.
//! - **Save_DeleteFile**: deletes that save's sidecar (cloud and local), if
//!   there is one.
//! - **Party_InitFromPartySetup** (a new game): mods get an empty state.
//!
//! "The state of the game in play" is the sidecar last loaded (or empty
//! after a new game). Saving starts from it, not from the target file's old
//! sidecar, so the state of a mod that is off now is carried into the new
//! save, and loading save_1 then saving as save_2 doesn't pick up save_2's
//! old state.
//!
//! # Where sidecars go
//! In Steam Cloud next to the game's saves, `qol_<save name>.toml`, through
//! `ISteamRemoteStorage` like the saves ([`crate::steam::cloud`]), so mod
//! state follows a save to other computers and the Steam Deck. Without Steam
//! (or when a cloud write fails): locally, `<game folder>/uk_qol/saves/
//! <account>/<save name>.qol.toml`. See [`crate::save::Store`]. At the first
//! hook of a session with Steam, local sidecars are moved into the cloud
//! ([`crate::save::Store::migrate`]: the newer copy wins, the local file is
//! kept in `migrated/`).
//!
//! `<account>` is the Steam account id (the number in Steam's
//! `userdata/<account>`, [`crate::steam::account_id`]) at the first save or
//! load, and `default` if that isn't available.
//!
//! # Heroes
//! A hero's `hero_id` in the save is only the party slot it was created in
//! (0..3), so it repeats when a hero is replaced. Mods key per-hero state by
//! [`hero_key`] (`"<hero_id>:<name>"`) and can check the class on load;
//! [`party_heroes`] lists the four heroes of the party.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::game;
use crate::guard;
use crate::hook::{HookError, HookSet};
use crate::host::{self, Host, ModSaveState};
use crate::save::{CloudFiles, Sidecar, Store, account_dir};
use crate::steam;

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

/// The save name of the game in play, if its sidecar existed but couldn't be
/// read (a newer format, a broken file). Saving to that name doesn't write a
/// sidecar at all, so the player's file (cloud or local) survives until they
/// fix or remove it. Cleared by the next load or new game.
static UNREADABLE: Mutex<Option<String>> = Mutex::new(None);

fn set_unreadable(name: Option<String>) {
    *UNREADABLE.lock().unwrap_or_else(|e| e.into_inner()) = name;
}

/// Runs `f` with the sidecar store of this account (Steam Cloud if
/// available, else the local folder); `None` without a data folder. The
/// first call of a session with Steam Cloud migrates local sidecars first.
fn with_store<R>(f: impl FnOnce(&Store) -> R) -> Option<R> {
    static MIGRATED: AtomicBool = AtomicBool::new(false);
    let dir = account_dir(&crate::data_dir()?, account().as_deref());
    let cloud = steam::cloud();
    let store = Store {
        dir,
        cloud: cloud.as_ref().map(|c| c as &dyn CloudFiles),
    };
    if store.cloud.is_some() && !MIGRATED.swap(true, Ordering::Relaxed) {
        for line in store.migrate() {
            crate::log(&format!("sidecars: {line}"));
        }
    }
    Some(f(&store))
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
    with_store(|store| {
        if UNREADABLE
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_deref()
            == Some(name)
        {
            crate::log(&format!(
                "save hook: {name} saved; its sidecar couldn't be read when loading, so it \
                 is left as it is"
            ));
            return;
        }
        let mut cur = current();
        let sc = cur.get_or_insert_with(Sidecar::default);
        if host::with(|h: &mut Host| h.save(&mut sc.mods)).is_none() {
            crate::log("save hook: mod host busy or missing; saving the carried state only");
        }
        // A mod with nothing to keep has no table (it reads back as empty).
        sc.mods.retain(|_, s| *s != ModSaveState::default());
        match store.write(name, sc.to_toml().as_bytes()) {
            Ok((to, notes)) => {
                for n in notes {
                    crate::log(&format!("save hook: {n}"));
                }
                crate::log(&format!(
                    "save hook: {name} saved; wrote {to} ({} mod states)",
                    sc.mods.len()
                ));
            }
            Err(e) => crate::log(&format!(
                "save hook: {name} saved, but writing its sidecar failed: {e}"
            )),
        }
    });
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
    set_unreadable(None);
    let read = name.and_then(|n| {
        with_store(|store| {
            let r = store.read(n).and_then(|found| match found {
                None => Ok((
                    Sidecar::default(),
                    format!("no sidecar at {}", store.describe(n)),
                )),
                Some((bytes, from)) => String::from_utf8(bytes)
                    .map_err(|_| format!("{from}: not UTF-8"))
                    .and_then(|text| Sidecar::parse(&text).map_err(|e| format!("{from}: {e}")))
                    .map(|sc| (sc, format!("read {from}"))),
            });
            (n, r)
        })
    });
    let sc = match read {
        None => {
            crate::log("load hook: can't read the save's file name; mods start empty");
            Sidecar::default()
        }
        Some((name, Ok((sc, how)))) => {
            crate::log(&format!(
                "load hook: {name} loaded; {how} ({} mod states)",
                sc.mods.len()
            ));
            sc
        }
        Some((name, Err(e))) => {
            crate::log(&format!(
                "load hook: {name} loaded, but {e}; mods start empty, and saving to {name} \
                 won't overwrite that file"
            ));
            set_unreadable(Some(name.to_owned()));
            Sidecar::default()
        }
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

/// Deletes the sidecar of `name`: only the cloud file `qol_<sanitized
/// name>.toml` and `uk_qol/saves/<account>/<sanitized name>.qol.toml`, names
/// built here, so nothing else.
fn after_delete(name: &str) {
    for line in with_store(|store| store.delete(name)).unwrap_or_default() {
        crate::log(&format!("delete hook: {name} deleted; {line}"));
    }
}

extern "system" fn new_game(setup: usize) {
    // SAFETY: the trampoline of Party_InitFromPartySetup.
    let orig: NewGameFn = unsafe { std::mem::transmute(ORIG_NEW_GAME.load(Ordering::Acquire)) };
    orig(setup);
    guard::call(GUARD_ID, || {
        set_unreadable(None);
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
        *a = steam::account_id().map(|id| id.to_string());
        let shown = a
            .as_deref()
            .unwrap_or("default (Steam account not available)");
        crate::log(&format!("sidecar account folder: {shown}"));
    }
    a.clone()
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
