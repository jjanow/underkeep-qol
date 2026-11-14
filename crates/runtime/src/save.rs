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
//! Where sidecars live ([`Store`]): with Steam, in the game's Steam Cloud
//! storage next to its saves (`userdata/<account>/2411200/remote/`), written
//! through `ISteamRemoteStorage` like the saves, as [`cloud_name`]
//! (`qol_<save name>.toml`), so mod state follows a save to other computers
//! and the Steam Deck. Without Steam: locally in the game folder,
//! `<game folder>/uk_qol/saves/<account>/<save name>.qol.toml`
//! ([`sidecar_path`]). Local sidecars are moved into the cloud by
//! [`Store::migrate`], keeping the newer copy.
//!
//! The game's save list never shows the cloud sidecars (game 1.0.5): it lists
//! only cloud files matching the glob `save_*` and then drops names with an
//! extension.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

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
    account_dir(data_dir, account).join(format!("{}{SUFFIX}", sanitize(save_name)))
}

/// The local sidecar folder of `account`: `<data_dir>/saves/<account>`
/// (`default` if it isn't known).
pub fn account_dir(data_dir: &Path, account: Option<&str>) -> PathBuf {
    let account = account.map_or_else(|| "default".to_owned(), sanitize);
    data_dir.join("saves").join(account)
}

/// Cloud file name of the sidecar of `save_name`: `qol_<sanitized save
/// name>.toml`. The game's save list ignores it twice over (it doesn't match
/// `save_*`, and it has an extension).
pub fn cloud_name(save_name: &str) -> String {
    format!("qol_{}.toml", sanitize(save_name))
}

/// Which of two copies of a sidecar is current, by modification time (Unix
/// seconds); a tie goes to the cloud copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Newer {
    None,
    Local,
    Cloud,
}

pub fn newer(local: Option<i64>, cloud: Option<i64>) -> Newer {
    match (local, cloud) {
        (None, None) => Newer::None,
        (Some(_), None) => Newer::Local,
        (None, Some(_)) => Newer::Cloud,
        (Some(l), Some(c)) if l > c => Newer::Local,
        _ => Newer::Cloud,
    }
}

/// What [`Store::migrate`] does with a local sidecar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Migration {
    /// Copy it into the cloud (no cloud copy, or the local one is newer), then
    /// move it to the backup folder.
    Upload,
    /// The cloud copy is as new or newer: only move it to the backup folder.
    KeepCloud,
    /// Its save isn't in this account's cloud (deleted, or another
    /// account's): leave it where it is.
    Skip,
}

/// The [`Migration`] of a local sidecar modified at `local` whose cloud copy
/// was modified at `cloud` (`None`: no cloud copy). `save_in_cloud`: the
/// game's save of that name exists in the cloud.
pub fn migration(local: i64, cloud: Option<i64>, save_in_cloud: bool) -> Migration {
    if !save_in_cloud {
        return Migration::Skip;
    }
    match newer(Some(local), cloud) {
        Newer::Local => Migration::Upload,
        _ => Migration::KeepCloud,
    }
}

/// Folder (inside the account's local sidecar folder) that migrated local
/// sidecars are moved to, as a backup.
pub const BACKUP_DIR: &str = "migrated";

/// A free path for the backup of `file_name` in `dir`: `dir/<file_name>`, or
/// `dir/<file_name>.<n>` (n = 1, 2, …) if that's taken (`taken`), so an older
/// backup is never replaced.
pub fn backup_path(dir: &Path, file_name: &str, taken: impl Fn(&Path) -> bool) -> PathBuf {
    let first = dir.join(file_name);
    if !taken(&first) {
        return first;
    }
    (1u32..)
        .map(|n| dir.join(format!("{file_name}.{n}")))
        .find(|p| !taken(p))
        .unwrap_or(first)
}

/// Steam Cloud files (`ISteamRemoteStorage`; `crate::steam::Cloud`).
pub trait CloudFiles {
    /// Modification time (Unix seconds) of `name`, `None` if it doesn't exist.
    fn timestamp(&self, name: &str) -> Option<i64>;
    /// Contents of `name`, `None` if it doesn't exist.
    fn read(&self, name: &str) -> Result<Option<Vec<u8>>, String>;
    fn write(&self, name: &str, bytes: &[u8]) -> Result<(), String>;
    /// Deletes `name`; `false` if it didn't exist.
    fn delete(&self, name: &str) -> Result<bool, String>;
    /// How `name` is shown in the log.
    fn describe(&self, name: &str) -> String;
}

/// Modification time of a local file in Unix seconds, `None` if missing.
pub fn mtime(path: &Path) -> Option<i64> {
    let t = fs::metadata(path).ok()?.modified().ok()?;
    Some(match t.duration_since(UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_secs()).unwrap_or(i64::MAX),
        Err(e) => -i64::try_from(e.duration().as_secs()).unwrap_or(i64::MAX),
    })
}

/// Where the sidecars of one account go: the cloud if there is one, else the
/// local folder `dir` (`<data_dir>/saves/<account>`). Leftover local files
/// (written while Steam was unavailable, or when a cloud write failed) are
/// still read when they are newer than the cloud copy.
pub struct Store<'a> {
    pub dir: PathBuf,
    pub cloud: Option<&'a dyn CloudFiles>,
}

impl Store<'_> {
    /// The local path of the sidecar of `save_name`.
    pub fn local(&self, save_name: &str) -> PathBuf {
        self.dir.join(format!("{}{SUFFIX}", sanitize(save_name)))
    }

    /// The raw sidecar of `save_name` (the newer of the cloud and the local
    /// copy) and where it was read from; `None` if there is none.
    pub fn read(&self, save_name: &str) -> Result<Option<(Vec<u8>, String)>, String> {
        let path = self.local(save_name);
        let local = mtime(&path);
        let cloud_ts = self.cloud.and_then(|c| c.timestamp(&cloud_name(save_name)));
        match newer(local, cloud_ts) {
            Newer::None => Ok(None),
            Newer::Local => match fs::read(&path) {
                Ok(b) => Ok(Some((b, path.display().to_string()))),
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(format!("{}: {e}", path.display())),
            },
            Newer::Cloud => {
                // `cloud_ts` is only `Some` with a cloud.
                let Some(c) = self.cloud else { return Ok(None) };
                let name = cloud_name(save_name);
                Ok(c.read(&name)?.map(|b| (b, c.describe(&name))))
            }
        }
    }

    /// Where the sidecar of `save_name` is read from and written to, for the
    /// log.
    pub fn describe(&self, save_name: &str) -> String {
        match self.cloud {
            Some(c) => c.describe(&cloud_name(save_name)),
            None => self.local(save_name).display().to_string(),
        }
    }

    /// Writes the sidecar of `save_name`: to the cloud, or locally without
    /// one or if the cloud write fails (the next [`Store::migrate`] retries
    /// the upload). After a cloud write, a leftover local copy is moved to
    /// the backup folder. Returns where it went, plus notes for the log.
    pub fn write(&self, save_name: &str, bytes: &[u8]) -> Result<(String, Vec<String>), String> {
        let mut notes = Vec::new();
        let path = self.local(save_name);
        if let Some(c) = self.cloud {
            let name = cloud_name(save_name);
            match c.write(&name, bytes) {
                Ok(()) => {
                    if path.exists() {
                        notes.push(self.back_up(&path));
                    }
                    return Ok((c.describe(&name), notes));
                }
                Err(e) => notes.push(format!("{e}; writing it locally instead")),
            }
        }
        write_atomic(&path, bytes).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok((path.display().to_string(), notes))
    }

    /// Deletes every copy of the sidecar of `save_name` (cloud and local;
    /// backups stay). Returns what was removed and what failed, for the log.
    pub fn delete(&self, save_name: &str) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(c) = self.cloud {
            let name = cloud_name(save_name);
            match c.delete(&name) {
                Ok(true) => out.push(format!("removed {}", c.describe(&name))),
                Ok(false) => {}
                Err(e) => out.push(e),
            }
        }
        let path = self.local(save_name);
        match fs::remove_file(&path) {
            Ok(()) => out.push(format!("removed {}", path.display())),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => out.push(format!("removing {} failed: {e}", path.display())),
        }
        out
    }

    /// Moves `path` into the backup folder; returns a log line.
    fn back_up(&self, path: &Path) -> String {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let dir = self.dir.join(BACKUP_DIR);
        let to = backup_path(&dir, &name, |p| p.exists());
        match fs::create_dir_all(&dir).and_then(|()| fs::rename(path, &to)) {
            Ok(()) => format!("moved {} to {}", path.display(), to.display()),
            Err(e) => format!("moving {} to {} failed: {e}", path.display(), to.display()),
        }
    }

    /// Moves the local sidecars in `dir` into the cloud (nothing without a
    /// cloud): each one whose save is in the cloud ([`Migration`]) is
    /// uploaded byte for byte if it is newer than the cloud copy, and then
    /// moved to `dir/migrated/` as a backup. Others are left alone. Safe to
    /// run every session: handled files leave `dir`. Returns log lines.
    pub fn migrate(&self) -> Vec<String> {
        let mut out = Vec::new();
        let Some(c) = self.cloud else { return out };
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return out;
        };
        let mut files: Vec<PathBuf> = entries
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
            .map(|e| e.path())
            .collect();
        files.sort();
        for path in files {
            let Some(save) = path
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.strip_suffix(SUFFIX))
                .filter(|s| !s.is_empty())
            else {
                continue;
            };
            let Some(local) = mtime(&path) else { continue };
            let name = cloud_name(save);
            match migration(local, c.timestamp(&name), c.timestamp(save).is_some()) {
                Migration::Skip => out.push(format!(
                    "migrate: {} left local: no save {save} in the cloud",
                    path.display()
                )),
                Migration::KeepCloud => out.push(format!(
                    "migrate: {} is as new or newer; {}",
                    c.describe(&name),
                    self.back_up(&path)
                )),
                Migration::Upload => {
                    let up = fs::read(&path)
                        .map_err(|e| format!("{}: {e}", path.display()))
                        .and_then(|b| c.write(&name, &b));
                    match up {
                        Ok(()) => out.push(format!(
                            "migrate: copied {} to {}; {}",
                            path.display(),
                            c.describe(&name),
                            self.back_up(&path)
                        )),
                        Err(e) => out.push(format!(
                            "migrate: {e}; {} left local, retried next session",
                            path.display()
                        )),
                    }
                }
            }
        }
        out
    }
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

    /// In-memory cloud: name → (timestamp, bytes); writes stamp `now`.
    #[derive(Default)]
    struct FakeCloud {
        files: std::cell::RefCell<BTreeMap<String, (i64, Vec<u8>)>>,
        now: std::cell::Cell<i64>,
        full: std::cell::Cell<bool>,
    }

    impl FakeCloud {
        fn put(&self, name: &str, ts: i64, bytes: &[u8]) {
            self.files
                .borrow_mut()
                .insert(name.into(), (ts, bytes.to_vec()));
        }
        fn get(&self, name: &str) -> Option<Vec<u8>> {
            self.files.borrow().get(name).map(|f| f.1.clone())
        }
    }

    impl CloudFiles for FakeCloud {
        fn timestamp(&self, name: &str) -> Option<i64> {
            self.files.borrow().get(name).map(|f| f.0)
        }
        fn read(&self, name: &str) -> Result<Option<Vec<u8>>, String> {
            Ok(self.get(name))
        }
        fn write(&self, name: &str, bytes: &[u8]) -> Result<(), String> {
            if self.full.get() {
                return Err("full".into());
            }
            self.put(name, self.now.get(), bytes);
            Ok(())
        }
        fn delete(&self, name: &str) -> Result<bool, String> {
            Ok(self.files.borrow_mut().remove(name).is_some())
        }
        fn describe(&self, name: &str) -> String {
            format!("cloud:{name}")
        }
    }

    /// Writes `dir/name` and sets its mtime to `ts` (Unix seconds).
    fn local_file(dir: &Path, name: &str, text: &str, ts: i64) -> PathBuf {
        let p = dir.join(name);
        fs::create_dir_all(dir).unwrap();
        fs::write(&p, text).unwrap();
        let t = UNIX_EPOCH + std::time::Duration::from_secs(ts as u64);
        fs::File::options()
            .write(true)
            .open(&p)
            .unwrap()
            .set_modified(t)
            .unwrap();
        assert_eq!(mtime(&p), Some(ts));
        p
    }

    #[test]
    fn naming() {
        assert_eq!(cloud_name("save_1"), "qol_save_1.toml");
        assert_eq!(cloud_name("save_auto"), "qol_save_auto.toml");
        assert_eq!(cloud_name("../x"), "qol____x.toml");
        // The game's save list keeps `save_*` names without an extension;
        // a cloud sidecar is neither.
        for save in ["save_1", "save_auto", "save_slot_quick", ""] {
            let n = cloud_name(save);
            assert!(!n.starts_with("save_") && n.contains('.'), "{n}");
        }
    }

    #[test]
    fn decisions() {
        assert_eq!(newer(None, None), Newer::None);
        assert_eq!(newer(Some(5), None), Newer::Local);
        assert_eq!(newer(None, Some(5)), Newer::Cloud);
        assert_eq!(newer(Some(6), Some(5)), Newer::Local);
        assert_eq!(newer(Some(5), Some(5)), Newer::Cloud);
        assert_eq!(newer(Some(4), Some(5)), Newer::Cloud);
        assert_eq!(migration(10, None, true), Migration::Upload);
        assert_eq!(migration(10, Some(9), true), Migration::Upload);
        assert_eq!(migration(10, Some(10), true), Migration::KeepCloud);
        assert_eq!(migration(10, Some(11), true), Migration::KeepCloud);
        assert_eq!(migration(10, None, false), Migration::Skip);
        assert_eq!(migration(10, Some(1), false), Migration::Skip);
    }

    #[test]
    fn backups_never_replace() {
        let d = Path::new("/b");
        let taken =
            |set: &'static [&'static str]| move |p: &Path| set.iter().any(|s| p == d.join(s));
        assert_eq!(
            backup_path(d, "s.qol.toml", taken(&[])),
            d.join("s.qol.toml")
        );
        assert_eq!(
            backup_path(d, "s.qol.toml", taken(&["s.qol.toml", "s.qol.toml.1"])),
            d.join("s.qol.toml.2")
        );
    }

    #[test]
    fn migrate() {
        let dir = temp_dir("migrate");
        let cloud = FakeCloud::default();
        cloud.now.set(1000);
        // Saves in the cloud: a, b, c, e (not d).
        for s in ["save_a", "save_b", "save_c", "save_e"] {
            cloud.put(s, 1, b"save");
        }
        // a: no cloud sidecar → uploaded. b: local newer → uploaded.
        // c: cloud newer → kept. d: no save → left. e: unreadable, newer →
        // uploaded byte for byte.
        local_file(&dir, "save_a.qol.toml", "format = 1 # a", 100);
        local_file(&dir, "save_b.qol.toml", "format = 1 # b local", 200);
        cloud.put("qol_save_b.toml", 150, b"format = 1 # b cloud");
        local_file(&dir, "save_c.qol.toml", "format = 1 # c local", 200);
        cloud.put("qol_save_c.toml", 300, b"format = 1 # c cloud");
        local_file(&dir, "save_d.qol.toml", "format = 1 # d", 100);
        local_file(&dir, "save_e.qol.toml", "format = 99", 100);
        local_file(&dir, "notes.txt", "x", 100);
        // An older backup of a is never replaced.
        local_file(&dir.join(BACKUP_DIR), "save_a.qol.toml", "old backup", 50);

        let store = Store {
            dir: dir.clone(),
            cloud: Some(&cloud),
        };
        let log = store.migrate();
        assert_eq!(log.len(), 5, "{log:#?}");
        assert_eq!(cloud.get("qol_save_a.toml").unwrap(), b"format = 1 # a");
        assert_eq!(
            cloud.get("qol_save_b.toml").unwrap(),
            b"format = 1 # b local"
        );
        assert_eq!(
            cloud.get("qol_save_c.toml").unwrap(),
            b"format = 1 # c cloud"
        );
        assert_eq!(cloud.get("qol_save_d.toml"), None);
        assert_eq!(cloud.get("qol_save_e.toml").unwrap(), b"format = 99");
        // Handled files moved to the backups, the rest left.
        let b = dir.join(BACKUP_DIR);
        assert_eq!(
            fs::read_to_string(b.join("save_a.qol.toml")).unwrap(),
            "old backup"
        );
        assert_eq!(
            fs::read_to_string(b.join("save_a.qol.toml.1")).unwrap(),
            "format = 1 # a"
        );
        assert_eq!(
            fs::read_to_string(b.join("save_c.qol.toml")).unwrap(),
            "format = 1 # c local"
        );
        for s in ["a", "b", "c", "e"] {
            assert!(!dir.join(format!("save_{s}.qol.toml")).exists());
        }
        assert!(dir.join("save_d.qol.toml").exists());
        assert!(dir.join("notes.txt").exists());
        // Again: only d is left, and it stays.
        assert_eq!(store.migrate().len(), 1);

        // A full cloud: the upload fails, the local file stays for next time.
        local_file(&dir, "save_a.qol.toml", "format = 1 # a2", 2000);
        cloud.full.set(true);
        let log = store.migrate();
        assert!(log.iter().any(|l| l.contains("full")), "{log:#?}");
        assert!(dir.join("save_a.qol.toml").exists());

        // Without a cloud nothing happens.
        let local = Store {
            dir: dir.clone(),
            cloud: None,
        };
        assert!(local.migrate().is_empty());
    }

    #[test]
    fn store_cloud() {
        let dir = temp_dir("store-cloud");
        let cloud = FakeCloud::default();
        cloud.now.set(i64::MAX / 2);
        let store = Store {
            dir: dir.clone(),
            cloud: Some(&cloud),
        };
        assert_eq!(store.read("save_1").unwrap(), None);
        let (to, notes) = store.write("save_1", b"format = 1").unwrap();
        assert_eq!((to.as_str(), notes.len()), ("cloud:qol_save_1.toml", 0));
        assert_eq!(
            store.read("save_1").unwrap(),
            Some((b"format = 1".to_vec(), "cloud:qol_save_1.toml".into()))
        );
        // A newer local leftover is read; the next cloud write backs it up.
        cloud.put("qol_save_1.toml", 10, b"cloud");
        local_file(&dir, "save_1.qol.toml", "local", 20);
        assert_eq!(store.read("save_1").unwrap().unwrap().0, b"local");
        let (_, notes) = store.write("save_1", b"new").unwrap();
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(!dir.join("save_1.qol.toml").exists());
        assert!(dir.join(BACKUP_DIR).join("save_1.qol.toml").exists());
        assert_eq!(store.read("save_1").unwrap().unwrap().0, b"new");
        // A failing cloud write falls back to a local file.
        cloud.full.set(true);
        let (to, notes) = store.write("save_2", b"two").unwrap();
        assert_eq!(PathBuf::from(&to), dir.join("save_2.qol.toml"));
        assert_eq!(notes.len(), 1);
        assert_eq!(store.read("save_2").unwrap().unwrap().0, b"two");
        // Delete removes the cloud file and a local copy, not the backups.
        cloud.put("qol_save_2.toml", 1, b"old");
        let log = store.delete("save_2");
        assert_eq!(log.len(), 2, "{log:?}");
        assert_eq!(cloud.get("qol_save_2.toml"), None);
        assert!(!dir.join("save_2.qol.toml").exists());
        assert_eq!(store.read("save_2").unwrap(), None);
        assert!(store.delete("save_2").is_empty());
        assert_eq!(store.delete("save_1").len(), 1);
        assert!(dir.join(BACKUP_DIR).join("save_1.qol.toml").exists());
    }

    #[test]
    fn store_local() {
        let dir = temp_dir("store-local");
        let store = Store {
            dir: dir.clone(),
            cloud: None,
        };
        assert_eq!(
            store.describe("save_1"),
            dir.join("save_1.qol.toml").display().to_string()
        );
        assert_eq!(store.read("save_1").unwrap(), None);
        store.write("save_1", b"x").unwrap();
        assert_eq!(fs::read(dir.join("save_1.qol.toml")).unwrap(), b"x");
        assert_eq!(store.read("save_1").unwrap().unwrap().0, b"x");
        assert_eq!(store.delete("save_1").len(), 1);
        assert_eq!(store.read("save_1").unwrap(), None);
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
