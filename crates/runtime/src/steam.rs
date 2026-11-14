//! The Steam API the game already loaded (`steam_api64.dll`), through its
//! flat C exports: the account id ([`account_id`]) and Steam Cloud files
//! ([`cloud`], `ISteamRemoteStorage`, the interface the game writes its saves
//! through).
//!
//! Nothing here loads `steam_api64.dll` or initialises Steam: the exports are
//! looked up in the module the game loaded (`GetModuleHandleW`). If the DLL or
//! an export is missing, or the interface isn't available yet, the callers fall
//! back to local files; each of those is logged once. Call from the game's
//! thread (the save hooks), after the game initialised Steam.

use std::ffi::{CString, c_char, c_void};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::save::CloudFiles;

/// Largest cloud file [`Cloud::read`] accepts (sidecars are a few KB).
pub const READ_MAX: i32 = 16 << 20;

type GetIface = unsafe extern "C" fn() -> *mut c_void;
type FileWrite = unsafe extern "C" fn(*mut c_void, *const c_char, *const c_void, i32) -> u8;
type FileRead = unsafe extern "C" fn(*mut c_void, *const c_char, *mut c_void, i32) -> i32;
type FileName = unsafe extern "C" fn(*mut c_void, *const c_char) -> u8;
type FileSize = unsafe extern "C" fn(*mut c_void, *const c_char) -> i32;
type FileTimestamp = unsafe extern "C" fn(*mut c_void, *const c_char) -> i64;

/// The `ISteamRemoteStorage` flat exports, resolved once.
#[cfg_attr(not(windows), allow(dead_code))]
struct RemoteStorage {
    iface: GetIface,
    write: FileWrite,
    read: FileRead,
    exists: FileName,
    delete: FileName,
    size: FileSize,
    timestamp: FileTimestamp,
}

/// Steam Cloud storage of the running game (`userdata/<account>/2411200/
/// remote/`), from [`cloud`].
pub struct Cloud {
    api: &'static RemoteStorage,
    iface: *mut c_void,
}

/// The game's Steam Cloud storage, `None` without Steam (logged once).
pub fn cloud() -> Option<Cloud> {
    static API: OnceLock<Option<RemoteStorage>> = OnceLock::new();
    static NO_IFACE: AtomicBool = AtomicBool::new(false);
    let api = API
        .get_or_init(|| {
            let api = resolve_remote_storage();
            if api.is_none() {
                crate::log(
                    "steam cloud: steam_api64.dll or its ISteamRemoteStorage exports not \
                     found; sidecars stay local",
                );
            }
            api
        })
        .as_ref()?;
    // SAFETY: `SteamAPI_SteamRemoteStorage_v016` takes no arguments and
    // returns the interface or null (Steam not initialised).
    let iface = unsafe { (api.iface)() };
    if iface.is_null() {
        if !NO_IFACE.swap(true, Ordering::Relaxed) {
            crate::log("steam cloud: ISteamRemoteStorage not available; sidecars stay local");
        }
        return None;
    }
    Some(Cloud { api, iface })
}

fn c_name(name: &str) -> Result<CString, String> {
    CString::new(name).map_err(|_| format!("cloud file name {name:?} contains NUL"))
}

impl CloudFiles for Cloud {
    fn timestamp(&self, name: &str) -> Option<i64> {
        let n = c_name(name).ok()?;
        // SAFETY: the interface from `cloud`, a NUL-terminated name; the
        // flat API's signatures (isteamremotestorage.h, v016).
        unsafe {
            if (self.api.exists)(self.iface, n.as_ptr()) == 0 {
                return None;
            }
            Some((self.api.timestamp)(self.iface, n.as_ptr()))
        }
    }

    fn read(&self, name: &str) -> Result<Option<Vec<u8>>, String> {
        let n = c_name(name)?;
        // SAFETY: as in `timestamp`; FileRead writes at most `size` bytes
        // into the buffer, which has that length.
        unsafe {
            if (self.api.exists)(self.iface, n.as_ptr()) == 0 {
                return Ok(None);
            }
            let size = (self.api.size)(self.iface, n.as_ptr());
            if !(0..=READ_MAX).contains(&size) {
                return Err(format!("cloud file {name}: size {size}"));
            }
            let mut buf = vec![0u8; size as usize];
            if size == 0 {
                return Ok(Some(buf));
            }
            let got = (self.api.read)(self.iface, n.as_ptr(), buf.as_mut_ptr().cast(), size);
            if got != size {
                return Err(format!("cloud file {name}: read {got} of {size} bytes"));
            }
            Ok(Some(buf))
        }
    }

    fn write(&self, name: &str, bytes: &[u8]) -> Result<(), String> {
        let n = c_name(name)?;
        let len = i32::try_from(bytes.len()).map_err(|_| format!("cloud file {name}: too big"))?;
        // SAFETY: as in `timestamp`; the buffer has `len` bytes.
        let ok = unsafe { (self.api.write)(self.iface, n.as_ptr(), bytes.as_ptr().cast(), len) };
        if ok == 0 {
            return Err(format!(
                "writing cloud file {name} failed (Steam Cloud full or unavailable?)"
            ));
        }
        Ok(())
    }

    fn delete(&self, name: &str) -> Result<bool, String> {
        let n = c_name(name)?;
        // SAFETY: as in `timestamp`.
        unsafe {
            if (self.api.exists)(self.iface, n.as_ptr()) == 0 {
                return Ok(false);
            }
            if (self.api.delete)(self.iface, n.as_ptr()) == 0 {
                return Err(format!("deleting cloud file {name} failed"));
            }
        }
        Ok(true)
    }

    fn describe(&self, name: &str) -> String {
        format!("Steam Cloud file {name}")
    }
}

#[cfg(windows)]
mod sys {
    use core::ffi::c_void;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetModuleHandleW(name: *const u16) -> *mut c_void;
        fn GetProcAddress(module: *mut c_void, name: *const u8) -> *mut c_void;
    }

    /// The loaded `steam_api64.dll`, null if it isn't loaded.
    pub fn steam_api() -> *mut c_void {
        let dll: Vec<u16> = "steam_api64.dll\0".encode_utf16().collect();
        // SAFETY: NUL-terminated name; doesn't load anything.
        unsafe { GetModuleHandleW(dll.as_ptr()) }
    }

    /// Export `name` of `module` (non-null), null if missing.
    pub fn export(module: *mut c_void, name: &core::ffi::CStr) -> *mut c_void {
        // SAFETY: a loaded module and a NUL-terminated export name.
        unsafe { GetProcAddress(module, name.as_ptr().cast()) }
    }
}

#[cfg(windows)]
fn resolve_remote_storage() -> Option<RemoteStorage> {
    let module = sys::steam_api();
    if module.is_null() {
        return None;
    }
    let get = |name: &core::ffi::CStr| {
        let p = sys::export(module, name);
        (!p.is_null()).then_some(p)
    };
    let iface = get(c"SteamAPI_SteamRemoteStorage_v016")?;
    let write = get(c"SteamAPI_ISteamRemoteStorage_FileWrite")?;
    let read = get(c"SteamAPI_ISteamRemoteStorage_FileRead")?;
    let exists = get(c"SteamAPI_ISteamRemoteStorage_FileExists")?;
    let delete = get(c"SteamAPI_ISteamRemoteStorage_FileDelete")?;
    let size = get(c"SteamAPI_ISteamRemoteStorage_GetFileSize")?;
    let timestamp = get(c"SteamAPI_ISteamRemoteStorage_GetFileTimestamp")?;
    // SAFETY: the flat API's signatures for these exports (steam_api_flat.h:
    // `bool` is one byte, `int32` / `int64` as declared).
    unsafe {
        Some(RemoteStorage {
            iface: std::mem::transmute::<*mut c_void, GetIface>(iface),
            write: std::mem::transmute::<*mut c_void, FileWrite>(write),
            read: std::mem::transmute::<*mut c_void, FileRead>(read),
            exists: std::mem::transmute::<*mut c_void, FileName>(exists),
            delete: std::mem::transmute::<*mut c_void, FileName>(delete),
            size: std::mem::transmute::<*mut c_void, FileSize>(size),
            timestamp: std::mem::transmute::<*mut c_void, FileTimestamp>(timestamp),
        })
    }
}

#[cfg(not(windows))]
fn resolve_remote_storage() -> Option<RemoteStorage> {
    None
}

/// The Steam account id (low 32 bits of the SteamID, the number in Steam's
/// `userdata/<account>`), `None` without Steam or a user.
#[cfg(windows)]
pub fn account_id() -> Option<u32> {
    let module = sys::steam_api();
    if module.is_null() {
        return None;
    }
    let user_fn = sys::export(module, c"SteamAPI_SteamUser_v023");
    let id_fn = sys::export(module, c"SteamAPI_ISteamUser_GetSteamID");
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
pub fn account_id() -> Option<u32> {
    None
}
