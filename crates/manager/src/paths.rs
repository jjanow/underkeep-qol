//! Finding Underkeep's Steam installs on Windows, Linux (native Steam,
//! Flatpak, Snap, Proton) and macOS (CrossOver / Whisky / Wine bottles).
//!
//! Adapted from Underkeeper's `paths.rs` (same author, MIT); library folders
//! and app manifests are read with the VDF parser in [`crate::vdf`].

use std::path::{Path, PathBuf};

use crate::vdf;

pub const APP_ID: &str = "2411200";
pub const GAME_EXE: &str = "underkeep.exe";

/// One Underkeep install.
#[derive(Clone, Debug, PartialEq)]
pub struct GameInstall {
    pub dir: PathBuf,
    /// The Steam installation that owns it (has `userdata/`), if known.
    pub steam_root: Option<PathBuf>,
    /// Short origin, e.g. "Steam (Flatpak)".
    pub source: String,
}

/// Does this folder hold the game?
pub fn is_game_dir(p: &Path) -> bool {
    p.join(GAME_EXE).is_file()
}

fn push_unique(v: &mut Vec<PathBuf>, p: PathBuf) {
    let canon = std::fs::canonicalize(&p).unwrap_or(p);
    if !v.contains(&canon) {
        v.push(canon);
    }
}

/// Wine/CrossOver/Whisky prefixes that might contain a Windows Steam install.
fn wine_prefixes() -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Some(home) = dirs::home_dir() else {
        return out;
    };
    out.push(home.join(".wine"));
    let bottle_parents = [
        home.join("Library/Application Support/CrossOver/Bottles"),
        home.join("Library/Containers/com.isaacmarovitz.Whisky/Bottles"),
        home.join("Library/Application Support/com.isaacmarovitz.Whisky/Bottles"),
        home.join(".local/share/bottles/bottles"),
        home.join(".var/app/com.usebottles.bottles/data/bottles/bottles"),
    ];
    for parent in bottle_parents {
        if let Ok(rd) = std::fs::read_dir(&parent) {
            for e in rd.flatten() {
                let p = e.path();
                if p.join("drive_c").is_dir() {
                    out.push(p);
                } else if p.join("pfx/drive_c").is_dir() {
                    out.push(p.join("pfx"));
                }
            }
        }
    }
    out
}

/// Candidate Steam installation roots for the current platform.
pub fn steam_roots() -> Vec<PathBuf> {
    let mut out = Vec::new();

    #[cfg(windows)]
    {
        use winreg::RegKey;
        use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
        if let Ok(k) = RegKey::predef(HKEY_CURRENT_USER).open_subkey("Software\\Valve\\Steam")
            && let Ok(p) = k.get_value::<String, _>("SteamPath")
        {
            push_unique(&mut out, PathBuf::from(p));
        }
        for sub in [
            "SOFTWARE\\WOW6432Node\\Valve\\Steam",
            "SOFTWARE\\Valve\\Steam",
        ] {
            if let Ok(k) = RegKey::predef(HKEY_LOCAL_MACHINE).open_subkey(sub)
                && let Ok(p) = k.get_value::<String, _>("InstallPath")
            {
                push_unique(&mut out, PathBuf::from(p));
            }
        }
        for var in ["ProgramFiles(x86)", "ProgramFiles"] {
            if let Ok(pf) = std::env::var(var) {
                push_unique(&mut out, PathBuf::from(pf).join("Steam"));
            }
        }
    }

    if let Some(home) = dirs::home_dir() {
        for rel in [
            ".steam/steam",
            ".steam/root",
            ".local/share/Steam",
            ".var/app/com.valvesoftware.Steam/.local/share/Steam",
            "snap/steam/common/.local/share/Steam",
            "Library/Application Support/Steam",
        ] {
            let p = home.join(rel);
            if p.is_dir() {
                push_unique(&mut out, p);
            }
        }
    }

    for prefix in wine_prefixes() {
        for rel in [
            "drive_c/Program Files (x86)/Steam",
            "drive_c/Program Files/Steam",
        ] {
            let p = prefix.join(rel);
            if p.is_dir() {
                push_unique(&mut out, p);
            }
        }
    }

    out.retain(|p| p.is_dir());
    out
}

/// Library paths listed in a `libraryfolders.vdf` text. Handles the current
/// format (`"0" { "path" "..." }`) and the old one (`"1" "D:\\Lib"`).
pub fn parse_library_folders(text: &str) -> Vec<String> {
    let Ok(doc) = vdf::parse(text) else {
        return Vec::new();
    };
    let Some(vdf::Entry {
        value: vdf::Value::Obj(libs),
        ..
    }) = vdf::find(&doc, "libraryfolders").or_else(|| vdf::find(&doc, "LibraryFolders"))
    else {
        return Vec::new();
    };
    libs.iter()
        .filter(|e| e.key.bytes().all(|b| b.is_ascii_digit()))
        .filter_map(|e| match &e.value {
            vdf::Value::Str(s) => Some(s.clone()),
            vdf::Value::Obj(o) => vdf::str_at(o, &["path"]).map(str::to_string),
        })
        .collect()
}

/// A Windows path read by a native (non-Windows) build inside a Wine bottle:
/// maps `C:\...` onto the bottle's `drive_c`.
fn map_wine_path(root: &Path, raw: &str) -> PathBuf {
    if !cfg!(windows)
        && raw.len() > 2
        && &raw[1..2] == ":"
        && raw[..1].eq_ignore_ascii_case("c")
        && let Some(drive_c) = root.ancestors().find(|a| a.ends_with("drive_c"))
    {
        return drive_c.join(raw[3..].replace('\\', "/"));
    }
    PathBuf::from(raw)
}

/// Steam library folders of `root`, the root itself first.
pub fn library_folders(root: &Path) -> Vec<PathBuf> {
    let mut out = vec![root.to_path_buf()];
    for file in [
        root.join("steamapps/libraryfolders.vdf"),
        root.join("config/libraryfolders.vdf"),
    ] {
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        for raw in parse_library_folders(&text) {
            let p = map_wine_path(root, &raw);
            if p.is_dir() {
                push_unique(&mut out, p);
            }
        }
    }
    out
}

/// The game's folder name from `appmanifest_2411200.acf`, if present.
pub fn parse_installdir(acf: &str) -> Option<String> {
    let doc = vdf::parse(acf).ok()?;
    vdf::str_at(&doc, &["AppState", "installdir"])
        .filter(|s| !s.is_empty() && !s.contains(['/', '\\']) && *s != "..")
        .map(str::to_string)
}

/// Where the game would be in library `lib`.
fn game_dir_in_library(lib: &Path) -> PathBuf {
    let acf = lib.join(format!("steamapps/appmanifest_{APP_ID}.acf"));
    let name = std::fs::read_to_string(acf)
        .ok()
        .and_then(|t| parse_installdir(&t))
        .unwrap_or_else(|| "Underkeep".into());
    lib.join("steamapps/common").join(name)
}

/// "Steam", "Steam (Flatpak)", ... for a Steam root.
pub fn source_label(root: &Path) -> String {
    let s = root.to_string_lossy();
    let kind = if root.ancestors().any(|a| a.ends_with("drive_c")) {
        if s.contains("CrossOver") {
            Some("CrossOver")
        } else if s.contains("Whisky") {
            Some("Whisky")
        } else {
            Some("Wine")
        }
    } else if s.contains("com.valvesoftware.Steam") {
        Some("Flatpak")
    } else if s.contains("/snap/steam/") {
        Some("Snap")
    } else {
        None
    };
    match kind {
        Some(k) => format!("Steam ({k})"),
        None => "Steam".into(),
    }
}

/// Every Underkeep install found, in Steam root order.
pub fn find_installs() -> Vec<GameInstall> {
    let mut out: Vec<GameInstall> = Vec::new();
    for root in steam_roots() {
        for lib in library_folders(&root) {
            let dir = game_dir_in_library(&lib);
            if !is_game_dir(&dir) {
                continue;
            }
            let dir = std::fs::canonicalize(&dir).unwrap_or(dir);
            if out.iter().any(|g| g.dir == dir) {
                continue;
            }
            out.push(GameInstall {
                dir,
                steam_root: Some(root.clone()),
                source: source_label(&root),
            });
        }
    }
    out
}

/// Describes a folder the user picked: finds the Steam root that owns it, if
/// it sits in a Steam library.
pub fn describe(dir: &Path) -> GameInstall {
    let dir = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    if let Some(g) = find_installs().into_iter().find(|g| g.dir == dir) {
        return g;
    }
    // <lib>/steamapps/common/<game>: is <lib> a library of a known root?
    let lib = dir.parent().and_then(Path::parent).and_then(Path::parent);
    let steam_root = lib.and_then(|lib| {
        steam_roots()
            .into_iter()
            .find(|r| library_folders(r).iter().any(|l| l == lib))
    });
    GameInstall {
        source: steam_root
            .as_deref()
            .map_or_else(|| "folder".into(), source_label),
        dir,
        steam_root,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn library_folders_both_formats() {
        let new = r#"
"libraryfolders"
{
	"0"
	{
		"path"		"/data/Steam"
		"label"		""
		"apps"
		{
			"2411200"		"123"
		}
	}
	"1"
	{
		"path"		"D:\\SteamLibrary"
	}
}"#;
        assert_eq!(
            parse_library_folders(new),
            vec!["/data/Steam".to_string(), r"D:\SteamLibrary".to_string()]
        );
        let old = r#"
"LibraryFolders"
{
	"TimeNextStatsReport"		"1"
	"ContentStatsID"		"2"
	"1"		"E:\\Games\\Steam"
}"#;
        assert_eq!(
            parse_library_folders(old),
            vec![r"E:\Games\Steam".to_string()]
        );
        assert!(parse_library_folders("garbage {").is_empty());
    }

    #[test]
    fn installdir_from_app_manifest() {
        let acf = r#"
"AppState"
{
	"appid"		"2411200"
	"name"		"Underkeep"
	"installdir"		"Underkeep"
}"#;
        assert_eq!(parse_installdir(acf).as_deref(), Some("Underkeep"));
        assert_eq!(
            parse_installdir("\"AppState\" { \"installdir\" \"../x\" }"),
            None
        );
    }

    #[test]
    fn finds_game_in_fake_library() {
        let tmp = tempfile::tempdir().unwrap();
        let lib = tmp.path();
        assert!(!is_game_dir(&game_dir_in_library(lib)));
        std::fs::create_dir_all(lib.join("steamapps/common/UK")).unwrap();
        std::fs::write(
            lib.join(format!("steamapps/appmanifest_{APP_ID}.acf")),
            "\"AppState\" { \"installdir\" \"UK\" }",
        )
        .unwrap();
        std::fs::write(lib.join("steamapps/common/UK").join(GAME_EXE), b"MZ").unwrap();
        assert!(is_game_dir(&game_dir_in_library(lib)));
    }

    #[test]
    fn labels() {
        assert_eq!(
            source_label(Path::new(
                "/home/u/.var/app/com.valvesoftware.Steam/.local/share/Steam"
            )),
            "Steam (Flatpak)"
        );
        assert_eq!(
            source_label(Path::new(
                "/Users/u/Library/Application Support/CrossOver/Bottles/S/drive_c/Program Files (x86)/Steam"
            )),
            "Steam (CrossOver)"
        );
        assert_eq!(source_label(Path::new("/opt/steam")), "Steam");
    }

    #[test]
    fn wine_paths_map_to_drive_c() {
        let root = Path::new("/b/drive_c/Program Files (x86)/Steam");
        if cfg!(windows) {
            return;
        }
        assert_eq!(
            map_wine_path(root, r"C:\Games\Lib"),
            PathBuf::from("/b/drive_c/Games/Lib")
        );
        assert_eq!(map_wine_path(root, "/x"), PathBuf::from("/x"));
    }
}
