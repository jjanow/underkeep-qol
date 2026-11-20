//! The Steam launch option Wine/Proton needs to load our `winmm.dll`
//! instead of its built-in one, read from (and, only when Steam is closed,
//! written to) `userdata/<account>/config/localconfig.vdf`.
//!
//! Windows doesn't need it: there the game folder's `winmm.dll` wins anyway.

use std::path::{Path, PathBuf};

use crate::paths::APP_ID;
use crate::tr;
use crate::vdf;

pub const LAUNCH_OPTION: &str = r#"WINEDLLOVERRIDES="winmm=n,b" %command%"#;

/// Whether the launch option matters on this system (the game runs in Wine).
pub fn needed() -> bool {
    !cfg!(windows)
}

const APP_PATH: [&str; 6] = [
    "UserLocalConfigStore",
    "Software",
    "Valve",
    "Steam",
    "apps",
    APP_ID,
];

#[derive(Debug, Clone, PartialEq)]
pub enum LaunchState {
    Set,
    /// Launch options are empty or absent.
    Empty,
    /// Launch options exist but don't load our DLL.
    Other(String),
    /// Steam has no entry for the game in this account.
    NoApp,
    Unreadable(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct AccountLaunch {
    pub account: String,
    pub config: PathBuf,
    pub state: LaunchState,
}

/// `WINEDLLOVERRIDES` loading winmm native-first, and `%command%`.
pub fn value_is_set(v: &str) -> bool {
    let lower = v.to_ascii_lowercase();
    let Some(i) = lower.find("winedlloverrides=") else {
        return false;
    };
    let rest = &lower[i..];
    let overrides = rest
        .split_whitespace()
        .next()
        .unwrap_or("")
        .trim_start_matches("winedlloverrides=")
        .trim_matches(['"', '\'']);
    let winmm_native = overrides.split(';').any(|part| {
        let (dlls, mode) = part.split_once('=').unwrap_or((part, ""));
        dlls.split(',').any(|d| d.trim() == "winmm") && mode.trim_start().starts_with('n')
    });
    winmm_native && lower.contains("%command%")
}

/// The override we add, inside a `WINEDLLOVERRIDES` value.
const WINMM_OVERRIDE: &str = "winmm=n,b";

/// `v` with our override added, keeping every option already there, or
/// `None` if `v` already names winmm with another mode (the player chose
/// that; we don't override it).
///
/// - `WINEDLLOVERRIDES` already set (another loader, such as
///   `version=n,b` for another mod framework): `;winmm=n,b` is appended
///   inside its value, keeping its quotes.
/// - No `WINEDLLOVERRIDES` but a `%command%`: ours goes in front.
/// - No `%command%` (Steam then appends the options to the game's command
///   line): ours and `%command%` go in front of them.
pub fn merged_value(v: &str) -> Option<String> {
    if value_is_set(v) {
        return Some(v.to_string());
    }
    let lower = v.to_ascii_lowercase();
    if let Some((start, end)) = overrides_span(&lower) {
        let overrides = &v[start..end];
        let names_winmm = overrides.split(';').any(|part| {
            let dlls = part.split_once('=').map_or(part, |(d, _)| d);
            dlls.split(',')
                .any(|d| d.trim().eq_ignore_ascii_case("winmm"))
        });
        if names_winmm {
            return None;
        }
        let sep = if overrides.trim().is_empty() || overrides.trim_end().ends_with(';') {
            ""
        } else {
            ";"
        };
        let out = format!("{}{sep}{WINMM_OVERRIDE}{}", &v[..end], &v[end..]);
        return if out.to_ascii_lowercase().contains("%command%") {
            Some(out)
        } else {
            Some(format!("{out} %command%").trim_start().to_string())
        };
    }
    let ours = format!("WINEDLLOVERRIDES=\"{WINMM_OVERRIDE}\"");
    if lower.contains("%command%") {
        Some(format!("{ours} {}", v.trim_start()))
    } else {
        Some(format!("{ours} %command% {}", v.trim()))
    }
}

/// Byte range of the `WINEDLLOVERRIDES` value in `lower` (lowercased launch
/// options), inside its quotes if it has any. The name must start a word.
fn overrides_span(lower: &str) -> Option<(usize, usize)> {
    const NAME: &str = "winedlloverrides=";
    let mut from = 0;
    while let Some(i) = lower[from..].find(NAME).map(|i| i + from) {
        let starts_word = lower[..i]
            .chars()
            .next_back()
            .is_none_or(char::is_whitespace);
        if starts_word {
            let mut start = i + NAME.len();
            let rest = &lower[start..];
            let end = match rest.chars().next() {
                Some(q @ ('"' | '\'')) => {
                    start += 1;
                    start + lower[start..].find(q)?
                }
                _ => start + rest.find(char::is_whitespace).unwrap_or(rest.len()),
            };
            return Some((start, end));
        }
        from = i + NAME.len();
    }
    None
}

/// Whether [`set_in_text`] can set the option for an account in `state`.
pub fn can_set(state: &LaunchState) -> bool {
    match state {
        LaunchState::Empty => true,
        LaunchState::Other(v) => merged_value(v).is_some(),
        _ => false,
    }
}

pub fn state_of(text: &str) -> LaunchState {
    let doc = match vdf::parse(text) {
        Ok(d) => d,
        Err(e) => return LaunchState::Unreadable(e),
    };
    if vdf::find_path(&doc, &APP_PATH).is_none() {
        return LaunchState::NoApp;
    }
    let mut path = APP_PATH.to_vec();
    path.push("LaunchOptions");
    match vdf::str_at(&doc, &path) {
        None => LaunchState::Empty,
        Some(v) if v.trim().is_empty() => LaunchState::Empty,
        Some(v) if value_is_set(v) => LaunchState::Set,
        Some(v) => LaunchState::Other(v.to_string()),
    }
}

/// Every Steam account under `steam_root` that has a `localconfig.vdf`.
pub fn accounts(steam_root: &Path) -> Vec<(String, PathBuf)> {
    let Ok(rd) = std::fs::read_dir(steam_root.join("userdata")) else {
        return Vec::new();
    };
    let mut out: Vec<(String, PathBuf)> = rd
        .flatten()
        .filter_map(|e| {
            let id = e.file_name().to_string_lossy().into_owned();
            if id == "0" || !id.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            let cfg = e.path().join("config/localconfig.vdf");
            cfg.is_file().then_some((id, cfg))
        })
        .collect();
    out.sort();
    out
}

pub fn check(steam_root: &Path) -> Vec<AccountLaunch> {
    accounts(steam_root)
        .into_iter()
        .map(|(account, config)| {
            let state = match std::fs::read_to_string(&config) {
                Ok(t) => state_of(&t),
                Err(e) => LaunchState::Unreadable(e.to_string()),
            };
            AccountLaunch {
                account,
                config,
                state,
            }
        })
        .collect()
}

/// Account ids shortened for display (`…1234`): output gets pasted into
/// public bug reports.
pub fn short_account(id: &str) -> String {
    if id.len() <= 4 {
        id.to_string()
    } else {
        format!("…{}", &id[id.len() - 4..])
    }
}

/// Sets the launch option in a `localconfig.vdf` text, changing only that
/// value. Other launch options the game has are kept ([`merged_value`]).
/// Refuses if they already name winmm with another mode, or the game has no
/// entry.
pub fn set_in_text(text: &str) -> Result<String, String> {
    let doc = vdf::parse(text).map_err(|e| tr!("err_vdf", e))?;
    let app = vdf::find_path(&doc, &APP_PATH).ok_or_else(|| tr!("err_launch_no_app"))?;
    let vdf::Value::Obj(children) = &app.value else {
        return Err(tr!("err_launch_no_app"));
    };
    let quoted = vdf::quote(LAUNCH_OPTION);
    if let Some(e) = vdf::find(children, "LaunchOptions") {
        let new = match &e.value {
            vdf::Value::Str(v) if value_is_set(v) => return Ok(text.to_string()),
            vdf::Value::Str(v) if v.trim().is_empty() => quoted,
            vdf::Value::Str(v) => match merged_value(v) {
                Some(m) => vdf::quote(&m),
                None => return Err(tr!("err_launch_other")),
            },
            _ => return Err(tr!("err_launch_other")),
        };
        return Ok(format!(
            "{}{new}{}",
            &text[..e.span.start],
            &text[e.span.end..]
        ));
    }
    // Insert a new line before the app block's closing brace, indented one
    // level deeper than the brace.
    let close = app.span.end - 1;
    let line_start = text[..close].rfind('\n').map_or(0, |i| i + 1);
    let indent: String = text[line_start..close]
        .chars()
        .take_while(|c| c.is_whitespace())
        .collect();
    let between = &text[line_start..close];
    if between.trim().is_empty() {
        let line = format!("{indent}\t\"LaunchOptions\"\t\t{quoted}\n");
        Ok(format!(
            "{}{line}{}",
            &text[..line_start],
            &text[line_start..]
        ))
    } else {
        // `}` shares its line with other content: insert inline.
        Ok(format!(
            "{} \"LaunchOptions\" {quoted} {}",
            &text[..close],
            &text[close..]
        ))
    }
}

/// Sets the launch option in one account's `localconfig.vdf`, after copying
/// the file to `localconfig.vdf.uk-qol-<unix time>.bak`. Returns the backup
/// path, or `None` if it was already set. The caller makes sure Steam isn't
/// running (Steam rewrites the file on exit).
pub fn set(config: &Path) -> Result<Option<PathBuf>, String> {
    let text = std::fs::read_to_string(config).map_err(|e| tr!("err_read", config.display(), e))?;
    let new = set_in_text(&text)?;
    if new == text {
        return Ok(None);
    }
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut name = config.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".uk-qol-{secs}.bak"));
    let backup = config.with_file_name(name);
    std::fs::copy(config, &backup).map_err(|e| tr!("err_write", backup.display(), e))?;
    crate::install::write_atomic(config, new.as_bytes())?;
    Ok(Some(backup))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Synthetic localconfig with decoy `2411200` keys at other levels.
    fn sample(app_body: &str) -> String {
        format!(
            "\"UserLocalConfigStore\"\n{{\n\t\"apptickets\"\n\t{{\n\t\t\"2411200\"\t\t\"00ff\"\n\t}}\n\
\t\"Software\"\n\t{{\n\t\t\"Valve\"\n\t\t{{\n\t\t\t\"Steam\"\n\t\t\t{{\n\t\t\t\t\"apps\"\n\t\t\t\t{{\n\
\t\t\t\t\t\"10\"\n\t\t\t\t\t{{\n\t\t\t\t\t\t\"LaunchOptions\"\t\t\"-novid\"\n\t\t\t\t\t}}\n\
{app_body}\
\t\t\t\t}}\n\t\t\t}}\n\t\t}}\n\t}}\n\t\"UserSettings\"\n\t{{\n\t\t\"2411200\"\t\t\"x\"\n\t}}\n}}\n"
        )
    }

    fn app(inner: &str) -> String {
        format!("\t\t\t\t\t\"2411200\"\n\t\t\t\t\t{{\n{inner}\t\t\t\t\t}}\n")
    }

    #[test]
    fn detects_states() {
        assert_eq!(state_of(&sample("")), LaunchState::NoApp);
        assert_eq!(
            state_of(&sample(&app("\t\t\t\t\t\t\"LastPlayed\"\t\t\"1\"\n"))),
            LaunchState::Empty
        );
        assert_eq!(
            state_of(&sample(&app("\t\t\t\t\t\t\"LaunchOptions\"\t\t\"\"\n"))),
            LaunchState::Empty
        );
        assert_eq!(
            state_of(&sample(&app(
                "\t\t\t\t\t\t\"LaunchOptions\"\t\t\"WINEDLLOVERRIDES=\\\"winmm=n,b\\\" %command%\"\n"
            ))),
            LaunchState::Set
        );
        assert_eq!(
            state_of(&sample(&app(
                "\t\t\t\t\t\t\"LaunchOptions\"\t\t\"PROTON_LOG=1 %command%\"\n"
            ))),
            LaunchState::Other("PROTON_LOG=1 %command%".into())
        );
        assert!(matches!(state_of("\"a\" {"), LaunchState::Unreadable(_)));
    }

    #[test]
    fn recognises_equivalent_values() {
        assert!(value_is_set(LAUNCH_OPTION));
        assert!(value_is_set("WINEDLLOVERRIDES=\"winmm=n\" %command%"));
        assert!(value_is_set(
            "PROTON_LOG=1 WINEDLLOVERRIDES='d3d9,winmm=n,b' %command% -x"
        ));
        assert!(value_is_set(
            "WINEDLLOVERRIDES=\"dxgi=n;winmm=n,b\" %command%"
        ));
        assert!(!value_is_set("WINEDLLOVERRIDES=\"winmm=b,n\" %command%"));
        assert!(!value_is_set("WINEDLLOVERRIDES=\"winmm=n,b\""));
        assert!(!value_is_set("%command%"));
    }

    #[test]
    fn sets_only_the_one_value() {
        // Missing LaunchOptions: inserted, everything else byte-identical.
        let before = sample(&app("\t\t\t\t\t\t\"LastPlayed\"\t\t\"1\"\n"));
        let after = set_in_text(&before).unwrap();
        assert_eq!(state_of(&after), LaunchState::Set);
        assert_eq!(
            after.replace("\t\t\t\t\t\t\"LaunchOptions\"\t\t\"WINEDLLOVERRIDES=\\\"winmm=n,b\\\" %command%\"\n", ""),
            before
        );
        // The decoys are untouched.
        let doc = vdf::parse(&after).unwrap();
        assert_eq!(
            vdf::str_at(&doc, &["UserLocalConfigStore", "UserSettings", "2411200"]),
            Some("x")
        );
        let other = [
            "UserLocalConfigStore",
            "Software",
            "Valve",
            "Steam",
            "apps",
            "10",
            "LaunchOptions",
        ];
        assert_eq!(vdf::str_at(&doc, &other), Some("-novid"));

        // Empty value: replaced in place.
        let before = sample(&app("\t\t\t\t\t\t\"LaunchOptions\"\t\t\"\"\n"));
        let after = set_in_text(&before).unwrap();
        assert_eq!(state_of(&after), LaunchState::Set);
        assert_eq!(
            after.len(),
            before.len() + vdf::quote(LAUNCH_OPTION).len() - 2
        );

        // Already set: unchanged. No entry: refused.
        assert_eq!(set_in_text(&after).unwrap(), after);
        assert!(set_in_text(&sample("")).is_err());

        // Other options: ours is merged in, only that value changes.
        let other = |v: &str| {
            sample(&app(&format!(
                "\t\t\t\t\t\t\"LaunchOptions\"\t\t{}\n",
                vdf::quote(v)
            )))
        };
        let before = other("WINEDLLOVERRIDES=\"version=n,b\" %command%");
        let after = set_in_text(&before).unwrap();
        assert_eq!(state_of(&after), LaunchState::Set);
        assert_eq!(
            after,
            other("WINEDLLOVERRIDES=\"version=n,b;winmm=n,b\" %command%")
        );
        assert_eq!(set_in_text(&after).unwrap(), after);
        // winmm already named with another mode: the player's choice, refused.
        assert!(set_in_text(&other("WINEDLLOVERRIDES=\"winmm=b\" %command%")).is_err());
    }

    #[test]
    fn merges_with_other_options() {
        let m = |v: &str| merged_value(v);
        // Another loader's override (a version.dll mod framework).
        assert_eq!(
            m("WINEDLLOVERRIDES=\"version=n,b\" %command%").as_deref(),
            Some("WINEDLLOVERRIDES=\"version=n,b;winmm=n,b\" %command%")
        );
        assert_eq!(
            m("PROTON_LOG=1 WINEDLLOVERRIDES='version=n,b' %command% -x").as_deref(),
            Some("PROTON_LOG=1 WINEDLLOVERRIDES='version=n,b;winmm=n,b' %command% -x")
        );
        assert_eq!(
            m("WINEDLLOVERRIDES=version=n,b %command%").as_deref(),
            Some("WINEDLLOVERRIDES=version=n,b;winmm=n,b %command%")
        );
        assert_eq!(
            m("WINEDLLOVERRIDES=\"dxgi=n;\" %command%").as_deref(),
            Some("WINEDLLOVERRIDES=\"dxgi=n;winmm=n,b\" %command%")
        );
        assert_eq!(
            m("WINEDLLOVERRIDES=\"\" %command%").as_deref(),
            Some("WINEDLLOVERRIDES=\"winmm=n,b\" %command%")
        );
        // An override without %command% gets one.
        assert_eq!(
            m("WINEDLLOVERRIDES=\"version=n,b\"").as_deref(),
            Some("WINEDLLOVERRIDES=\"version=n,b;winmm=n,b\" %command%")
        );
        // No override yet.
        assert_eq!(
            m("PROTON_LOG=1 %command%").as_deref(),
            Some("WINEDLLOVERRIDES=\"winmm=n,b\" PROTON_LOG=1 %command%")
        );
        assert_eq!(
            m("gamemoderun %command% -novid").as_deref(),
            Some("WINEDLLOVERRIDES=\"winmm=n,b\" gamemoderun %command% -novid")
        );
        assert_eq!(
            m("-novid").as_deref(),
            Some("WINEDLLOVERRIDES=\"winmm=n,b\" %command% -novid")
        );
        // A variable whose name only ends in WINEDLLOVERRIDES isn't it.
        assert_eq!(
            m("MY_WINEDLLOVERRIDES=x %command%").as_deref(),
            Some("WINEDLLOVERRIDES=\"winmm=n,b\" MY_WINEDLLOVERRIDES=x %command%")
        );
        // Every result loads our DLL.
        for v in [
            "WINEDLLOVERRIDES=\"version=n,b\" %command%",
            "PROTON_LOG=1 %command%",
            "-novid",
            "WINEDLLOVERRIDES=\"version=n,b\"",
        ] {
            assert!(value_is_set(&m(v).unwrap()), "{v}");
        }
        // Already set: unchanged.
        assert_eq!(m(LAUNCH_OPTION).as_deref(), Some(LAUNCH_OPTION));
        // winmm named with another mode, or in a list: left to the player.
        assert_eq!(m("WINEDLLOVERRIDES=\"winmm=b\" %command%"), None);
        assert_eq!(m("WINEDLLOVERRIDES=\"d3d9,WinMM=\" %command%"), None);
        assert!(can_set(&LaunchState::Empty));
        assert!(can_set(&LaunchState::Other("-x".into())));
        assert!(!can_set(&LaunchState::Other(
            "WINEDLLOVERRIDES=winmm=b %command%".into()
        )));
        assert!(!can_set(&LaunchState::Set));
    }

    #[test]
    fn set_writes_a_backup_in_a_temp_steam() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_dir = tmp.path().join("userdata/12345/config");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        let cfg = cfg_dir.join("localconfig.vdf");
        let before = sample(&app(""));
        std::fs::write(&cfg, &before).unwrap();
        std::fs::create_dir_all(tmp.path().join("userdata/anonymous")).unwrap();

        let found = check(tmp.path());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].state, LaunchState::Empty);

        let backup = set(&cfg).unwrap().unwrap();
        assert_eq!(std::fs::read_to_string(backup).unwrap(), before);
        assert_eq!(check(tmp.path())[0].state, LaunchState::Set);
        assert_eq!(set(&cfg).unwrap(), None);
        assert_eq!(short_account("12345"), "…2345");
    }
}
