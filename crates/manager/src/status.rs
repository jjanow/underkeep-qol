//! Everything known about one game folder: build, install, launch option,
//! mods (enabled, compatible, last runtime status). Read-only: gathering it
//! never creates or changes a file.

use std::path::Path;

use uk_qol_runtime::build::BuildInfo;
use uk_qol_runtime::pattern::Image;

use crate::config::ConfigFile;
use crate::install::{self, DllState, InstallManifest};
use crate::launch::{self, AccountLaunch};
use crate::mods::{self, Compat, ModInfo};
use crate::paths::{self, GameInstall};

pub struct ModStatus {
    pub info: ModInfo,
    pub enabled: bool,
    pub compat: Compat,
    /// The manifest's `game_builds` doesn't list this build (the mod still
    /// runs if its patterns resolve).
    pub untested: bool,
    /// Uses a downloaded pattern file instead of the built-in one.
    pub pattern_override: bool,
    /// The mod's last line in `uk_qol.log`, if any.
    pub last_log: Option<String>,
}

pub struct Status {
    pub install: GameInstall,
    pub build: Result<BuildInfo, String>,
    pub manifest: Result<Option<InstallManifest>, String>,
    pub dll: DllState,
    pub config: Result<ConfigFile, String>,
    /// Empty on Windows or when no Steam root is known.
    pub launch: Vec<AccountLaunch>,
    pub mods: Vec<ModStatus>,
    pub log: Option<LogSummary>,
    /// `uk_qol/session.toml`: the game will start in safe mode next time.
    pub safe_mode_next: bool,
    pub abnormal_exits: u32,
}

pub struct LogSummary {
    /// The runtime's "uk-qol <version> loaded" line.
    pub loaded: Option<String>,
    /// The log was written by a different game build than the current exe.
    pub other_build: bool,
    pub lines: Vec<String>,
}

pub fn read_log(game_dir: &Path, build: Option<&BuildInfo>) -> Option<LogSummary> {
    let text = std::fs::read_to_string(game_dir.join("uk_qol/uk_qol.log")).ok()?;
    let lines: Vec<String> = text.lines().map(str::to_string).collect();
    let loaded = lines.iter().find(|l| l.contains(" loaded into ")).cloned();
    let other_build = build.is_some_and(|b| {
        let hex = b.sha256_hex();
        lines.iter().any(|l| l.contains("exe sha256 ")) && !lines.iter().any(|l| l.contains(&hex))
    });
    Some(LogSummary {
        loaded,
        other_build,
        lines,
    })
}

/// The last log line about mod `id`: `mod <id>: ...` or
/// `mod <id> <version>: <status>` (the runtime's summary), or `[<id>] ...`.
pub fn last_mod_line(lines: &[String], id: &str) -> Option<String> {
    let needles = [
        format!("mod {id}:"),
        format!("mod {id} "),
        format!("[{id}]"),
    ];
    lines
        .iter()
        .rev()
        .find(|l| needles.iter().any(|n| l.contains(n.as_str())))
        .map(|l| {
            // Drop a leading timestamp ("[12.345] ...") if present.
            match l.strip_prefix('[').and_then(|r| r.split_once("] ")) {
                Some((ts, rest))
                    if ts
                        .bytes()
                        .all(|b| b.is_ascii_digit() || b == b'.' || b == b':') =>
                {
                    rest.to_string()
                }
                _ => l.clone(),
            }
        })
}

pub fn gather(g: &GameInstall) -> Status {
    let dir = &g.dir;
    let exe = dir.join(paths::GAME_EXE);
    let build = uk_qol_runtime::build::detect_from_file(&exe).map_err(|e| e.to_string());
    let manifest = InstallManifest::read(dir);
    let dll = install::dll_state(dir, manifest.as_ref().ok().and_then(Option::as_ref));
    let config = ConfigFile::load(&crate::config::path(dir));
    let launch = match (&g.steam_root, launch::needed()) {
        (Some(root), true) => launch::check(root),
        _ => Vec::new(),
    };
    let log = read_log(dir, build.as_ref().ok());
    let session = uk_qol_runtime::safe::read(&dir.join(install::DATA_DIR));
    let image = mods::load_image(&exe);
    let build_hex = build.as_ref().ok().map(|b| b.sha256_hex());
    let mods = mods::builtin()
        .into_iter()
        .map(|info| {
            let override_path = dir.join(install::pattern_rel(&info.id));
            let pattern_override = override_path.is_file();
            let compat = match &image {
                Ok(img) => match Image::new(img) {
                    Ok(image) => mods::check_patterns(
                        &image,
                        &mods::effective_patterns(&info, dir),
                        build_hex.as_deref(),
                    ),
                    Err(e) => Compat::Unknown(e.to_string()),
                },
                Err(e) => Compat::Unknown(e.clone()),
            };
            let enabled = config
                .as_ref()
                .map_or(info.enabled_default, |c| c.is_enabled(&info));
            let last_log = log.as_ref().and_then(|l| last_mod_line(&l.lines, &info.id));
            let untested = info.tested_on(build_hex.as_deref()) == Some(false);
            ModStatus {
                untested,
                info,
                enabled,
                compat,
                pattern_override,
                last_log,
            }
        })
        .collect();
    Status {
        install: g.clone(),
        build,
        manifest,
        dll,
        config,
        launch,
        mods,
        log,
        safe_mode_next: session
            .as_ref()
            .is_some_and(|s| s.abnormal_exits >= uk_qol_runtime::safe::CRASH_THRESHOLD),
        abnormal_exits: session.map_or(0, |s| s.abnormal_exits),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_mod_lines() {
        let lines: Vec<String> = [
            "[0.01] uk-qol 0.1.0 loaded into Z:\\x\\underkeep.exe (pid 1)",
            "[0.02] mod skip-logos: on",
            "[0.03] mod weapon-swap: off (pattern draw_slot not found)",
            "[9.00] mod skip-logos: off (turned off in overlay)",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(
            last_mod_line(&lines, "skip-logos").as_deref(),
            Some("mod skip-logos: off (turned off in overlay)")
        );
        assert!(
            last_mod_line(&lines, "weapon-swap")
                .unwrap()
                .contains("not found")
        );
        assert_eq!(last_mod_line(&lines, "inventory-sort"), None);
    }

    #[test]
    fn gather_on_a_fake_game_creates_nothing() {
        let game = crate::install::tests::fake_game();
        let g = GameInstall {
            dir: game.path().to_path_buf(),
            steam_root: None,
            source: "test".into(),
        };
        let s = gather(&g);
        assert!(s.build.is_err());
        assert_eq!(s.dll, DllState::Missing);
        assert!(matches!(s.manifest, Ok(None)));
        assert!(s.log.is_none());
        assert!(!game.path().join("uk_qol").exists());
    }
}
