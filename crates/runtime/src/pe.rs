//! Minimal PE32+ reader: just enough to find the sections of the game exe,
//! either as mapped in memory (in game) or from the file on disk (tests,
//! tools). Everything downstream works in RVA space on an image-layout buffer.

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeError {
    Truncated,
    BadMagic(&'static str),
    NotPe32Plus,
}

impl fmt::Display for PeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PeError::Truncated => write!(f, "PE headers truncated"),
            PeError::BadMagic(what) => write!(f, "bad {what} signature"),
            PeError::NotPe32Plus => write!(f, "not a PE32+ (64-bit) image"),
        }
    }
}

impl std::error::Error for PeError {}

pub const SCN_MEM_EXECUTE: u32 = 0x2000_0000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub name: String,
    /// Address relative to the image base.
    pub rva: u32,
    /// Size in memory.
    pub virtual_size: u32,
    /// Offset and size in the file.
    pub raw_offset: u32,
    pub raw_size: u32,
    pub characteristics: u32,
}

impl Section {
    pub fn is_executable(&self) -> bool {
        self.characteristics & SCN_MEM_EXECUTE != 0
    }

    /// RVA range of the section in the mapped image (including zero fill).
    pub fn rva_range(&self) -> std::ops::Range<usize> {
        self.rva as usize..self.rva as usize + self.virtual_size as usize
    }
}

#[derive(Debug, Clone)]
pub struct Headers {
    pub image_base: u64,
    pub size_of_image: u32,
    pub size_of_headers: u32,
    pub sections: Vec<Section>,
}

fn u16_at(b: &[u8], o: usize) -> Result<u16, PeError> {
    b.get(o..o + 2)
        .map(|s| u16::from_le_bytes([s[0], s[1]]))
        .ok_or(PeError::Truncated)
}

fn u32_at(b: &[u8], o: usize) -> Result<u32, PeError> {
    b.get(o..o + 4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
        .ok_or(PeError::Truncated)
}

fn u64_at(b: &[u8], o: usize) -> Result<u64, PeError> {
    Ok(u32_at(b, o)? as u64 | (u32_at(b, o + 4)? as u64) << 32)
}

impl Headers {
    /// Parses the headers at the start of `b` (file or image layout; the
    /// headers are the same in both).
    pub fn parse(b: &[u8]) -> Result<Headers, PeError> {
        if b.get(..2) != Some(b"MZ") {
            return Err(PeError::BadMagic("MZ"));
        }
        let nt = u32_at(b, 0x3c)? as usize;
        if b.get(nt..nt + 4) != Some(b"PE\0\0") {
            return Err(PeError::BadMagic("PE"));
        }
        let coff = nt + 4;
        let nsections = u16_at(b, coff + 2)? as usize;
        let opt_size = u16_at(b, coff + 16)? as usize;
        let opt = coff + 20;
        if u16_at(b, opt)? != 0x20b {
            return Err(PeError::NotPe32Plus);
        }
        let image_base = u64_at(b, opt + 24)?;
        let size_of_image = u32_at(b, opt + 56)?;
        let size_of_headers = u32_at(b, opt + 60)?;
        let mut sections = Vec::with_capacity(nsections);
        for i in 0..nsections {
            let s = opt + opt_size + i * 40;
            let raw_name = b.get(s..s + 8).ok_or(PeError::Truncated)?;
            let end = raw_name.iter().position(|&c| c == 0).unwrap_or(8);
            sections.push(Section {
                name: String::from_utf8_lossy(&raw_name[..end]).into_owned(),
                virtual_size: u32_at(b, s + 8)?,
                rva: u32_at(b, s + 12)?,
                raw_size: u32_at(b, s + 16)?,
                raw_offset: u32_at(b, s + 20)?,
                characteristics: u32_at(b, s + 36)?,
            });
        }
        Ok(Headers {
            image_base,
            size_of_image,
            size_of_headers,
            sections,
        })
    }

    pub fn section(&self, name: &str) -> Option<&Section> {
        self.sections.iter().find(|s| s.name == name)
    }
}

/// Lays out a PE file as the loader would map it (sections at their RVAs,
/// zero fill), without applying relocations. Patterns and RVAs computed on
/// the result match the in-memory image.
pub fn map_file(file: &[u8]) -> Result<Vec<u8>, PeError> {
    let h = Headers::parse(file)?;
    let mut img = vec![0u8; h.size_of_image as usize];
    let hdr = (h.size_of_headers as usize).min(file.len()).min(img.len());
    img[..hdr].copy_from_slice(&file[..hdr]);
    for s in &h.sections {
        // Raw data is padded to the file alignment; only `virtual_size`
        // bytes of it are mapped.
        let n = s.raw_size.min(s.virtual_size) as usize;
        let src = file
            .get(s.raw_offset as usize..s.raw_offset as usize + n)
            .ok_or(PeError::Truncated)?;
        img.get_mut(s.rva as usize..s.rva as usize + n)
            .ok_or(PeError::Truncated)?
            .copy_from_slice(src);
    }
    Ok(img)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Builds a tiny PE32+ file with the given sections (name, rva, bytes,
    /// characteristics). File alignment 0x200, section alignment 0x1000.
    pub fn build_pe(sections: &[(&str, u32, &[u8], u32)]) -> Vec<u8> {
        let nt = 0x80usize;
        let opt_size = 240usize;
        let shdr = nt + 24 + opt_size;
        let headers_end = shdr + 40 * sections.len();
        let size_of_headers = headers_end.next_multiple_of(0x200);
        let mut f = vec![0u8; size_of_headers];
        f[..2].copy_from_slice(b"MZ");
        f[0x3c..0x40].copy_from_slice(&(nt as u32).to_le_bytes());
        f[nt..nt + 4].copy_from_slice(b"PE\0\0");
        f[nt + 4..nt + 6].copy_from_slice(&0x8664u16.to_le_bytes());
        f[nt + 6..nt + 8].copy_from_slice(&(sections.len() as u16).to_le_bytes());
        f[nt + 20..nt + 22].copy_from_slice(&(opt_size as u16).to_le_bytes());
        let opt = nt + 24;
        f[opt..opt + 2].copy_from_slice(&0x20bu16.to_le_bytes());
        f[opt + 24..opt + 32].copy_from_slice(&0x1_4000_0000u64.to_le_bytes());
        let end_rva = sections
            .iter()
            .map(|s| s.1 as usize + s.2.len())
            .max()
            .unwrap_or(0x1000)
            .next_multiple_of(0x1000);
        f[opt + 56..opt + 60].copy_from_slice(&(end_rva as u32).to_le_bytes());
        f[opt + 60..opt + 64].copy_from_slice(&(size_of_headers as u32).to_le_bytes());
        for (i, (name, rva, data, ch)) in sections.iter().enumerate() {
            let raw_offset = f.len();
            let raw_size = data.len().next_multiple_of(0x200);
            let s = shdr + i * 40;
            f[s..s + name.len()].copy_from_slice(name.as_bytes());
            f[s + 8..s + 12].copy_from_slice(&(data.len() as u32).to_le_bytes());
            f[s + 12..s + 16].copy_from_slice(&rva.to_le_bytes());
            f[s + 16..s + 20].copy_from_slice(&(raw_size as u32).to_le_bytes());
            f[s + 20..s + 24].copy_from_slice(&(raw_offset as u32).to_le_bytes());
            f[s + 36..s + 40].copy_from_slice(&ch.to_le_bytes());
            f.extend_from_slice(data);
            f.resize(raw_offset + raw_size, 0);
        }
        f
    }

    #[test]
    fn parse_and_map() {
        let f = build_pe(&[
            (".text", 0x1000, &[0x90, 0xc3], SCN_MEM_EXECUTE),
            (".rdata", 0x2000, b"1.0.5\0", 0),
        ]);
        let h = Headers::parse(&f).unwrap();
        assert_eq!(h.image_base, 0x1_4000_0000);
        assert_eq!(h.sections.len(), 2);
        assert!(h.section(".text").unwrap().is_executable());
        let img = map_file(&f).unwrap();
        assert_eq!(img.len(), 0x3000);
        assert_eq!(&img[0x1000..0x1002], &[0x90, 0xc3]);
        assert_eq!(&img[0x2000..0x2006], b"1.0.5\0");
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(
            Headers::parse(b"nope").unwrap_err(),
            PeError::BadMagic("MZ")
        );
    }

    /// Path of the real game exe, from `UNDERKEEP_DIR`. Tests that need it
    /// skip when it's unset.
    pub fn game_exe() -> Option<std::path::PathBuf> {
        let p = std::path::PathBuf::from(std::env::var_os("UNDERKEEP_DIR")?).join("underkeep.exe");
        p.exists().then_some(p)
    }

    #[test]
    fn game_exe_sections() {
        let Some(p) = game_exe() else { return };
        let f = std::fs::read(p).unwrap();
        let h = Headers::parse(&f).unwrap();
        assert!(h.section(".text").unwrap().is_executable());
        let img = map_file(&f).unwrap();
        assert_eq!(img.len(), h.size_of_image as usize);
    }
}
