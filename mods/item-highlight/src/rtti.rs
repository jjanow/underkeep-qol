//! Finds a class's vtable from its MSVC RTTI name, so the mod can tell an
//! entity's class without a byte pattern per class.
//!
//! MSVC x64 RTTI: the class name (`.?AUItem@@`) sits 0x10 bytes into its
//! TypeDescriptor (in `.data`). A CompleteObjectLocator (in `.rdata`) is six
//! u32s: signature 1, offset of the subobject (0 for the most-derived
//! vtable), cdOffset, then the RVAs of the TypeDescriptor, the class
//! hierarchy and the locator itself. The vtable's slot -1 holds the
//! locator's address, so the vtable starts 8 bytes after that pointer.

/// RVA of the vtable of the class whose RTTI name is `name` (for example
/// `".?AUItem@@"`), searched in an image mapped at its RVAs (`image[rva]`).
/// `bases` are the values a pointer in the image may be relative to: the
/// address the image is loaded at (pointers are relocated in memory) and
/// its preferred base (an image laid out from the file). `sections` are
/// (rva, size) of the data sections to search. `None` if the name, its
/// locator or the vtable isn't found, or isn't unique.
pub fn find_vtable(
    image: &[u8],
    sections: &[(u32, u32)],
    bases: &[u64],
    name: &str,
) -> Option<u32> {
    let mut needle = name.as_bytes().to_vec();
    needle.push(0);
    let in_sections = |f: &mut dyn FnMut(usize)| {
        for &(rva, size) in sections {
            let start = rva as usize;
            let end = (start + size as usize).min(image.len());
            for at in start..end {
                f(at);
            }
        }
    };

    // The TypeDescriptor: the name, at +0x10, 8-byte aligned descriptor.
    let mut td = None;
    let mut count = 0;
    in_sections(&mut |at| {
        if at >= 0x10 && (at - 0x10) % 8 == 0 && image[at..].starts_with(&needle) {
            td = Some((at - 0x10) as u32);
            count += 1;
        }
    });
    if count != 1 {
        return None;
    }
    let td = td?;

    let u32_at = |at: usize| -> Option<u32> {
        Some(u32::from_le_bytes(image.get(at..at + 4)?.try_into().ok()?))
    };
    let u64_at = |at: usize| -> Option<u64> {
        Some(u64::from_le_bytes(image.get(at..at + 8)?.try_into().ok()?))
    };

    // The most-derived locator: signature 1, offset 0, this TypeDescriptor,
    // and its self RVA pointing at itself.
    let mut col = None;
    let mut count = 0;
    in_sections(&mut |at| {
        if at % 4 == 0
            && u32_at(at) == Some(1)
            && u32_at(at + 4) == Some(0)
            && u32_at(at + 12) == Some(td)
            && u32_at(at + 20) == Some(at as u32)
        {
            col = Some(at as u32);
            count += 1;
        }
    });
    if count != 1 {
        return None;
    }
    let col = col? as u64;

    // The pointer to the locator, right before the vtable.
    let mut vt = None;
    let mut count = 0;
    in_sections(&mut |at| {
        if at % 8 == 0
            && let Some(p) = u64_at(at)
            && bases.iter().any(|&b| b.wrapping_add(col) == p)
        {
            vt = Some(at as u32 + 8);
            count += 1;
        }
    });
    (count == 1).then_some(vt?)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: u64 = 0x1_4000_0000;

    /// An image with one class: name at 0x1010 (descriptor 0x1000), locator
    /// at 0x2000, vtable at 0x2108 (locator pointer at 0x2100).
    fn image(name: &str) -> Vec<u8> {
        let mut img = vec![0u8; 0x3000];
        img[0x1010..0x1010 + name.len()].copy_from_slice(name.as_bytes());
        let col: [u32; 6] = [1, 0, 0, 0x1000, 0x1800, 0x2000];
        for (i, v) in col.iter().enumerate() {
            img[0x2000 + i * 4..0x2004 + i * 4].copy_from_slice(&v.to_le_bytes());
        }
        img[0x2100..0x2108].copy_from_slice(&(BASE + 0x2000).to_le_bytes());
        img
    }

    const SECTIONS: [(u32, u32); 2] = [(0x1000, 0x1000), (0x2000, 0x1000)];

    #[test]
    fn finds_the_vtable() {
        let img = image(".?AUItem@@");
        assert_eq!(
            find_vtable(&img, &SECTIONS, &[BASE], ".?AUItem@@"),
            Some(0x2108)
        );
    }

    #[test]
    fn relocated_pointer() {
        let mut img = image(".?AUItem@@");
        let loaded = 0x7ff6_0000_0000u64;
        img[0x2100..0x2108].copy_from_slice(&(loaded + 0x2000).to_le_bytes());
        assert_eq!(
            find_vtable(&img, &SECTIONS, &[loaded, BASE], ".?AUItem@@"),
            Some(0x2108)
        );
    }

    #[test]
    fn name_must_match_exactly() {
        let img = image(".?AUItemX@@");
        assert_eq!(find_vtable(&img, &SECTIONS, &[BASE], ".?AUItem@@"), None);
    }

    #[test]
    fn missing_locator() {
        let mut img = image(".?AUItem@@");
        img[0x2014..0x2018].copy_from_slice(&0u32.to_le_bytes());
        assert_eq!(find_vtable(&img, &SECTIONS, &[BASE], ".?AUItem@@"), None);
    }
}
