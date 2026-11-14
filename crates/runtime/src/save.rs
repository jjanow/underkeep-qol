//! Save sidecars (PLAN §2): `<save name>.qol.toml`, one table per mod, keyed
//! by hero.
//!
//! ```toml
//! format = 1
//!
//! [mods.weapon-swap.global]
//! seen_warning = true
//!
//! [mods.weapon-swap.heroes."hero key"]
//! ranged = { item = "bow", durability = 12 }
//! ```
//!
//! - `format` is [`FORMAT_VERSION`]. A missing `format` reads as 1; a higher
//!   one is an error, so an older runtime never overwrites a newer sidecar.
//! - Unknown top-level keys are kept ([`Sidecar::extra`]). Keys of a mod table
//!   other than `global` and `heroes` are not (a mod's state is
//!   [`ModSaveState`]).
//!
//! Where sidecars live: the game keeps its saves in Steam Cloud storage
//! (`userdata/<account>/2411200/remote/`), which the game or Steam writes;
//! whether the game writes there itself or through `ISteamRemoteStorage` is
//! not known yet. Writing our own files into Steam's folder could confuse the
//! cloud sync, so sidecars are stored locally in the game folder instead:
//! `<game folder>/uk_qol/saves/<account>/<save name>.qol.toml` (see
//! [`sidecar_path`]). Mod state is therefore per computer; the saves still sync.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use toml::{Table, Value};

use crate::config::write_atomic;
use crate::host::ModSaveState;

/// The sidecar format this runtime writes and the newest it reads.
pub const FORMAT_VERSION: i64 = 1;

/// File name suffix: `<save name>.qol.toml`.
pub const SUFFIX: &str = ".qol.toml";

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Sidecar {
    pub mods: BTreeMap<String, ModSaveState>,
    /// Top-level keys other than `format` and `mods`, kept as read.
    pub extra: toml::Table,
}

impl Sidecar {
    pub fn parse(text: &str) -> Result<Sidecar, String> {
        let mut root: Table = text.parse().map_err(|e| format!("sidecar: {e}"))?;
        match root.remove("format") {
            None => {}
            Some(Value::Integer(v)) if (1..=FORMAT_VERSION).contains(&v) => {}
            Some(Value::Integer(v)) => {
                return Err(format!(
                    "sidecar: format {v} is not supported (this runtime reads up to {FORMAT_VERSION})"
                ));
            }
            Some(_) => return Err("sidecar: `format` must be an integer".into()),
        }
        let mut sc = Sidecar::default();
        if let Some(mods) = root.remove("mods") {
            let Value::Table(mods) = mods else {
                return Err("sidecar: `mods` must be a table".into());
            };
            for (id, m) in mods {
                let Value::Table(mut m) = m else {
                    return Err(format!("sidecar: `mods.{id}` must be a table"));
                };
                let mut state = ModSaveState::default();
                match m.remove("global") {
                    None => {}
                    Some(Value::Table(g)) => state.global = g,
                    Some(_) => return Err(format!("sidecar: `mods.{id}.global` must be a table")),
                }
                match m.remove("heroes") {
                    None => {}
                    Some(Value::Table(h)) => {
                        for (hero, t) in h {
                            let Value::Table(t) = t else {
                                return Err(format!(
                                    "sidecar: `mods.{id}.heroes.{hero}` must be a table"
                                ));
                            };
                            state.heroes.insert(hero, t);
                        }
                    }
                    Some(_) => return Err(format!("sidecar: `mods.{id}.heroes` must be a table")),
                }
                sc.mods.insert(id, state);
            }
        }
        sc.extra = root;
        Ok(sc)
    }

    pub fn to_toml(&self) -> String {
        let mut root = self.extra.clone();
        root.insert("format".into(), Value::Integer(FORMAT_VERSION));
        let mut mods = Table::new();
        for (id, state) in &self.mods {
            // Empty `global` / `heroes` are left out; they read back as empty.
            let mut m = Table::new();
            if !state.global.is_empty() {
                m.insert("global".into(), Value::Table(state.global.clone()));
            }
            if !state.heroes.is_empty() {
                let heroes: Table = state
                    .heroes
                    .iter()
                    .map(|(k, v)| (k.clone(), Value::Table(v.clone())))
                    .collect();
                m.insert("heroes".into(), Value::Table(heroes));
            }
            mods.insert(id.clone(), Value::Table(m));
        }
        if !mods.is_empty() {
            root.insert("mods".into(), Value::Table(mods));
        }
        // A `Table` of TOML values always serializes.
        toml::to_string(&root).unwrap_or_default()
    }

    /// Reads `path`; a missing file is an empty sidecar (a save made without
    /// the mods, or on another computer).
    pub fn load(path: &Path) -> Result<Sidecar, String> {
        match fs::read_to_string(path) {
            Ok(text) => Sidecar::parse(&text),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Sidecar::default()),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    /// Writes the sidecar atomically, creating its folder.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        write_atomic(path, self.to_toml().as_bytes())
    }
}

/// Where the sidecar of `save_name` (the game's save file name, e.g.
/// `save_1`) lives: `<data_dir>/saves/<account>/<save name>.qol.toml`, with
/// `account` the Steam account id (`userdata/<account>`) or `default` if it
/// isn't known. Both names are sanitized into one safe path component.
pub fn sidecar_path(data_dir: &Path, account: Option<&str>, save_name: &str) -> PathBuf {
    let account = account.map_or_else(|| "default".to_owned(), sanitize);
    data_dir
        .join("saves")
        .join(account)
        .join(format!("{}{SUFFIX}", sanitize(save_name)))
}

/// Turns any string into one file name component that is valid on Windows
/// and Linux: characters other than ASCII letters, digits, `-`, `_` and `.`
/// become `_`; leading and trailing dots are replaced (no `..`, no hidden
/// files, no trailing dot Windows would strip); Windows device names (`CON`,
/// `nul.txt`, `COM1`, …) get a `_` prefix; empty becomes `_`; at most 100
/// characters.
pub fn sanitize(name: &str) -> String {
    let mut s: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .take(100)
        .collect();
    // ASCII only from here, so byte indexes are char indexes.
    let lead = s.len() - s.trim_start_matches('.').len();
    let trail = s.len() - s.trim_end_matches('.').len();
    if lead == s.len() {
        s = "_".repeat(s.len());
    } else {
        s.replace_range(..lead, &"_".repeat(lead));
        let n = s.len();
        s.replace_range(n - trail.., &"_".repeat(trail));
    }
    if s.is_empty() {
        s.push('_');
    }
    let stem = s.split('.').next().unwrap_or("").to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.as_bytes()[3].is_ascii_digit());
    if reserved {
        s.insert(0, '_');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::tests::temp_dir;

    fn table(text: &str) -> Table {
        text.parse().unwrap()
    }

    fn sample() -> Sidecar {
        let mut ws = ModSaveState {
            global: table("seen_warning = true"),
            ..Default::default()
        };
        ws.heroes.insert(
            "hero 1".into(),
            table(r#"ranged = { item = "bow", durability = 12 }"#),
        );
        ws.heroes.insert("a.b".into(), table("x = 1"));
        ws.heroes.insert("Žofie".into(), table("mode = \"health\""));
        ws.heroes.insert("empty".into(), Table::new());
        let mut sc = Sidecar::default();
        sc.mods.insert("weapon-swap".into(), ws);
        sc.mods.insert("nothing".into(), ModSaveState::default());
        sc.extra.insert("note".into(), Value::from("kept"));
        sc
    }

    #[test]
    fn round_trip() {
        let sc = sample();
        let text = sc.to_toml();
        assert!(text.starts_with("format = 1\n"), "{text}");
        assert!(
            text.contains("[mods.weapon-swap.heroes.\"hero 1\".ranged]"),
            "{text}"
        );
        assert_eq!(Sidecar::parse(&text).unwrap(), sc, "{text}");
        assert_eq!(Sidecar::parse("").unwrap(), Sidecar::default());
        let empty = Sidecar::default().to_toml();
        assert_eq!(Sidecar::parse(&empty).unwrap(), Sidecar::default());
    }

    #[test]
    fn hand_written() {
        let sc = Sidecar::parse(
            r#"
format = 1
[mods.bloodmage-cost.heroes.h1]
pay = "health"
[mods.bloodmage-cost.global]
"#,
        )
        .unwrap();
        let st = &sc.mods["bloodmage-cost"];
        assert_eq!(st.heroes["h1"]["pay"].as_str(), Some("health"));
        assert!(st.global.is_empty());
    }

    #[test]
    fn errors() {
        assert!(Sidecar::parse("format = 2").is_err());
        assert!(Sidecar::parse("format = \"1\"").is_err());
        assert!(Sidecar::parse("format = 0").is_err());
        assert!(Sidecar::parse("mods = 1").is_err());
        assert!(Sidecar::parse("[mods]\nx = 1").is_err());
        assert!(Sidecar::parse("[mods.x]\nglobal = 1").is_err());
        assert!(Sidecar::parse("[mods.x.heroes]\nh = 1").is_err());
        assert!(Sidecar::parse("[mods").is_err());
    }

    #[test]
    fn save_and_load() {
        let dir = temp_dir("sidecar");
        let p = sidecar_path(&dir, Some("12345"), "save_1");
        assert_eq!(p, dir.join("saves").join("12345").join("save_1.qol.toml"));
        assert_eq!(Sidecar::load(&p).unwrap(), Sidecar::default());
        let sc = sample();
        sc.save(&p).unwrap();
        assert_eq!(Sidecar::load(&p).unwrap(), sc);
        assert_eq!(
            sidecar_path(&dir, None, "save_auto"),
            dir.join("saves").join("default").join("save_auto.qol.toml")
        );
    }

    #[test]
    fn sanitizing() {
        assert_eq!(sanitize("save_1"), "save_1");
        assert_eq!(sanitize("save auto"), "save_auto");
        assert_eq!(sanitize("../../x"), "___.._x");
        assert_eq!(sanitize(".."), "__");
        assert_eq!(sanitize(".hidden"), "_hidden");
        assert_eq!(sanitize("a/b\\c:d"), "a_b_c_d");
        assert_eq!(sanitize("trail. "), "trail._");
        assert_eq!(sanitize("trail.."), "trail__");
        assert_eq!(sanitize(""), "_");
        assert_eq!(sanitize("CON"), "_CON");
        assert_eq!(sanitize("nul.txt"), "_nul.txt");
        assert_eq!(sanitize("com1"), "_com1");
        assert_eq!(sanitize("COM10"), "COM10");
        assert_eq!(sanitize("console"), "console");
        assert_eq!(sanitize("Žofie"), "_ofie");
        assert_eq!(sanitize(&"x".repeat(300)).len(), 100);
    }
}
