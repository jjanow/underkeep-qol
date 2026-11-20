//! `uk-qol report`: one plain-text file with everything a bug report needs
//! (versions, game build, install record, settings, mod compatibility, the
//! runtime log). Plain text so the player can read what they're about to
//! share and GitHub takes it as an attachment.
//!
//! It goes into a public issue, so the home folder is replaced by `~` and
//! Steam account ids and Steam's own config files are left out.

use std::fmt::Write as _;
use std::path::Path;

use crate::install::{self, DllState};
use crate::launch::LaunchState;
use crate::mods::Compat;
use crate::status::Status;

/// Replaces the home folder in `text` with `~`.
pub fn redact(text: &str, home: Option<&Path>) -> String {
    let Some(home) = home.map(|h| h.to_string_lossy().into_owned()) else {
        return text.to_string();
    };
    if home.len() < 2 {
        return text.to_string();
    }
    let mut out = text.replace(&home, "~");
    // Wine shows the same folder as Z:\home\user.
    let wine = format!("Z:{}", home.replace('/', "\\"));
    out = out.replace(&wine, "Z:~");
    out
}

fn os_info() -> String {
    let mut s = format!("{} {}", std::env::consts::OS, std::env::consts::ARCH);
    #[cfg(target_os = "linux")]
    if let Ok(t) = std::fs::read_to_string("/etc/os-release")
        && let Some(name) = t.lines().find_map(|l| l.strip_prefix("PRETTY_NAME="))
    {
        let _ = write!(s, " ({})", name.trim_matches('"'));
    }
    if std::env::var_os("SteamDeck").is_some() {
        s.push_str(", Steam Deck");
    }
    s
}

/// DLLs the game doesn't ship (`version.dll` of another mod framework, a
/// ReShade `dxgi.dll`...) and other loaders' folders next to the exe, sorted.
/// Another loader can explain a crash or a mod that misbehaves.
fn other_loaders(game: &Path) -> Vec<String> {
    const SHIPPED: [&str; 6] = [
        "bugsplat64.dll",
        "bugsplatrc64.dll",
        "fmod.dll",
        "sdl3.dll",
        "steam_api64.dll",
        "winmm.dll",
    ];
    const FOLDERS: [&str; 1] = ["underkeepmodframework"];
    let Ok(dir) = std::fs::read_dir(game) else {
        return Vec::new();
    };
    let mut out: Vec<String> = dir
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let lower = name.to_ascii_lowercase();
            let is_dir = e.file_type().is_ok_and(|t| t.is_dir());
            let dll = !is_dir && lower.ends_with(".dll") && !SHIPPED.contains(&lower.as_str());
            let loader_dir = is_dir && FOLDERS.contains(&lower.as_str());
            (dll || loader_dir).then(|| if is_dir { format!("{name}/") } else { name })
        })
        .collect();
    out.sort();
    out
}

pub fn build_report(st: &Status) -> String {
    let mut r = String::new();
    let _ = writeln!(r, "Underkeep QoL report");
    let _ = writeln!(r, "====================");
    let _ = writeln!(r, "manager: uk-qol {}", env!("CARGO_PKG_VERSION"));
    let _ = writeln!(r, "system:  {}", os_info());
    let _ = writeln!(
        r,
        "game:    {} [{}]",
        st.install.dir.display(),
        st.install.source
    );
    match &st.build {
        Ok(b) => {
            let _ = writeln!(
                r,
                "build:   version {}, exe sha256 {}",
                b.version.as_deref().unwrap_or("unknown"),
                b.sha256_hex()
            );
        }
        Err(e) => {
            let _ = writeln!(r, "build:   unknown ({e})");
        }
    }
    let dll = match st.dll {
        DllState::Missing => "not installed",
        DllState::Ours => "installed by uk-qol",
        DllState::Modified => "changed since install",
        DllState::Foreign => "present, not installed by uk-qol",
    };
    let _ = writeln!(r, "winmm.dll: {dll}");
    let others = other_loaders(&st.install.dir);
    let _ = writeln!(
        r,
        "other mod loaders or DLLs in the game folder: {}",
        if others.is_empty() {
            "none".to_string()
        } else {
            others.join(", ")
        }
    );
    let _ = writeln!(
        r,
        "abnormal exits in a row: {}{}",
        st.abnormal_exits,
        if st.safe_mode_next {
            " (next start in safe mode)"
        } else {
            ""
        }
    );
    for a in &st.launch {
        let state = match &a.state {
            LaunchState::Set => "set".to_string(),
            LaunchState::Empty => "not set".to_string(),
            LaunchState::Other(v) => format!("not set (other options: {v})"),
            LaunchState::NoApp => "no entry for the game".to_string(),
            LaunchState::Unreadable(e) => format!("unreadable ({e})"),
        };
        let _ = writeln!(r, "launch option (a Steam account): {state}");
    }

    let _ = writeln!(r, "\n--- mods ---");
    for m in &st.mods {
        let compat = match &m.compat {
            Compat::Works => "patterns match".to_string(),
            Compat::Broken(f) => format!(
                "patterns fail: {}",
                f.iter()
                    .map(|(n, e)| format!("{n} ({e})"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Compat::Unknown(e) => format!("not checked ({e})"),
        };
        let _ = writeln!(
            r,
            "{} {}: {}, {}{}",
            m.info.id,
            m.info.version,
            if m.enabled { "on" } else { "off" },
            compat,
            if m.pattern_override {
                ", downloaded patterns"
            } else {
                ""
            }
        );
    }

    let _ = writeln!(r, "\n--- {} ---", install::MANIFEST);
    match &st.manifest {
        Ok(Some(_)) => {
            let text =
                std::fs::read_to_string(st.install.dir.join(install::MANIFEST)).unwrap_or_default();
            r.push_str(&text);
        }
        Ok(None) => r.push_str("(none)\n"),
        Err(e) => {
            let _ = writeln!(r, "({e})");
        }
    }

    let _ = writeln!(r, "\n--- uk_qol/config.toml ---");
    match &st.config {
        Ok(c) if c.path.is_file() => r.push_str(&c.text()),
        Ok(_) => r.push_str("(none)\n"),
        Err(e) => {
            let _ = writeln!(r, "({e})");
        }
    }

    let _ = writeln!(r, "\n--- uk_qol/uk_qol.log ---");
    match &st.log {
        Some(l) => {
            if l.other_build {
                let _ = writeln!(r, "(written by a different game build)");
            }
            // The tail is what matters; keep reports a reasonable size.
            let skip = l.lines.len().saturating_sub(2000);
            if skip > 0 {
                let _ = writeln!(r, "({skip} earlier lines left out)");
            }
            for line in &l.lines[skip..] {
                r.push_str(line);
                r.push('\n');
            }
        }
        None => r.push_str("(none)\n"),
    }
    redact(&r, dirs::home_dir().as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_home() {
        let home = Path::new("/home/someone");
        let t = "game: /home/someone/.steam/x\nZ:\\home\\someone\\x\\underkeep.exe\n/home/other";
        let r = redact(t, Some(home));
        assert_eq!(r, "game: ~/.steam/x\nZ:~\\x\\underkeep.exe\n/home/other");
    }

    #[test]
    fn report_for_fake_install() {
        let game = crate::install::tests::fake_game();
        let g = game.path();
        crate::install::install(
            g,
            &crate::install::InstallOpts {
                dll: &crate::install::tests::fake_dll(1),
                runtime_version: "0.1.0".into(),
                force: false,
            },
        )
        .unwrap();
        std::fs::write(g.join("uk_qol/uk_qol.log"), "uk-qol 0.1.0 loaded into x\n").unwrap();
        let st = crate::status::gather(&crate::paths::GameInstall {
            dir: g.to_path_buf(),
            steam_root: None,
            source: "test".into(),
        });
        let r = build_report(&st);
        assert!(r.contains("winmm.dll: installed by uk-qol"));
        assert!(r.contains("[files]"));
        assert!(r.contains("loaded into x"));
        assert!(r.contains("other mod loaders or DLLs in the game folder: none"));
    }

    #[test]
    fn lists_other_loaders() {
        let game = crate::install::tests::fake_game();
        let g = game.path();
        for f in [
            "SDL3.dll",
            "winmm.dll",
            "version.dll",
            "dxgi.DLL",
            "notes.txt",
        ] {
            std::fs::write(g.join(f), b"x").unwrap();
        }
        std::fs::create_dir(g.join("UnderkeepModFramework")).unwrap();
        std::fs::create_dir(g.join("mods")).unwrap();
        assert_eq!(
            other_loaders(g),
            ["UnderkeepModFramework/", "dxgi.DLL", "version.dll"]
        );
    }
}
