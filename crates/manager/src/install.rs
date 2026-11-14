//! Installing and removing the mod loader in a game folder.
//!
//! What gets installed:
//!
//! - `<game>/winmm.dll`: the loader, with the runtime and every built-in mod.
//! - `<game>/uk_qol/install.toml`: what the manager installed (see
//!   [`InstallManifest`]), so uninstall removes exactly that and nothing else.
//! - `<game>/uk_qol/patterns/<mod id>.toml`: downloaded pattern updates.
//!
//! The runtime adds `uk_qol/config.toml` (settings), `uk_qol/uk_qol.log`,
//! session/crash markers and save sidecars. Uninstall removes the loader,
//! the files listed in `install.toml` and the log; it keeps the settings and
//! **always keeps save sidecars**, which hold items that exist nowhere else.
//!
//! None of these functions checks whether the game is running; callers do.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::tr;

pub const DATA_DIR: &str = "uk_qol";
pub const DLL: &str = "winmm.dll";
pub const MANIFEST: &str = "uk_qol/install.toml";
/// Where a `winmm.dll` that wasn't ours is kept while we're installed.
pub const BACKUP_DLL: &str = "uk_qol/backup/winmm.dll";
/// Save sidecars (`saves/<account|default>/<save>.qol.toml`): never removed
/// by the manager.
pub const SAVES_DIR: &str = "uk_qol/saves";
/// Files the runtime writes that are safe to remove on uninstall.
pub const RUNTIME_FILES: &[&str] = &[
    "uk_qol/uk_qol.log",
    "uk_qol/uk_qol.log.prev",
    "uk_qol/session.toml",
];

/// `uk_qol/install.toml`.
///
/// ```toml
/// manager_version = "0.1.0"
/// runtime_version = "0.1.0"
/// installed_at = 1794677278   # unix seconds
/// backup = "uk_qol/backup/winmm.dll"   # only if a foreign winmm.dll was replaced
///
/// [files]
/// "winmm.dll" = "<sha256 hex>"
/// "uk_qol/patterns/skip-logos.toml" = "<sha256 hex>"
/// ```
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct InstallManifest {
    pub manager_version: String,
    /// Version of the installed `winmm.dll` ("unknown" if not known).
    pub runtime_version: String,
    pub installed_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backup: Option<String>,
    /// Path relative to the game folder (always `/`) → SHA-256 hex.
    #[serde(default)]
    pub files: BTreeMap<String, String>,
}

impl InstallManifest {
    pub fn read(game_dir: &Path) -> Result<Option<InstallManifest>, String> {
        let p = game_dir.join(MANIFEST);
        match std::fs::read_to_string(&p) {
            Ok(t) => toml::from_str(&t)
                .map(Some)
                .map_err(|e| tr!("err_manifest_parse", p.display(), e)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(tr!("err_read", p.display(), e)),
        }
    }

    pub fn write(&self, game_dir: &Path) -> Result<(), String> {
        let body = toml::to_string(self).map_err(|e| e.to_string())?;
        let text = format!("{}\n{body}", tr!("manifest_header"));
        write_atomic(&game_dir.join(MANIFEST), text.as_bytes())
    }
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    uk_qol_runtime::build::hex(&Sha256::digest(bytes))
}

pub fn sha256_file(p: &Path) -> std::io::Result<String> {
    uk_qol_runtime::build::sha256_file(p).map(|h| uk_qol_runtime::build::hex(&h))
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Writes `bytes` to `path` through a temporary file in the same folder and
/// a rename, creating parent folders.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir).map_err(|e| tr!("err_write", dir.display(), e))?;
    let mut tmp_name = path.file_name().unwrap_or_default().to_os_string();
    tmp_name.push(format!(".uk-qol-tmp-{}", std::process::id()));
    let tmp = dir.join(tmp_name);
    let res = std::fs::write(&tmp, bytes).and_then(|()| std::fs::rename(&tmp, path));
    if let Err(e) = res {
        let _ = std::fs::remove_file(&tmp);
        return Err(tr!("err_write", path.display(), e));
    }
    Ok(())
}

/// State of `<game>/winmm.dll`.
#[derive(Debug, Clone, PartialEq)]
pub enum DllState {
    Missing,
    /// Matches what `install.toml` recorded.
    Ours,
    /// Recorded in `install.toml` but changed since.
    Modified,
    /// There, but we didn't install it.
    Foreign,
}

pub fn dll_state(game_dir: &Path, manifest: Option<&InstallManifest>) -> DllState {
    let p = game_dir.join(DLL);
    if !p.is_file() {
        return DllState::Missing;
    }
    let Some(want) = manifest.and_then(|m| m.files.get(DLL)) else {
        return DllState::Foreign;
    };
    match sha256_file(&p) {
        Ok(h) if &h == want => DllState::Ours,
        _ => DllState::Modified,
    }
}

/// A quick sanity check that `bytes` is a 64-bit Windows DLL.
pub fn looks_like_dll(bytes: &[u8]) -> bool {
    let Ok(h) = uk_qol_runtime::pe::Headers::parse(bytes) else {
        return false;
    };
    let _ = h;
    // IMAGE_FILE_DLL in the COFF characteristics.
    let pe = u32::from_le_bytes(bytes[0x3c..0x40].try_into().unwrap()) as usize;
    bytes
        .get(pe + 22..pe + 24)
        .is_some_and(|c| u16::from_le_bytes([c[0], c[1]]) & 0x2000 != 0)
}

pub struct InstallOpts<'a> {
    pub dll: &'a [u8],
    pub runtime_version: String,
    /// Replace a `winmm.dll` we didn't install (it's backed up and put back
    /// on uninstall).
    pub force: bool,
}

#[derive(Debug, Default, PartialEq)]
pub struct InstallOutcome {
    pub updated: bool,
    pub backed_up: bool,
}

/// Installs (or updates) the loader in `game_dir`.
pub fn install(game_dir: &Path, opts: &InstallOpts) -> Result<InstallOutcome, String> {
    if !crate::paths::is_game_dir(game_dir) {
        return Err(tr!("err_not_game_dir", game_dir.display()));
    }
    if !looks_like_dll(opts.dll) {
        return Err(tr!("err_not_dll"));
    }
    let mut manifest = InstallManifest::read(game_dir)?.unwrap_or_default();
    let mut outcome = InstallOutcome::default();
    let new_hash = sha256_hex(opts.dll);
    match dll_state(game_dir, Some(&manifest)) {
        DllState::Missing => {}
        DllState::Ours => outcome.updated = true,
        DllState::Modified | DllState::Foreign => {
            let current = sha256_file(&game_dir.join(DLL)).unwrap_or_default();
            if current == new_hash {
                // Same file already there (e.g. copied by hand): adopt it.
            } else if !opts.force {
                return Err(tr!("err_foreign_dll"));
            } else if manifest.backup.is_none() && !manifest.files.contains_key(DLL) {
                // Foreign DLL: keep it to put back on uninstall.
                let backup = game_dir.join(BACKUP_DLL);
                std::fs::create_dir_all(backup.parent().unwrap())
                    .map_err(|e| tr!("err_write", backup.display(), e))?;
                std::fs::copy(game_dir.join(DLL), &backup)
                    .map_err(|e| tr!("err_write", backup.display(), e))?;
                manifest.backup = Some(BACKUP_DLL.into());
                outcome.backed_up = true;
            }
        }
    }
    write_atomic(&game_dir.join(DLL), opts.dll)?;
    manifest.manager_version = env!("CARGO_PKG_VERSION").into();
    manifest.runtime_version = opts.runtime_version.clone();
    manifest.installed_at = now();
    manifest.files.insert(DLL.into(), new_hash);
    manifest.write(game_dir)?;
    Ok(outcome)
}

/// Relative path of a mod's downloaded pattern file.
pub fn pattern_rel(mod_id: &str) -> String {
    format!("{DATA_DIR}/patterns/{mod_id}.toml")
}

/// Writes a downloaded pattern file and records it in `install.toml`.
pub fn install_pattern(game_dir: &Path, mod_id: &str, bytes: &[u8]) -> Result<(), String> {
    if mod_id.is_empty()
        || !mod_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(tr!("err_bad_mod_id", mod_id));
    }
    let mut manifest = InstallManifest::read(game_dir)?.ok_or_else(|| tr!("err_not_installed"))?;
    let rel = pattern_rel(mod_id);
    write_atomic(&game_dir.join(&rel), bytes)?;
    manifest.files.insert(rel, sha256_hex(bytes));
    manifest.write(game_dir)
}

pub struct UninstallOpts {
    /// Remove files even if they changed since install, and a `winmm.dll`
    /// without an install record.
    pub force: bool,
    /// Also remove settings and everything else in `uk_qol/` except save
    /// sidecars.
    pub purge: bool,
}

#[derive(Debug, Default, PartialEq)]
pub struct UninstallOutcome {
    pub removed: Vec<String>,
    /// Kept because they changed since install (relative paths).
    pub kept_modified: Vec<String>,
    pub restored_backup: bool,
    /// `uk_qol/saves/` still holds files.
    pub sidecars_kept: bool,
    pub config_kept: bool,
}

fn remove_file(game_dir: &Path, rel: &str, out: &mut UninstallOutcome) -> Result<(), String> {
    let p = game_dir.join(rel);
    match std::fs::remove_file(&p) {
        Ok(()) => {
            out.removed.push(rel.to_string());
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(tr!("err_remove", p.display(), e)),
    }
}

/// Removes `dir` if it is empty (ignores errors).
fn remove_if_empty(dir: &Path) {
    if std::fs::read_dir(dir).is_ok_and(|mut rd| rd.next().is_none()) {
        let _ = std::fs::remove_dir(dir);
    }
}

/// Removes the loader from `game_dir`.
pub fn uninstall(game_dir: &Path, opts: &UninstallOpts) -> Result<UninstallOutcome, String> {
    let manifest = InstallManifest::read(game_dir)?;
    let state = dll_state(game_dir, manifest.as_ref());
    let mut out = UninstallOutcome::default();
    match (&manifest, &state) {
        (None, DllState::Missing) => return Err(tr!("err_not_installed")),
        (None, _) if !opts.force => return Err(tr!("err_no_record")),
        (Some(_), DllState::Modified | DllState::Foreign) if !opts.force => {
            return Err(tr!("err_dll_changed"));
        }
        _ => {}
    }

    if let Some(m) = &manifest {
        for (rel, want) in &m.files {
            if rel.contains("..") || Path::new(rel).is_absolute() {
                continue;
            }
            let p = game_dir.join(rel);
            if !p.is_file() {
                continue;
            }
            let matches = sha256_file(&p).is_ok_and(|h| &h == want);
            if matches || opts.force {
                remove_file(game_dir, rel, &mut out)?;
            } else {
                out.kept_modified.push(rel.clone());
            }
        }
    } else {
        remove_file(game_dir, DLL, &mut out)?;
    }

    // Put back a winmm.dll we replaced.
    let backup = game_dir.join(BACKUP_DLL);
    if backup.is_file() && !game_dir.join(DLL).exists() {
        std::fs::rename(&backup, game_dir.join(DLL))
            .map_err(|e| tr!("err_write", game_dir.join(DLL).display(), e))?;
        out.restored_backup = true;
    }

    for rel in RUNTIME_FILES {
        remove_file(game_dir, rel, &mut out)?;
    }
    if manifest.is_some() {
        remove_file(game_dir, MANIFEST, &mut out)?;
    }

    let data = game_dir.join(DATA_DIR);
    if opts.purge {
        purge_data_dir(game_dir, &mut out)?;
    }
    remove_if_empty(&data.join("patterns"));
    remove_if_empty(&data.join("backup"));
    remove_if_empty(&game_dir.join(SAVES_DIR));
    out.sidecars_kept = game_dir.join(SAVES_DIR).is_dir();
    out.config_kept = crate::config::path(game_dir).is_file();
    remove_if_empty(&data);
    Ok(out)
}

/// Everything in `uk_qol/` except `saves/`, one level of folders deep;
/// anything deeper than that is left alone.
fn purge_data_dir(game_dir: &Path, out: &mut UninstallOutcome) -> Result<(), String> {
    let data = game_dir.join(DATA_DIR);
    let Ok(rd) = std::fs::read_dir(&data) else {
        return Ok(());
    };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name == "saves" {
            continue;
        }
        let ft = e
            .file_type()
            .map_err(|err| tr!("err_read", e.path().display(), err))?;
        if ft.is_file() {
            remove_file(game_dir, &format!("{DATA_DIR}/{name}"), out)?;
        } else if ft.is_dir() {
            let sub: Vec<PathBuf> = std::fs::read_dir(e.path())
                .map(|rd| rd.flatten().map(|x| x.path()).collect())
                .unwrap_or_default();
            for f in sub.iter().filter(|f| f.is_file()) {
                let fname = f.file_name().unwrap_or_default().to_string_lossy();
                remove_file(game_dir, &format!("{DATA_DIR}/{name}/{fname}"), out)?;
            }
            remove_if_empty(&e.path());
        }
    }
    Ok(())
}

/// The `winmm.dll` to install when none is given: next to the manager
/// executable (the release zip ships them side by side).
pub fn bundled_dll() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let p = exe.parent()?.join(DLL);
    p.is_file().then_some(p)
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// Smallest PE the runtime's header parser accepts, flagged as a DLL.
    pub fn fake_dll(tag: u8) -> Vec<u8> {
        let mut b = vec![0u8; 0x400];
        b[0] = b'M';
        b[1] = b'Z';
        b[0x3c] = 0x80;
        b[0x80..0x84].copy_from_slice(b"PE\0\0");
        b[0x84..0x86].copy_from_slice(&0x8664u16.to_le_bytes()); // machine
        b[0x86..0x88].copy_from_slice(&1u16.to_le_bytes()); // sections
        b[0x94..0x96].copy_from_slice(&0xF0u16.to_le_bytes()); // optional header size
        b[0x96..0x98].copy_from_slice(&0x2022u16.to_le_bytes()); // DLL | exe | large addr
        let opt = 0x98;
        b[opt..opt + 2].copy_from_slice(&0x20Bu16.to_le_bytes()); // PE32+
        b[opt + 56..opt + 60].copy_from_slice(&0x2000u32.to_le_bytes()); // size of image
        b[opt + 60..opt + 64].copy_from_slice(&0x200u32.to_le_bytes()); // size of headers
        let sec = opt + 0xF0;
        b[sec..sec + 5].copy_from_slice(b".text");
        b[sec + 8..sec + 12].copy_from_slice(&0x100u32.to_le_bytes()); // vsize
        b[sec + 12..sec + 16].copy_from_slice(&0x1000u32.to_le_bytes()); // rva
        b[sec + 16..sec + 20].copy_from_slice(&0x100u32.to_le_bytes()); // raw size
        b[sec + 20..sec + 24].copy_from_slice(&0x200u32.to_le_bytes()); // raw ptr
        b[sec + 36..sec + 40].copy_from_slice(&0x6000_0020u32.to_le_bytes());
        b[0x200] = tag;
        b
    }

    pub fn fake_game() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join(crate::paths::GAME_EXE), b"MZ not really").unwrap();
        tmp
    }

    fn opts(dll: &[u8], force: bool) -> InstallOpts<'_> {
        InstallOpts {
            dll,
            runtime_version: "0.1.0".into(),
            force,
        }
    }

    #[test]
    fn fake_dll_is_a_dll() {
        assert!(looks_like_dll(&fake_dll(1)));
        assert!(!looks_like_dll(b"MZ nope"));
        assert!(!looks_like_dll(&[]));
    }

    #[test]
    fn install_then_uninstall_leaves_nothing_but_kept_files() {
        let game = fake_game();
        let g = game.path();
        let dll = fake_dll(1);
        assert!(install(&g.join("nope"), &opts(&dll, false)).is_err());

        let out = install(g, &opts(&dll, false)).unwrap();
        assert_eq!(out, InstallOutcome::default());
        assert_eq!(std::fs::read(g.join(DLL)).unwrap(), dll);
        let m = InstallManifest::read(g).unwrap().unwrap();
        assert_eq!(m.files.get(DLL), Some(&sha256_hex(&dll)));
        assert_eq!(dll_state(g, Some(&m)), DllState::Ours);

        // Update in place.
        let dll2 = fake_dll(2);
        assert!(install(g, &opts(&dll2, false)).unwrap().updated);

        // Runtime files, a pattern update, settings and a sidecar.
        install_pattern(g, "skip-logos", b"x = \"C3\"").unwrap();
        std::fs::write(g.join("uk_qol/uk_qol.log"), "log").unwrap();
        std::fs::write(g.join("uk_qol/config.toml"), "[mods.a]\nenabled = true\n").unwrap();
        std::fs::create_dir_all(g.join(SAVES_DIR).join("12345")).unwrap();
        std::fs::write(g.join(SAVES_DIR).join("12345/save_1.qol.toml"), "x").unwrap();

        let out = uninstall(
            g,
            &UninstallOpts {
                force: false,
                purge: false,
            },
        )
        .unwrap();
        assert!(out.removed.contains(&DLL.to_string()));
        assert!(out.removed.contains(&pattern_rel("skip-logos")));
        assert!(out.removed.contains(&MANIFEST.to_string()));
        assert!(out.sidecars_kept && out.config_kept);
        assert!(!g.join(DLL).exists());
        assert!(!g.join("uk_qol/patterns").exists());
        assert!(g.join("uk_qol/config.toml").exists());
        assert!(g.join(SAVES_DIR).join("12345/save_1.qol.toml").exists());
        assert!(g.join(crate::paths::GAME_EXE).exists());

        // Nothing installed any more.
        assert!(
            uninstall(
                g,
                &UninstallOpts {
                    force: false,
                    purge: false
                }
            )
            .is_err()
        );
    }

    #[test]
    fn purge_keeps_sidecars_only() {
        let game = fake_game();
        let g = game.path();
        install(g, &opts(&fake_dll(1), false)).unwrap();
        std::fs::write(g.join("uk_qol/config.toml"), "").unwrap();
        std::fs::write(g.join("uk_qol/other.txt"), "").unwrap();
        std::fs::create_dir_all(g.join(SAVES_DIR)).unwrap();
        std::fs::write(g.join(SAVES_DIR).join("s.qol.toml"), "x").unwrap();
        let out = uninstall(
            g,
            &UninstallOpts {
                force: false,
                purge: true,
            },
        )
        .unwrap();
        assert!(!out.config_kept && out.sidecars_kept);
        let left: Vec<_> = std::fs::read_dir(g.join(DATA_DIR))
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert_eq!(left, vec![std::ffi::OsString::from("saves")]);

        // Without sidecars, the folder goes completely.
        let game = fake_game();
        let g = game.path();
        install(g, &opts(&fake_dll(1), false)).unwrap();
        uninstall(
            g,
            &UninstallOpts {
                force: false,
                purge: true,
            },
        )
        .unwrap();
        assert!(!g.join(DATA_DIR).exists());
    }

    #[test]
    fn foreign_dll_is_refused_then_backed_up_and_restored() {
        let game = fake_game();
        let g = game.path();
        std::fs::write(g.join(DLL), b"someone else's dll").unwrap();
        assert!(
            uninstall(
                g,
                &UninstallOpts {
                    force: false,
                    purge: false
                }
            )
            .is_err()
        );
        assert!(install(g, &opts(&fake_dll(1), false)).is_err());
        assert_eq!(std::fs::read(g.join(DLL)).unwrap(), b"someone else's dll");

        let out = install(g, &opts(&fake_dll(1), true)).unwrap();
        assert!(out.backed_up);
        let out = uninstall(
            g,
            &UninstallOpts {
                force: false,
                purge: false,
            },
        )
        .unwrap();
        assert!(out.restored_backup);
        assert_eq!(std::fs::read(g.join(DLL)).unwrap(), b"someone else's dll");
        assert!(!g.join(DATA_DIR).exists());
    }

    #[test]
    fn changed_dll_needs_force() {
        let game = fake_game();
        let g = game.path();
        install(g, &opts(&fake_dll(1), false)).unwrap();
        std::fs::write(g.join(DLL), b"changed").unwrap();
        let m = InstallManifest::read(g).unwrap();
        assert_eq!(dll_state(g, m.as_ref()), DllState::Modified);
        assert!(
            uninstall(
                g,
                &UninstallOpts {
                    force: false,
                    purge: false
                }
            )
            .is_err()
        );
        assert!(g.join(DLL).exists());
        uninstall(
            g,
            &UninstallOpts {
                force: true,
                purge: false,
            },
        )
        .unwrap();
        assert!(!g.join(DLL).exists());
    }

    #[test]
    fn identical_dll_copied_by_hand_is_adopted() {
        let game = fake_game();
        let g = game.path();
        let dll = fake_dll(7);
        std::fs::write(g.join(DLL), &dll).unwrap();
        install(g, &opts(&dll, false)).unwrap();
        let m = InstallManifest::read(g).unwrap().unwrap();
        assert!(m.backup.is_none());
    }

    #[test]
    fn manifest_round_trips_and_ignores_unknown_fields() {
        let game = fake_game();
        let g = game.path();
        let mut m = InstallManifest {
            manager_version: "0.1.0".into(),
            runtime_version: "unknown".into(),
            installed_at: 5,
            backup: None,
            files: BTreeMap::new(),
        };
        m.files.insert(DLL.into(), "ab".into());
        m.write(g).unwrap();
        let text = std::fs::read_to_string(g.join(MANIFEST)).unwrap();
        assert!(text.contains("[files]"));
        assert!(text.contains("\"winmm.dll\" = \"ab\"") || text.contains("\"winmm.dll\" = 'ab'"));
        std::fs::write(g.join(MANIFEST), format!("future = 1\n{text}")).unwrap();
        assert_eq!(InstallManifest::read(g).unwrap().unwrap(), m);
    }
}
