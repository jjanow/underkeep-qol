//! Import-address-table hooks: redirect one module's calls to a DLL
//! function by swapping the function pointer in that module's IAT.
//!
//! Only calls made *through that import* are redirected (the game exe's calls
//! to SDL, not SDL's internal calls), and no code is rewritten, so this
//! doesn't need the detour engine. The overlay uses it for the SDL3 functions
//! `underkeep.exe` imports by name.

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IatError {
    /// The PE headers or import directory are malformed or truncated.
    Malformed(&'static str),
    /// The module doesn't import that DLL.
    NoDll(String),
    /// The module imports the DLL but not that function by name.
    NoFunction(String),
    /// Not supported on this platform (non-Windows builds).
    Unsupported,
    /// VirtualProtect failed or the slot changed under us.
    Memory(String),
}

impl fmt::Display for IatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IatError::Malformed(what) => write!(f, "malformed import table: {what}"),
            IatError::NoDll(d) => write!(f, "module doesn't import {d}"),
            IatError::NoFunction(n) => write!(f, "{n} is not imported by name"),
            IatError::Unsupported => write!(f, "IAT hooks are not supported on this platform"),
            IatError::Memory(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for IatError {}

fn u16_at(b: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(o..o + 2)?.try_into().ok()?))
}

fn u32_at(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?))
}

fn u64_at(b: &[u8], o: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(o..o + 8)?.try_into().ok()?))
}

/// The NUL-terminated ASCII string at `o` (at most 512 bytes).
fn cstr_at(b: &[u8], o: usize) -> Option<&[u8]> {
    let tail = b.get(o..)?;
    let n = tail.iter().take(512).position(|&c| c == 0)?;
    Some(&tail[..n])
}

/// One by-name import of a module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Import {
    pub dll: String,
    pub name: String,
    /// RVA of the IAT slot (the pointer the loader fills in).
    pub slot_rva: u32,
}

/// Lists the by-name imports of a PE32+ image in image layout (as mapped by
/// the loader, or by [`crate::pe::map_file`]). Ordinal imports are skipped.
///
/// Names are read from the import lookup table (`OriginalFirstThunk`),
/// because in a loaded image the IAT already holds addresses.
pub fn imports(img: &[u8]) -> Result<Vec<Import>, IatError> {
    let m = IatError::Malformed;
    if img.get(..2) != Some(b"MZ") {
        return Err(m("MZ signature"));
    }
    let nt = u32_at(img, 0x3c).ok_or(m("e_lfanew"))? as usize;
    if img.get(nt..nt + 4) != Some(b"PE\0\0") {
        return Err(m("PE signature"));
    }
    let opt = nt + 24;
    if u16_at(img, opt) != Some(0x20b) {
        return Err(m("not PE32+"));
    }
    let ndirs = u32_at(img, opt + 108).ok_or(m("data directory count"))?;
    if ndirs < 2 {
        return Ok(Vec::new());
    }
    let dir = u32_at(img, opt + 112 + 8).ok_or(m("import directory"))? as usize;
    if dir == 0 {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for i in 0.. {
        let d = dir + i * 20;
        let lookup = u32_at(img, d).ok_or(m("import descriptor"))? as usize;
        let name_rva = u32_at(img, d + 12).ok_or(m("import descriptor"))? as usize;
        let iat = u32_at(img, d + 16).ok_or(m("import descriptor"))? as usize;
        if lookup == 0 && name_rva == 0 && iat == 0 {
            break;
        }
        if i > 4096 {
            return Err(m("unterminated import directory"));
        }
        let dll = String::from_utf8_lossy(cstr_at(img, name_rva).ok_or(m("DLL name"))?).into_owned();
        // Without a lookup table the names are only in the IAT, which is
        // valid before binding (files, not loaded images).
        let names = if lookup != 0 { lookup } else { iat };
        for j in 0.. {
            let entry = u64_at(img, names + j * 8).ok_or(m("thunk"))?;
            if entry == 0 {
                break;
            }
            if j > 65536 {
                return Err(m("unterminated thunk list"));
            }
            if entry & (1 << 63) != 0 {
                continue; // by ordinal
            }
            let hint_name = (entry & 0x7fff_ffff) as usize;
            let name = cstr_at(img, hint_name + 2).ok_or(m("import name"))?;
            out.push(Import {
                dll: dll.clone(),
                name: String::from_utf8_lossy(name).into_owned(),
                slot_rva: (iat + j * 8) as u32,
            });
        }
    }
    Ok(out)
}

/// RVA of the IAT slot through which `img` calls `dll!func`. The DLL name is
/// compared case-insensitively, the function name exactly.
pub fn find_slot(img: &[u8], dll: &str, func: &str) -> Result<u32, IatError> {
    let all = imports(img)?;
    let mut any_dll = false;
    for imp in &all {
        if imp.dll.eq_ignore_ascii_case(dll) {
            any_dll = true;
            if imp.name == func {
                return Ok(imp.slot_rva);
            }
        }
    }
    Err(if any_dll {
        IatError::NoFunction(format!("{dll}!{func}"))
    } else {
        IatError::NoDll(dll.to_owned())
    })
}

#[cfg(windows)]
pub use win::IatHook;

#[cfg(windows)]
mod win {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use windows_sys::Win32::System::Memory::{PAGE_READWRITE, VirtualProtect};

    /// An installed IAT hook. Dropping it does not unhook; call [`remove`].
    ///
    /// [`remove`]: IatHook::remove
    pub struct IatHook {
        slot: *const AtomicUsize,
        original: usize,
        detour: usize,
        pub name: String,
    }

    // SAFETY: the slot is a word in the module's IAT, which lives as long as
    // the module (the game exe: the whole process); all access is atomic.
    unsafe impl Send for IatHook {}
    unsafe impl Sync for IatHook {}

    unsafe fn write_slot(slot: *const AtomicUsize, expect: usize, new: usize) -> Result<(), IatError> {
        let mut old = 0u32;
        // SAFETY: the slot is mapped (it's in the module image); protection
        // is restored right after.
        unsafe {
            if VirtualProtect(slot as *const _, 8, PAGE_READWRITE, &mut old) == 0 {
                return Err(IatError::Memory(format!(
                    "VirtualProtect failed ({})",
                    std::io::Error::last_os_error()
                )));
            }
            let r = (*slot).compare_exchange(expect, new, Ordering::SeqCst, Ordering::SeqCst);
            let mut ignored = 0u32;
            VirtualProtect(slot as *const _, 8, old, &mut ignored);
            r.map(|_| ()).map_err(|now| {
                IatError::Memory(format!(
                    "slot changed while hooking (now {now:#x}, expected {expect:#x})"
                ))
            })
        }
    }

    impl IatHook {
        /// Points the import `dll!func` of the module at `base` to `detour`.
        /// The current slot value (the real function, or another hook's
        /// detour, which is then chained) is stored in `original` *before*
        /// the slot is swapped, so the detour can call it from the first call.
        ///
        /// # Safety
        /// `base` must be a loaded module whose imports are bound, and
        /// `detour` a function with the import's exact signature and ABI.
        pub unsafe fn install(
            base: usize,
            dll: &str,
            func: &str,
            detour: usize,
            original: &AtomicUsize,
        ) -> Result<IatHook, IatError> {
            // SAFETY: a loaded module's headers are readable; SizeOfImage
            // covers every section, and all of them are mapped.
            let img = unsafe {
                let hdr = std::slice::from_raw_parts(base as *const u8, 0x1000);
                let h = crate::pe::Headers::parse(hdr).map_err(|_| IatError::Malformed("headers"))?;
                std::slice::from_raw_parts(base as *const u8, h.size_of_image as usize)
            };
            let rva = find_slot(img, dll, func)?;
            let slot = (base + rva as usize) as *const AtomicUsize;
            // SAFETY: an aligned 8-byte IAT entry inside the image.
            let current = unsafe { (*slot).load(Ordering::SeqCst) };
            if current < base + img.len() && current >= base {
                return Err(IatError::Memory(format!(
                    "{func}: import not bound yet (slot {current:#x})"
                )));
            }
            original.store(current, Ordering::SeqCst);
            // SAFETY: see above.
            unsafe { write_slot(slot, current, detour)? };
            Ok(IatHook {
                slot,
                original: current,
                detour,
                name: func.to_owned(),
            })
        }

        /// The function the slot pointed to before the hook.
        pub fn original(&self) -> usize {
            self.original
        }

        /// Restores the original pointer, if the slot still holds our detour
        /// (another hook installed on top is left alone, and Err returned).
        pub fn remove(&self) -> Result<(), IatError> {
            // SAFETY: the slot was valid at install and the image is still
            // loaded (it's the exe).
            unsafe { write_slot(self.slot, self.detour, self.original) }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pe::{map_file, tests::build_pe};

    /// Builds an image with an `.idata` section at 0x2000 importing
    /// `dlls[i].1` (by name; `None` = by ordinal) from `dlls[i].0`.
    /// With `with_lookup` false the descriptors have no lookup table.
    fn image_with_imports(dlls: &[(&str, &[Option<&str>])], with_lookup: bool) -> Vec<u8> {
        const BASE: usize = 0x2000;
        let mut idata = vec![0u8; 20 * (dlls.len() + 1)];
        let mut put = |bytes: &[u8], idata: &mut Vec<u8>| -> usize {
            let at = idata.len().next_multiple_of(8);
            idata.resize(at, 0);
            idata.extend_from_slice(bytes);
            BASE + at
        };
        for (i, (dll, funcs)) in dlls.iter().enumerate() {
            let mut name = dll.as_bytes().to_vec();
            name.push(0);
            let name_rva = put(&name, &mut idata);
            let mut thunks = Vec::new();
            for (k, f) in funcs.iter().enumerate() {
                let v = match f {
                    Some(f) => {
                        let mut hn = vec![k as u8, 0];
                        hn.extend_from_slice(f.as_bytes());
                        hn.push(0);
                        put(&hn, &mut idata) as u64
                    }
                    None => (1u64 << 63) | 7,
                };
                thunks.extend_from_slice(&v.to_le_bytes());
            }
            thunks.extend_from_slice(&[0; 8]);
            let ilt = put(&thunks, &mut idata);
            let iat = put(&thunks, &mut idata);
            let d = i * 20;
            if with_lookup {
                idata[d..d + 4].copy_from_slice(&(ilt as u32).to_le_bytes());
            }
            idata[d + 12..d + 16].copy_from_slice(&(name_rva as u32).to_le_bytes());
            idata[d + 16..d + 20].copy_from_slice(&(iat as u32).to_le_bytes());
        }
        let mut f = build_pe(&[(".text", 0x1000, &[0xc3], 0x2000_0000), (".idata", BASE as u32, &idata, 0)]);
        // `build_pe` leaves the data directories empty: set the count and
        // the import directory (index 1).
        let opt = 0x80 + 24;
        f[opt + 108..opt + 112].copy_from_slice(&16u32.to_le_bytes());
        f[opt + 120..opt + 124].copy_from_slice(&(BASE as u32).to_le_bytes());
        f[opt + 124..opt + 128].copy_from_slice(&((20 * (dlls.len() + 1)) as u32).to_le_bytes());
        map_file(&f).unwrap()
    }

    #[test]
    fn finds_slots_by_name() {
        let img = image_with_imports(
            &[
                ("KERNEL32.dll", &[Some("Sleep"), Some("ExitProcess")]),
                (
                    "SDL3.dll",
                    &[Some("SDL_Init"), None, Some("SDL_PollEvent"), Some("SDL_GL_SwapWindow")],
                ),
            ],
            true,
        );
        let all = imports(&img).unwrap();
        assert_eq!(all.len(), 5, "ordinal import skipped");
        let poll = find_slot(&img, "sdl3.DLL", "SDL_PollEvent").unwrap();
        let swap = find_slot(&img, "SDL3.dll", "SDL_GL_SwapWindow").unwrap();
        let init = find_slot(&img, "SDL3.dll", "SDL_Init").unwrap();
        // Slots are consecutive 8-byte entries of the same IAT.
        assert_eq!(poll, init + 16);
        assert_eq!(swap, init + 24);
        // The slot is in the IAT, not the lookup table: the two tables hold
        // the same values, and the IAT comes second.
        let sleep = find_slot(&img, "kernel32.dll", "Sleep").unwrap();
        assert!(init > sleep);
        assert_eq!(
            find_slot(&img, "SDL3.dll", "SDL_Quit"),
            Err(IatError::NoFunction("SDL3.dll!SDL_Quit".into()))
        );
        assert_eq!(
            find_slot(&img, "fmod.dll", "x"),
            Err(IatError::NoDll("fmod.dll".into()))
        );
    }

    #[test]
    fn falls_back_to_iat_without_lookup_table() {
        let img = image_with_imports(&[("SDL3.dll", &[Some("SDL_PollEvent")])], false);
        assert!(find_slot(&img, "SDL3.dll", "SDL_PollEvent").is_ok());
    }

    #[test]
    fn rejects_garbage_and_empty() {
        assert_eq!(imports(b"nope"), Err(IatError::Malformed("MZ signature")));
        let plain = map_file(&build_pe(&[(".text", 0x1000, &[0xc3], 0x2000_0000)])).unwrap();
        assert_eq!(imports(&plain), Ok(Vec::new()));
    }

    #[test]
    fn game_exe_imports_sdl_entry_points() {
        let Some(p) = crate::pe::tests::game_exe() else { return };
        let img = map_file(&std::fs::read(p).unwrap()).unwrap();
        for f in [
            "SDL_PollEvent",
            "SDL_GL_SwapWindow",
            "SDL_GetGamepadButton",
            "SDL_GetGamepadAxis",
        ] {
            find_slot(&img, "SDL3.dll", f).unwrap();
        }
    }
}
