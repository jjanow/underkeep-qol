//! Is the game (or Steam) running? Files are never changed while the game
//! runs, and Steam's config only while Steam is closed.
//!
//! Linux reads `/proc` directly; other systems use `sysinfo`.

/// One running process as seen by the matchers.
#[derive(Debug, Default, Clone)]
pub struct Proc {
    /// Short name (`/proc/<pid>/comm`, or the process name).
    pub name: String,
    /// File name of the executable, if readable.
    pub exe: String,
    /// The first command-line argument (Wine shows the Windows path here).
    pub argv0: String,
}

fn file_name_of(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

/// Underkeep itself, natively or under Wine/Proton
/// (`Z:\...\underkeep.exe` as argv0).
pub fn is_game(p: &Proc) -> bool {
    let is = |s: &str| file_name_of(s).eq_ignore_ascii_case(crate::paths::GAME_EXE);
    is(&p.name) || is(&p.exe) || is(&p.argv0)
}

/// The Steam client (native, Flatpak, or `steam.exe` in a bottle).
pub fn is_steam(p: &Proc) -> bool {
    let is = |s: &str| {
        let n = file_name_of(s);
        n == "steam" || n.eq_ignore_ascii_case("steam.exe") || n == "steam_osx"
    };
    is(&p.name) || is(&p.exe) || is(&p.argv0)
}

#[cfg(target_os = "linux")]
pub fn processes() -> Vec<Proc> {
    let Ok(rd) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    rd.flatten()
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .bytes()
                .all(|b| b.is_ascii_digit())
        })
        .map(|e| {
            let dir = e.path();
            let name = std::fs::read_to_string(dir.join("comm"))
                .map(|s| s.trim_end().to_string())
                .unwrap_or_default();
            let exe = std::fs::read_link(dir.join("exe"))
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default();
            let argv0 = std::fs::read(dir.join("cmdline"))
                .ok()
                .and_then(|b| {
                    b.split(|&c| c == 0)
                        .next()
                        .map(|a| String::from_utf8_lossy(a).into_owned())
                })
                .unwrap_or_default();
            Proc { name, exe, argv0 }
        })
        .collect()
}

#[cfg(not(target_os = "linux"))]
pub fn processes() -> Vec<Proc> {
    use sysinfo::{ProcessRefreshKind, RefreshKind, System, UpdateKind};
    let sys = System::new_with_specifics(
        RefreshKind::nothing().with_processes(
            ProcessRefreshKind::nothing()
                .with_exe(UpdateKind::Always)
                .with_cmd(UpdateKind::Always),
        ),
    );
    let Ok(sys) = sys else {
        return Vec::new();
    };
    sys.processes()
        .values()
        .map(|p| Proc {
            name: p.name().to_string_lossy().into_owned(),
            exe: p
                .exe()
                .map(|e| e.to_string_lossy().into_owned())
                .unwrap_or_default(),
            argv0: p
                .cmd()
                .first()
                .map(|c| c.to_string_lossy().into_owned())
                .unwrap_or_default(),
        })
        .collect()
}

pub fn game_running() -> bool {
    processes().iter().any(is_game)
}

pub fn steam_running() -> bool {
    processes().iter().any(is_steam)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(name: &str, exe: &str, argv0: &str) -> Proc {
        Proc {
            name: name.into(),
            exe: exe.into(),
            argv0: argv0.into(),
        }
    }

    #[test]
    fn matches_game_natively_and_under_proton() {
        assert!(is_game(&p("underkeep.exe", "", "")));
        assert!(is_game(&p(
            "Underkeep.exe",
            "C:\\Games\\Underkeep\\Underkeep.exe",
            ""
        )));
        assert!(is_game(&p(
            "wine64-preload",
            "/proton/bin/wine64-preload",
            "Z:\\home\\u\\.local\\share\\Steam\\steamapps\\common\\Underkeep\\underkeep.exe"
        )));
        assert!(!is_game(&p("bash", "/usr/bin/bash", "uk-qol")));
        assert!(!is_game(&p("underkeeper", "/x/underkeeper", "underkeeper")));
        assert!(!is_game(&p("uk-qol", "/x/uk-qol", "/x/uk-qol")));
    }

    #[test]
    fn matches_steam() {
        assert!(is_steam(&p(
            "steam",
            "/home/u/.local/share/Steam/ubuntu12_32/steam",
            ""
        )));
        assert!(is_steam(&p(
            "steam.exe",
            "",
            "C:\\Program Files (x86)\\Steam\\steam.exe"
        )));
        assert!(!is_steam(&p("steamwebhelper", "", "")));
        assert!(!is_steam(&p("steam-runtime-launcher", "", "")));
    }

    #[test]
    fn process_list_is_readable() {
        // At least this test process must show up.
        assert!(!processes().is_empty());
    }
}
