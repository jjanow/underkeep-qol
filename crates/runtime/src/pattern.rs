//! Byte-pattern scanner. Mods locate game code by patterns, never by fixed
//! addresses, so a game update either still matches or fails loudly.
//!
//! A pattern is space-separated hex bytes with `?` or `??` as wildcards:
//! `"48 8B 05 ?? ?? ?? ?? E8 ? ? ? ?"`. A pattern must match exactly once
//! (`find_unique`); zero or several matches mean the mod using it is off for
//! that build. Everything here works on an image-layout buffer indexed by RVA
//! (`pe::map_file`, or the mapped module in game).

use crate::pe::Headers;
use std::fmt;
use std::str::FromStr;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatternError {
    Empty,
    BadToken(String),
    /// A pattern can't start with a wildcard: it would only shift the match.
    LeadingWildcard,
}

impl fmt::Display for PatternError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PatternError::Empty => write!(f, "empty pattern"),
            PatternError::BadToken(t) => write!(f, "bad pattern token {t:?}"),
            PatternError::LeadingWildcard => write!(f, "pattern starts with a wildcard"),
        }
    }
}

impl std::error::Error for PatternError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScanError {
    NotFound,
    /// Matched more than once; `first` holds up to 8 match RVAs.
    Ambiguous {
        count: usize,
        first: Vec<u32>,
    },
    /// The bytes at the address aren't the expected instruction.
    BadInstruction {
        rva: u32,
    },
    OutOfRange {
        rva: u32,
    },
}

impl fmt::Display for ScanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ScanError::NotFound => write!(f, "pattern not found"),
            ScanError::Ambiguous { count, first } => {
                write!(f, "pattern matched {count} times (at")?;
                for r in first {
                    write!(f, " {r:#x}")?;
                }
                write!(f, ")")
            }
            ScanError::BadInstruction { rva } => write!(f, "unexpected instruction at {rva:#x}"),
            ScanError::OutOfRange { rva } => write!(f, "address {rva:#x} outside the image"),
        }
    }
}

impl std::error::Error for ScanError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pattern {
    /// `None` is a wildcard.
    bytes: Vec<Option<u8>>,
}

impl FromStr for Pattern {
    type Err = PatternError;

    fn from_str(s: &str) -> Result<Pattern, PatternError> {
        let bytes = s
            .split_whitespace()
            .map(|t| match t {
                "?" | "??" => Ok(None),
                _ if t.len() == 2 => u8::from_str_radix(t, 16)
                    .map(Some)
                    .map_err(|_| PatternError::BadToken(t.into())),
                _ => Err(PatternError::BadToken(t.into())),
            })
            .collect::<Result<Vec<_>, _>>()?;
        match bytes.first() {
            None => Err(PatternError::Empty),
            Some(None) => Err(PatternError::LeadingWildcard),
            Some(Some(_)) => Ok(Pattern { bytes }),
        }
    }
}

impl fmt::Display for Pattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, b) in self.bytes.iter().enumerate() {
            if i > 0 {
                f.write_str(" ")?;
            }
            match b {
                Some(b) => write!(f, "{b:02X}")?,
                None => f.write_str("??")?,
            }
        }
        Ok(())
    }
}

impl Pattern {
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    pub fn matches_at(&self, hay: &[u8], at: usize) -> bool {
        hay.get(at..at + self.bytes.len()).is_some_and(|w| {
            w.iter()
                .zip(&self.bytes)
                .all(|(h, p)| p.is_none_or(|p| p == *h))
        })
    }

    /// All match offsets in `hay`, in order (overlapping matches included).
    pub fn find_iter<'a>(&'a self, hay: &'a [u8]) -> impl Iterator<Item = usize> + 'a {
        let first = self.bytes[0].expect("parse rejects a leading wildcard");
        let last_start = hay.len().checked_sub(self.bytes.len());
        hay.iter()
            .enumerate()
            .take(last_start.map_or(0, |n| n + 1))
            .filter(move |&(i, &b)| b == first && self.matches_at(hay, i))
            .map(|(i, _)| i)
    }

    /// The single match offset in `hay`.
    pub fn find_unique(&self, hay: &[u8]) -> Result<usize, ScanError> {
        unique(self.find_iter(hay))
    }
}

fn unique(it: impl Iterator<Item = usize>) -> Result<usize, ScanError> {
    let mut first = Vec::new();
    let mut count = 0;
    for i in it {
        if first.len() < 8 {
            first.push(i as u32);
        }
        count += 1;
    }
    match count {
        0 => Err(ScanError::NotFound),
        1 => Ok(first[0] as usize),
        _ => Err(ScanError::Ambiguous { count, first }),
    }
}

/// A mapped image (indexed by RVA) and its headers.
pub struct Image<'a> {
    pub bytes: &'a [u8],
    pub headers: Headers,
}

impl<'a> Image<'a> {
    pub fn new(bytes: &'a [u8]) -> Result<Image<'a>, crate::pe::PeError> {
        Ok(Image {
            headers: Headers::parse(bytes)?,
            bytes,
        })
    }

    /// RVAs of all matches in executable sections.
    pub fn find_code(&self, pat: &Pattern) -> Vec<u32> {
        let mut out = Vec::new();
        for s in self.headers.sections.iter().filter(|s| s.is_executable()) {
            let r = s.rva_range();
            let Some(hay) = self.bytes.get(r.start..r.end.min(self.bytes.len())) else {
                continue;
            };
            out.extend(pat.find_iter(hay).map(|i| (r.start + i) as u32));
        }
        out
    }

    /// RVA of the single match in executable sections.
    pub fn scan(&self, pat: &Pattern) -> Result<u32, ScanError> {
        unique(self.find_code(pat).into_iter().map(|r| r as usize)).map(|r| r as u32)
    }

    /// RVA of the single match in the named section.
    pub fn scan_section(&self, section: &str, pat: &Pattern) -> Result<u32, ScanError> {
        let s = self.headers.section(section).ok_or(ScanError::NotFound)?;
        let r = s.rva_range();
        let hay = self
            .bytes
            .get(r.start..r.end.min(self.bytes.len()))
            .ok_or(ScanError::NotFound)?;
        pat.find_unique(hay).map(|i| (r.start + i) as u32)
    }

    pub fn read_i32(&self, rva: u32) -> Result<i32, ScanError> {
        let i = rva as usize;
        self.bytes
            .get(i..i + 4)
            .map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .ok_or(ScanError::OutOfRange { rva })
    }

    /// Target of a RIP-relative operand: the instruction at `insn` is
    /// `insn_len` bytes long and its disp32 sits `disp_offset` bytes in.
    /// E.g. `48 8B 05 <disp32>` (mov rax, [rip+disp]) is `(insn, 3, 7)`.
    pub fn rip_target(&self, insn: u32, disp_offset: u32, insn_len: u32) -> Result<u32, ScanError> {
        let disp = self.read_i32(insn + disp_offset)?;
        let t = (insn as i64) + insn_len as i64 + disp as i64;
        if t < 0 || t as u64 >= self.bytes.len() as u64 {
            return Err(ScanError::OutOfRange { rva: t as u32 });
        }
        Ok(t as u32)
    }

    /// Target of the `call rel32` (E8) or `jmp rel32` (E9) at `insn`.
    pub fn call_target(&self, insn: u32) -> Result<u32, ScanError> {
        match self.bytes.get(insn as usize) {
            Some(0xE8 | 0xE9) => self.rip_target(insn, 1, 5),
            Some(_) => Err(ScanError::BadInstruction { rva: insn }),
            None => Err(ScanError::OutOfRange { rva: insn }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pe::{self, SCN_MEM_EXECUTE, tests::build_pe};

    fn p(s: &str) -> Pattern {
        s.parse().unwrap()
    }

    #[test]
    fn parse() {
        assert_eq!(p("48 8b ?? ? C3").to_string(), "48 8B ?? ?? C3");
        assert_eq!("".parse::<Pattern>(), Err(PatternError::Empty));
        assert_eq!(
            "?? 48".parse::<Pattern>(),
            Err(PatternError::LeadingWildcard)
        );
        assert_eq!(
            "48 8".parse::<Pattern>(),
            Err(PatternError::BadToken("8".into()))
        );
        assert_eq!(
            "48 zz".parse::<Pattern>(),
            Err(PatternError::BadToken("zz".into()))
        );
    }

    #[test]
    fn find() {
        let hay = [0x90, 0x48, 0x8B, 0x05, 0x48, 0x8B, 0x06, 0x48];
        assert_eq!(p("48 8B ??").find_iter(&hay).collect::<Vec<_>>(), [1, 4]);
        assert_eq!(p("48 8B 06").find_unique(&hay), Ok(4));
        assert_eq!(p("48 8B 07").find_unique(&hay), Err(ScanError::NotFound));
        assert_eq!(
            p("48 8B").find_unique(&hay),
            Err(ScanError::Ambiguous {
                count: 2,
                first: vec![1, 4]
            })
        );
        // Doesn't run past the end, and handles a pattern longer than hay.
        assert_eq!(p("48 ??").find_iter(&hay).collect::<Vec<_>>(), [1, 4]);
        assert_eq!(p("90 48 8B 05 48 8B 06 48 00").find_iter(&hay).count(), 0);
        // Overlapping matches.
        assert_eq!(p("AA AA").find_iter(&[0xAA; 3]).count(), 2);
    }

    /// .text at 0x1000: mov rax,[rip+X]; call F; nop; ret; F: ret.
    /// .rdata at 0x2000 with a copy of the code bytes (must not match code scans).
    fn image() -> Vec<u8> {
        // mov rax, [rip + disp] at 0x1000, len 7 -> target 0x2000.
        let disp: i32 = 0x2000 - 0x1007;
        let mut code = vec![0x48, 0x8B, 0x05];
        code.extend_from_slice(&disp.to_le_bytes());
        // call at 0x1007, len 5 -> target 0x100E.
        code.push(0xE8);
        code.extend_from_slice(&(0x100Ei32 - 0x100C).to_le_bytes());
        code.extend_from_slice(&[0x90, 0xC3, 0xC3]);
        let rdata = code.clone();
        pe::map_file(&build_pe(&[
            (".text", 0x1000, &code, SCN_MEM_EXECUTE),
            (".rdata", 0x2000, &rdata, 0),
        ]))
        .unwrap()
    }

    #[test]
    fn image_scan_and_resolve() {
        let bytes = image();
        let img = Image::new(&bytes).unwrap();
        // Only .text is scanned for code.
        let mov = img.scan(&p("48 8B 05 ?? ?? ?? ?? E8")).unwrap();
        assert_eq!(mov, 0x1000);
        assert_eq!(img.rip_target(mov, 3, 7), Ok(0x2000));
        assert_eq!(img.call_target(0x1007), Ok(0x100E));
        assert_eq!(
            img.call_target(0x1000),
            Err(ScanError::BadInstruction { rva: 0x1000 })
        );
        assert_eq!(img.scan_section(".rdata", &p("48 8B 05")), Ok(0x2000));
        assert_eq!(
            img.scan_section(".nope", &p("48")),
            Err(ScanError::NotFound)
        );
    }

    /// Against the real exe (`UNDERKEEP_DIR`): find the code that loads the
    /// address of a known string with `lea r64, [rip+disp32]` and check that
    /// resolution lands on it, then check that a pattern cut from that site
    /// and scanned back finds it. No game bytes are stored in the repo.
    #[test]
    fn game_exe_lea_to_string() {
        let Some(path) = pe::tests::game_exe() else {
            return;
        };
        let bytes = pe::map_file(&std::fs::read(path).unwrap()).unwrap();
        let img = Image::new(&bytes).unwrap();
        let needle = b"\0Save location: ";
        let s = img
            .scan_section(".rdata", &p(&hex(needle)))
            .expect("string in .rdata")
            + 1;

        let t = std::time::Instant::now();
        let mut sites = Vec::new();
        for rex in ["48", "4C"] {
            for at in img.find_code(&p(&format!("{rex} 8D"))) {
                let modrm = bytes[at as usize + 2];
                if modrm & 0xC7 == 0x05 && img.rip_target(at, 3, 7) == Ok(s) {
                    sites.push(at);
                }
            }
        }
        eprintln!(
            "lea sites for string at {s:#x}: {sites:x?} ({:?})",
            t.elapsed()
        );
        assert!(!sites.is_empty(), "no lea references the string");

        // A 24-byte pattern from the site with the disp32 wildcarded.
        let at = sites[0] as usize;
        let mut toks: Vec<String> = bytes[at..at + 24]
            .iter()
            .map(|b| format!("{b:02X}"))
            .collect();
        for t in &mut toks[3..7] {
            *t = "??".into();
        }
        let pat = p(&toks.join(" "));
        match img.scan(&pat) {
            Ok(r) => assert_eq!(r, sites[0]),
            Err(ScanError::Ambiguous { first, .. }) => assert!(first.contains(&sites[0])),
            Err(e) => panic!("{e}"),
        }
    }

    fn hex(b: &[u8]) -> String {
        b.iter()
            .map(|b| format!("{b:02X}"))
            .collect::<Vec<_>>()
            .join(" ")
    }
}
