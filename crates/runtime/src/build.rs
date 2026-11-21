//! Build detection: which game exe the runtime is running in. Mods match their
//! patterns against the code, not against this, but the hash and version go
//! into the log and bug reports, and let the manager say which builds a mod
//! release was tested on.

use std::fmt;
use std::io::{self, Read};
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::pe;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildInfo {
    pub sha256: [u8; 32],
    /// The game's version string (`1.0.5`), if the exe has one we recognise.
    pub version: Option<String>,
}

impl BuildInfo {
    pub fn sha256_hex(&self) -> String {
        hex(&self.sha256)
    }
}

impl fmt::Display for BuildInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "exe sha256 {}, game version {}",
            self.sha256_hex(),
            self.version.as_deref().unwrap_or("unknown")
        )
    }
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn sha256_file(path: &Path) -> io::Result<[u8; 32]> {
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().into())
}

/// `X.Y.Z` with 1–4 digit decimal parts.
fn is_version(s: &[u8]) -> bool {
    let parts: Vec<&[u8]> = s.split(|&c| c == b'.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| (1..=4).contains(&p.len()) && p.iter().all(u8::is_ascii_digit))
}

/// Finds the game version in `.rdata` of an image-layout buffer. Prefers the
/// title-screen literal `version X.Y.Z`; otherwise takes a standalone
/// `X.Y.Z` string if exactly one distinct one exists.
pub fn find_version(image: &[u8], headers: &pe::Headers) -> Option<String> {
    let rdata = image.get(headers.section(".rdata")?.rva_range())?;
    let strings = rdata.split(|&c| c == 0);
    let mut bare: Vec<&[u8]> = Vec::new();
    for s in strings {
        if let Some(v) = s.strip_prefix(b"version ")
            && is_version(v)
        {
            return Some(String::from_utf8_lossy(v).into_owned());
        }
        if is_version(s) && !bare.contains(&s) {
            bare.push(s);
        }
    }
    match bare.as_slice() {
        [v] => Some(String::from_utf8_lossy(v).into_owned()),
        _ => None,
    }
}

#[derive(Debug)]
pub enum DetectError {
    Io(io::Error),
    Pe(pe::PeError),
}

impl fmt::Display for DetectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DetectError::Io(e) => write!(f, "{e}"),
            DetectError::Pe(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for DetectError {}

pub fn detect_from_file(path: &Path) -> Result<BuildInfo, DetectError> {
    let file = std::fs::read(path).map_err(DetectError::Io)?;
    let sha256 = Sha256::digest(&file).into();
    let headers = pe::Headers::parse(&file).map_err(DetectError::Pe)?;
    let image = pe::map_file(&file).map_err(DetectError::Pe)?;
    Ok(BuildInfo {
        sha256,
        version: find_version(&image, &headers),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pe::tests::{build_pe, game_exe};

    fn version_of(rdata: &[u8]) -> Option<String> {
        let f = build_pe(&[
            (".text", 0x1000, &[0xc3], pe::SCN_MEM_EXECUTE),
            (".rdata", 0x2000, rdata, 0),
        ]);
        let h = pe::Headers::parse(&f).unwrap();
        find_version(&pe::map_file(&f).unwrap(), &h)
    }

    #[test]
    fn prefers_title_literal() {
        assert_eq!(
            version_of(b"\x002.0.0\0version 1.0.5\0Game version: %1\0").as_deref(),
            Some("1.0.5")
        );
    }

    #[test]
    fn bare_fallback_needs_one() {
        assert_eq!(
            version_of(b"x\x001.2.3\0y\x001.2.3\0").as_deref(),
            Some("1.2.3")
        );
        assert_eq!(version_of(b"x\x001.2.3\0y\x003.2.1\0"), None);
        assert_eq!(version_of(b"version 1.2\0v1.2.3\0"), None);
    }

    #[test]
    fn hashes_file() {
        let dir = std::env::temp_dir().join(format!("uk-qol-build-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("abc");
        std::fs::write(&p, b"abc").unwrap();
        assert_eq!(
            hex(&sha256_file(&p).unwrap()),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let _ = std::fs::remove_file(&p);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn game_exe_build() {
        let Some(p) = game_exe() else { return };
        let b = detect_from_file(&p).unwrap();
        println!("{b}");
        assert_eq!(b.version.as_deref(), Some("1.0.6"));
    }
}
