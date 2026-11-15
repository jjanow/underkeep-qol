//! Updates from this repository's GitHub releases: the manager itself, the
//! installed loader (`winmm.dll`, with the built-in mods) and mod pattern
//! files. Adapted from Underkeeper's `update.rs`.
//!
//! The latest (non-pre-release) release is the source of truth. Every
//! download is checked against the release's `SHA256SUMS` asset (and against
//! GitHub's own `digest` for the asset when it has one); a file without a
//! checksum is never used. Nothing is changed without the user's go-ahead:
//! [`check`] only reads.
//!
//! Release assets (see `.github/workflows/build.yml`):
//! - `uk-qol-windows-x64.exe`, `uk-qol-linux-x64.tar.gz`,
//!   `uk-qol-macos-arm64.tar.gz`: the manager.
//! - `uk-qol-runtime-windows-x64.zip`: `winmm.dll`.
//! - `uk-qol-patterns.zip` (optional): `<mod id>.toml` pattern files, and
//!   `core.toml` for the runtime's own hooks.
//! - `SHA256SUMS`: `sha256sum` output for all of the above.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::tr;

pub const REPO: &str = "jjanow/underkeep-qol";
pub const RELEASES_URL: &str = "https://github.com/jjanow/underkeep-qol/releases";
pub const SUMS_ASSET: &str = "SHA256SUMS";
pub const RUNTIME_ASSET: &str = "uk-qol-runtime-windows-x64.zip";
pub const PATTERNS_ASSET: &str = "uk-qol-patterns.zip";

/// Prefix of the staging folder created next to the program while updating.
const STAGING_PREFIX: &str = ".uk-qol-update-";

/// The running version. `UK_QOL_FAKE_VERSION` overrides it for testing the
/// update path.
pub fn current_version() -> String {
    std::env::var("UK_QOL_FAKE_VERSION")
        .ok()
        .filter(|v| parse_version(v).is_some())
        .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string())
}

/// `v1.2.3`, `1.2.3` or `1.2.3-rc1` → (1, 2, 3). Missing minor/patch count as 0.
pub fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let s = s.trim().trim_start_matches(['v', 'V']);
    let core = s.split(['-', '+']).next()?;
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().map_or(Some(0), |p| p.parse().ok())?;
    let patch = parts.next().map_or(Some(0), |p| p.parse().ok())?;
    Some((major, minor, patch))
}

/// `latest` is newer than `current`; an unparsable `current` (e.g.
/// "unknown") counts as older.
pub fn is_newer(latest: &str, current: &str) -> bool {
    match (parse_version(latest), parse_version(current)) {
        (Some(l), Some(c)) => l > c,
        (Some(_), None) => true,
        _ => false,
    }
}

/// The manager's release asset for the platform this binary was built for.
pub fn manager_asset() -> Option<&'static str> {
    if cfg!(all(windows, target_arch = "x86_64")) {
        Some("uk-qol-windows-x64.exe")
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some("uk-qol-linux-x64.tar.gz")
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Some("uk-qol-macos-arm64.tar.gz")
    } else {
        None
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Asset {
    pub name: String,
    pub url: String,
    /// GitHub's own digest, lowercase hex, if it published one.
    pub digest: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Release {
    /// Version without the leading `v`.
    pub version: String,
    pub page_url: String,
    pub assets: Vec<Asset>,
    /// From `SHA256SUMS`: asset name → lowercase hex.
    pub sums: BTreeMap<String, String>,
}

impl Release {
    pub fn asset(&self, name: &str) -> Option<&Asset> {
        self.assets.iter().find(|a| a.name == name)
    }
}

fn agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .http_status_as_error(false)
        .user_agent(format!("uk-qol/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

/// Parses `sha256sum` output (`<hex>  <name>` or `<hex> *<name>`).
pub fn parse_sums(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .filter_map(|l| {
            let (hash, name) = l.trim().split_once(char::is_whitespace)?;
            let name = name.trim_start().trim_start_matches('*').trim();
            let ok = hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit());
            (ok && !name.is_empty()).then(|| (name.to_string(), hash.to_ascii_lowercase()))
        })
        .collect()
}

/// Reads the release JSON from the GitHub API (without the sums).
pub fn parse_release(json: &serde_json::Value) -> Result<Release, String> {
    let tag = json["tag_name"]
        .as_str()
        .ok_or_else(|| tr!("err_update_reply"))?;
    let assets = json["assets"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|a| {
                    Some(Asset {
                        name: a["name"].as_str()?.to_string(),
                        url: a["browser_download_url"].as_str()?.to_string(),
                        digest: a["digest"].as_str().and_then(|d| {
                            let (algo, hex) = d.split_once(':')?;
                            algo.eq_ignore_ascii_case("sha256")
                                .then(|| hex.to_ascii_lowercase())
                        }),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(Release {
        version: tag.trim_start_matches(['v', 'V']).to_string(),
        page_url: json["html_url"]
            .as_str()
            .unwrap_or(RELEASES_URL)
            .to_string(),
        assets,
        sums: BTreeMap::new(),
    })
}

/// Asks GitHub for the latest release. `Ok(None)`: none published yet.
pub fn latest() -> Result<Option<Release>, String> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let mut resp = agent(Duration::from_secs(20))
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| tr!("err_update_reach", e))?;
    match resp.status().as_u16() {
        200 => {}
        404 => return Ok(None),
        code => return Err(tr!("err_update_status", code)),
    }
    let json: serde_json::Value = resp
        .body_mut()
        .read_json()
        .map_err(|e| tr!("err_update_reply_detail", e))?;
    let mut rel = parse_release(&json)?;
    if let Some(sums) = rel.asset(SUMS_ASSET).cloned() {
        let bytes = download(&sums.url)?;
        rel.sums = parse_sums(&String::from_utf8_lossy(&bytes));
    }
    Ok(Some(rel))
}

/// The expected SHA-256 of an asset: from `SHA256SUMS`, cross-checked with
/// GitHub's digest. Errors if there is none or they disagree.
pub fn expected_sha(rel: &Release, asset: &Asset) -> Result<String, String> {
    let sum = rel.sums.get(&asset.name);
    match (sum, &asset.digest) {
        (Some(s), Some(d)) if s != d => Err(tr!("err_update_sum_mismatch", asset.name)),
        (Some(s), _) => Ok(s.clone()),
        (None, _) => Err(tr!("err_update_no_sum", asset.name)),
    }
}

/// Downloads a whole (small) file into memory.
fn download(url: &str) -> Result<Vec<u8>, String> {
    let resp = agent(Duration::from_secs(600))
        .get(url)
        .call()
        .map_err(|e| tr!("err_download", e))?;
    if resp.status().as_u16() != 200 {
        return Err(tr!("err_download", resp.status()));
    }
    let mut out = Vec::new();
    resp.into_body()
        .into_reader()
        .take(512 << 20)
        .read_to_end(&mut out)
        .map_err(|e| tr!("err_download", e))?;
    Ok(out)
}

/// Downloads an asset and checks its SHA-256.
pub fn fetch_verified(rel: &Release, name: &str) -> Result<Vec<u8>, String> {
    let asset = rel
        .asset(name)
        .ok_or_else(|| tr!("err_update_no_asset", rel.version, name))?;
    let want = expected_sha(rel, asset)?;
    let bytes = download(&asset.url)?;
    verify(&bytes, &want)?;
    Ok(bytes)
}

pub fn verify(bytes: &[u8], want: &str) -> Result<(), String> {
    let got = crate::install::sha256_hex(bytes);
    if got != want {
        return Err(tr!("err_update_corrupt", want, got));
    }
    Ok(())
}

/// Files in a zip, by file name (folders dropped).
pub fn unzip(bytes: &[u8]) -> Result<BTreeMap<String, Vec<u8>>, String> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|e| e.to_string())?;
    let mut out = BTreeMap::new();
    for i in 0..zip.len() {
        let mut f = zip.by_index(i).map_err(|e| e.to_string())?;
        if f.is_dir() {
            continue;
        }
        let name = f
            .name()
            .map_err(|e| e.to_string())?
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .to_string();
        let mut buf = Vec::new();
        f.by_ref()
            .take(64 << 20)
            .read_to_end(&mut buf)
            .map_err(|e| e.to_string())?;
        out.insert(name, buf);
    }
    Ok(out)
}

/// `winmm.dll` from the release's runtime zip, verified.
pub fn fetch_runtime(rel: &Release) -> Result<Vec<u8>, String> {
    let zip = fetch_verified(rel, RUNTIME_ASSET)?;
    unzip(&zip)?
        .remove(crate::install::DLL)
        .ok_or_else(|| tr!("err_update_no_dll", RUNTIME_ASSET))
}

/// Pattern files from the release (`mod id` → text), verified. Empty if the
/// release has none.
pub fn fetch_patterns(rel: &Release) -> Result<BTreeMap<String, Vec<u8>>, String> {
    if rel.asset(PATTERNS_ASSET).is_none() {
        return Ok(BTreeMap::new());
    }
    let zip = fetch_verified(rel, PATTERNS_ASSET)?;
    Ok(unzip(&zip)?
        .into_iter()
        .filter_map(|(name, bytes)| {
            let id = name.strip_suffix(".toml")?;
            // `<id>/patterns.toml` comes through as "patterns"; skip it.
            (id != "patterns").then(|| (id.to_string(), bytes))
        })
        .collect())
}

/// Pattern files to install from `rel`. A loader of a known version is
/// either the release's own (its patterns are built in) or gets replaced by
/// it in the same update, so pattern files only go to a loader whose version
/// isn't known.
pub fn pattern_updates(
    game_dir: &Path,
    manifest: &crate::install::InstallManifest,
    rel: &Release,
) -> Result<Vec<(String, Vec<u8>)>, String> {
    if parse_version(&manifest.runtime_version).is_some() {
        return Ok(Vec::new());
    }
    Ok(changed_patterns(game_dir, &fetch_patterns(rel)?))
}

/// Pattern files that differ from what's installed in `game_dir`.
pub fn changed_patterns(
    game_dir: &Path,
    fetched: &BTreeMap<String, Vec<u8>>,
) -> Vec<(String, Vec<u8>)> {
    fetched
        .iter()
        .filter(|(id, bytes)| {
            let p = game_dir.join(crate::install::pattern_rel(id));
            std::fs::read(p).map_or(true, |cur| &cur != *bytes)
        })
        .map(|(id, b)| (id.clone(), b.clone()))
        .collect()
}

// --- Self-update ---------------------------------------------------------

/// The running executable.
pub fn install_target() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| tr!("err_update_self", e))?;
    Ok(exe.canonicalize().unwrap_or(exe))
}

/// Removes what an earlier update left behind (old Windows exe, interrupted
/// staging folders).
pub fn cleanup_leftovers() {
    let Ok(target) = install_target() else { return };
    let _ = std::fs::remove_file(old_path(&target));
    let Some(dir) = target.parent() else { return };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        if e.file_name().to_string_lossy().starts_with(STAGING_PREFIX) {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

/// Where the replaced program is parked during the swap: `uk-qol.exe.old`.
fn old_path(target: &Path) -> PathBuf {
    let mut name = target.file_name().unwrap_or_default().to_os_string();
    name.push(".old");
    target.with_file_name(name)
}

/// Downloads, verifies and installs the manager from `rel` over the running
/// executable. Returns the path to start the new version with.
pub fn update_self(rel: &Release) -> Result<PathBuf, String> {
    let name = manager_asset().ok_or_else(|| tr!("err_update_platform"))?;
    let target = install_target()?;
    let dir = target.parent().ok_or_else(|| tr!("err_update_self", "?"))?;
    let bytes = fetch_verified(rel, name)?;
    let staging = dir.join(format!("{STAGING_PREFIX}{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|e| tr!("err_write", dir.display(), e))?;
    let result = (|| {
        let file = staging.join(name);
        std::fs::File::create(&file)
            .and_then(|mut f| f.write_all(&bytes).and_then(|()| f.sync_all()))
            .map_err(|e| tr!("err_write", file.display(), e))?;
        let new = unpack(&file, &staging)?;
        swap(&new, &target)?;
        Ok(target.clone())
    })();
    let _ = std::fs::remove_dir_all(&staging);
    result
}

fn unpack(file: &Path, staging: &Path) -> Result<PathBuf, String> {
    if cfg!(windows) {
        return Ok(file.to_path_buf());
    }
    let status = std::process::Command::new("tar")
        .arg("-xzf")
        .arg(file)
        .arg("-C")
        .arg(staging)
        .status()
        .map_err(|e| tr!("err_update_unpack", e))?;
    let new = staging.join("uk-qol");
    if !status.success() || !new.exists() {
        return Err(tr!("err_update_unpack", status));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&new, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| tr!("err_write", new.display(), e))?;
    }
    Ok(new)
}

/// Replaces `target` with `new`. The running program keeps working until it
/// exits.
fn swap(new: &Path, target: &Path) -> Result<(), String> {
    let fail = |e: std::io::Error| tr!("err_write", target.display(), e);
    if cfg!(unix) {
        // A rename over a running executable is fine on Unix.
        return std::fs::rename(new, target).map_err(fail);
    }
    // Windows can't overwrite a running exe but can rename it.
    let old = old_path(target);
    let _ = std::fs::remove_file(&old);
    std::fs::rename(target, &old).map_err(fail)?;
    if let Err(e) = std::fs::rename(new, target) {
        let _ = std::fs::rename(&old, target);
        return Err(fail(e));
    }
    // The old exe is still running; cleanup_leftovers deletes it next start.
    Ok(())
}

/// Starts the freshly installed version. The caller should exit right after.
pub fn relaunch(path: &Path) -> Result<(), String> {
    std::process::Command::new(path)
        .arg("gui")
        .spawn()
        .map(|_| ())
        .map_err(|e| tr!("err_update_restart", e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert_eq!(parse_version("v0.3.0"), Some((0, 3, 0)));
        assert_eq!(parse_version("1.2"), Some((1, 2, 0)));
        assert_eq!(parse_version("1.2.3-rc1"), Some((1, 2, 3)));
        assert_eq!(parse_version("nightly"), None);
        assert!(is_newer("v0.3.0", "0.2.0"));
        assert!(is_newer("v0.10.0", "0.9.9"));
        assert!(is_newer("v0.1.0", "unknown"));
        assert!(!is_newer("v0.2.0", "0.2.0"));
        assert!(!is_newer("v0.1.9", "0.2.0"));
        assert!(!is_newer("garbage", "0.2.0"));
    }

    #[test]
    fn sums() {
        let a = "a".repeat(64);
        let b = "B".repeat(64);
        let s = parse_sums(&format!(
            "{a}  uk-qol-windows-x64.exe\n{b} *SHA-file.zip\nbad line\n\n"
        ));
        assert_eq!(s.get("uk-qol-windows-x64.exe"), Some(&a));
        assert_eq!(s.get("SHA-file.zip"), Some(&"b".repeat(64)));
        assert_eq!(s.len(), 2);
    }

    fn sample() -> Release {
        let json = serde_json::json!({
            "tag_name": "v9.0.0",
            "html_url": "https://example.invalid/release",
            "assets": [
                {"name": "uk-qol-runtime-windows-x64.zip", "browser_download_url": "https://example.invalid/r", "digest": "SHA256:AB"},
                {"name": "SHA256SUMS", "browser_download_url": "https://example.invalid/s"},
                {"name": "nourl"}
            ],
        });
        parse_release(&json).unwrap()
    }

    #[test]
    fn release_parsing_and_checksums() {
        let mut r = sample();
        assert_eq!(r.version, "9.0.0");
        assert_eq!(r.assets.len(), 2);
        let rt = r.asset(RUNTIME_ASSET).unwrap().clone();
        assert_eq!(rt.digest.as_deref(), Some("ab"));
        // No SHA256SUMS entry: refused even with a GitHub digest.
        assert!(expected_sha(&r, &rt).is_err());
        r.sums.insert(RUNTIME_ASSET.into(), "cd".into());
        assert!(expected_sha(&r, &rt).is_err());
        r.sums.insert(RUNTIME_ASSET.into(), "ab".into());
        assert_eq!(expected_sha(&r, &rt).unwrap(), "ab");
        assert!(parse_release(&serde_json::json!({"message": "Not Found"})).is_err());
    }

    #[test]
    fn verify_and_unzip() {
        let mut buf = Vec::new();
        {
            let mut w = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
            let o = zip::write::SimpleFileOptions::default();
            w.start_file("winmm.dll", o).unwrap();
            w.write_all(b"dll").unwrap();
            w.start_file("patterns/skip-logos.toml", o).unwrap();
            w.write_all(b"a = \"C3\"").unwrap();
            w.finish().unwrap();
        }
        let files = unzip(&buf).unwrap();
        assert_eq!(files.get("winmm.dll").map(Vec::as_slice), Some(&b"dll"[..]));
        assert!(files.contains_key("skip-logos.toml"));
        assert!(verify(&buf, &crate::install::sha256_hex(&buf)).is_ok());
        assert!(verify(&buf, &"0".repeat(64)).is_err());
    }

    #[test]
    fn no_pattern_files_for_a_known_loader() {
        let game = crate::install::tests::fake_game();
        let m = crate::install::InstallManifest {
            runtime_version: "1.0.0".into(),
            ..Default::default()
        };
        // Returns before downloading anything.
        assert!(
            pattern_updates(game.path(), &m, &sample())
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn changed_pattern_detection() {
        let game = crate::install::tests::fake_game();
        let mut fetched = BTreeMap::new();
        fetched.insert("a".to_string(), b"x".to_vec());
        assert_eq!(changed_patterns(game.path(), &fetched).len(), 1);
        let p = game.path().join(crate::install::pattern_rel("a"));
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, b"x").unwrap();
        assert!(changed_patterns(game.path(), &fetched).is_empty());
    }

    /// Against the live repository: with no release published this must be
    /// `Ok(None)`, not an error. `cargo test -p uk-qol-manager -- --ignored`
    #[test]
    #[ignore]
    fn live_latest() {
        let r = latest();
        eprintln!("{:?}", r.as_ref().map(|o| o.as_ref().map(|r| &r.version)));
        assert!(r.is_ok());
    }
}
