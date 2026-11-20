//! The stats page's row vector and how the mod's rows go into it.
//!
//! The page keeps its rows in a vector at `page+0x28`: `{count i32,
//! capacity i32, data*}`, rows of 0x140 bytes:
//! - `+0x00` kind (i32): 0 a stat, 1 a section header, 2 an active effect;
//! - `+0x08` the text key (a game `String`), `+0x20` the value (`String`);
//! - the rest plain data (zero for stats; an effect's data for kind 2).
//!
//! The game rebuilds the rows every frame (`stats_rebuild`) and appends them
//! with `row_push`, which moves a row in (it takes the row's two strings and
//! empties the source's). Nothing in a row points into the row itself (a
//! `String`'s bytes are a literal or a heap buffer), so rows can be moved
//! around bytewise, as the vector's own growth does.
//!
//! The mod appends its rows with the game's push, then moves them in front
//! of the Offense and Defense headers and to the end, so each section's
//! extra rows come right after its own.

use crate::stats::Section;

pub const ROW_SIZE: usize = 0x140;
pub const ROW_KIND: usize = 0x00;
pub const ROW_KEY: usize = 0x08;
pub const ROW_VALUE: usize = 0x20;
pub const KIND_STAT: i32 = 0;
pub const KIND_HEADER: i32 = 1;

/// Offset of the row vector in the stats page.
pub const PAGE_ROWS: usize = 0x28;
const VEC_COUNT: usize = 0x00;
const VEC_CAPACITY: usize = 0x04;
const VEC_DATA: usize = 0x08;
/// More rows than this is taken as a misread.
const MAX_ROWS: i32 = 4096;

pub const HEADER_OFFENSE: &str = "stats_section_offense";
pub const HEADER_DEFENSE: &str = "stats_section_defense";

/// The game's `String` (0x18 bytes).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct GString {
    pub buf: usize,
    pub data: usize,
    pub len: i32,
    pub pad: u32,
}

impl GString {
    /// A literal over `s` (which must live as long as anything uses it).
    pub fn literal(s: &str) -> GString {
        GString {
            buf: 0,
            data: s.as_ptr() as usize,
            len: s.len() as i32,
            pad: 0,
        }
    }
}

/// Memory access, so the logic runs on fake memory in tests.
pub trait Mem {
    fn read_i32(&self, at: usize) -> Option<i32>;
    fn read_usize(&self, at: usize) -> Option<usize>;
    fn read_bytes(&self, at: usize, len: usize) -> Option<Vec<u8>>;
    /// Writes `bytes` at `at` (already known to be the vector's data).
    fn write_bytes(&self, at: usize, bytes: &[u8]);
}

/// The vector's count and data pointer, if sane.
pub fn header(m: &dyn Mem, vec: usize) -> Option<(usize, usize)> {
    let n = m.read_i32(vec + VEC_COUNT)?;
    let cap = m.read_i32(vec + VEC_CAPACITY)?;
    if !(0..=MAX_ROWS).contains(&n) || cap < n {
        return None;
    }
    let data = m.read_usize(vec + VEC_DATA)?;
    if n > 0 && data == 0 {
        return None;
    }
    Some((n as usize, data))
}

/// A row's text key.
fn key(m: &dyn Mem, row: usize) -> Option<Vec<u8>> {
    let len = m.read_i32(row + ROW_KEY + 0x10)?;
    if !(0..=256).contains(&len) {
        return None;
    }
    let data = m.read_usize(row + ROW_KEY + 0x08)?;
    m.read_bytes(data, len as usize)
}

/// What the mod needs from the game's rows: where the Offense and Defense
/// headers are, and a stat row to copy the plain data of.
pub struct Layout {
    pub count: usize,
    pub offense: Option<usize>,
    pub defense: Option<usize>,
    pub template: Option<Vec<u8>>,
}

pub fn layout(m: &dyn Mem, vec: usize) -> Option<Layout> {
    let (n, data) = header(m, vec)?;
    let mut l = Layout {
        count: n,
        offense: None,
        defense: None,
        template: None,
    };
    for i in 0..n {
        let row = data + i * ROW_SIZE;
        let kind = m.read_i32(row + ROW_KIND)?;
        if kind == KIND_HEADER {
            match key(m, row).as_deref() {
                Some(k) if k == HEADER_OFFENSE.as_bytes() => l.offense = Some(i),
                Some(k) if k == HEADER_DEFENSE.as_bytes() => l.defense = Some(i),
                _ => {}
            }
        } else if kind == KIND_STAT && l.template.is_none() {
            l.template = Some(m.read_bytes(row, ROW_SIZE)?);
        }
    }
    Some(l)
}

/// A new stat row: `template`'s plain data, kind 0, `key` and `value`.
pub fn make_row(template: &[u8], key: GString, value: GString) -> [u8; ROW_SIZE] {
    let mut r = [0u8; ROW_SIZE];
    r.copy_from_slice(&template[..ROW_SIZE]);
    r[ROW_KIND..ROW_KIND + 4].copy_from_slice(&KIND_STAT.to_le_bytes());
    for (at, s) in [(ROW_KEY, key), (ROW_VALUE, value)] {
        r[at..at + 8].copy_from_slice(&s.buf.to_le_bytes());
        r[at + 8..at + 16].copy_from_slice(&s.data.to_le_bytes());
        r[at + 16..at + 20].copy_from_slice(&s.len.to_le_bytes());
        r[at + 20..at + 24].copy_from_slice(&0u32.to_le_bytes());
    }
    r
}

/// Where each section's rows go, as positions in the old rows: before the
/// Offense header (end of Hero), before the Defense header (end of
/// Offense), at the end (Defense). A missing header puts that section's
/// rows at the end.
fn insert_points(l: &Layout) -> [usize; 3] {
    let end = l.count;
    let def = l.defense.unwrap_or(end);
    let off = l.offense.filter(|&o| o <= def).unwrap_or(def);
    [off, def, end]
}

/// Moves the `added` rows appended after the old `l.count` rows (in
/// section order: Hero, Offense, Defense; `counts` per section) to their
/// sections. `data` is the vector's data pointer after the pushes.
pub fn place(m: &dyn Mem, data: usize, l: &Layout, counts: [usize; 3]) -> Option<()> {
    let added: usize = counts.iter().sum();
    let total = l.count + added;
    let all = m.read_bytes(data, total * ROW_SIZE)?;
    let row = |i: usize| &all[i * ROW_SIZE..(i + 1) * ROW_SIZE];
    let points = insert_points(l);
    let mut out = Vec::with_capacity(all.len());
    let mut next_old = 0;
    let mut next_new = l.count;
    for (s, &at) in points.iter().enumerate() {
        while next_old < at {
            out.extend_from_slice(row(next_old));
            next_old += 1;
        }
        for _ in 0..counts[s] {
            out.extend_from_slice(row(next_new));
            next_new += 1;
        }
    }
    while next_old < l.count {
        out.extend_from_slice(row(next_old));
        next_old += 1;
    }
    debug_assert_eq!(out.len(), all.len());
    m.write_bytes(data, &out);
    Some(())
}

/// Index of a section in [`place`]'s `counts`.
pub fn section_index(s: Section) -> usize {
    match s {
        Section::Hero => 0,
        Section::Offense => 1,
        Section::Defense => 2,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::cell::RefCell;

    /// Fake memory: one byte arena addressed from `base`.
    pub struct Fake {
        pub base: usize,
        pub mem: RefCell<Vec<u8>>,
    }

    impl Fake {
        pub fn new(size: usize) -> Fake {
            Fake {
                base: 0x10000,
                mem: RefCell::new(vec![0; size]),
            }
        }
        fn off(&self, at: usize) -> usize {
            at - self.base
        }
        pub fn put(&self, at: usize, b: &[u8]) {
            let o = self.off(at);
            self.mem.borrow_mut()[o..o + b.len()].copy_from_slice(b);
        }
    }

    impl Mem for Fake {
        fn read_i32(&self, at: usize) -> Option<i32> {
            let o = self.off(at);
            Some(i32::from_le_bytes(
                self.mem.borrow()[o..o + 4].try_into().ok()?,
            ))
        }
        fn read_usize(&self, at: usize) -> Option<usize> {
            let o = self.off(at);
            Some(usize::from_le_bytes(
                self.mem.borrow()[o..o + 8].try_into().ok()?,
            ))
        }
        fn read_bytes(&self, at: usize, len: usize) -> Option<Vec<u8>> {
            let o = self.off(at);
            Some(self.mem.borrow().get(o..o + len)?.to_vec())
        }
        fn write_bytes(&self, at: usize, bytes: &[u8]) {
            self.put(at, bytes);
        }
    }

    /// A fake vector at `vec` with rows (kind, key) at `data`; the keys'
    /// bytes at `strings`.
    pub fn build(f: &Fake, vec: usize, data: usize, strings: usize, rows: &[(i32, &str)]) {
        let mut s = strings;
        for (i, (kind, k)) in rows.iter().enumerate() {
            let r = data + i * ROW_SIZE;
            f.put(s, k.as_bytes());
            f.put(r + ROW_KIND, &kind.to_le_bytes());
            f.put(r + ROW_KEY + 8, &s.to_le_bytes());
            f.put(r + ROW_KEY + 16, &(k.len() as i32).to_le_bytes());
            // A marker in the plain data: the row's index.
            f.put(r + 0x60, &(i as i32 + 1000).to_le_bytes());
            s += k.len() + 1;
        }
        f.put(vec + VEC_COUNT, &(rows.len() as i32).to_le_bytes());
        f.put(vec + VEC_CAPACITY, &64i32.to_le_bytes());
        f.put(vec + VEC_DATA, &data.to_le_bytes());
    }

    pub fn page_rows() -> Vec<(i32, &'static str)> {
        vec![
            (1, "stats_section_status_effects"),
            (2, "stats_status_effect_none"),
            (1, "stats_section_hero"),
            (0, "stats_class"),
            (0, "stats_wis"),
            (1, HEADER_OFFENSE),
            (0, "stats_total_damage"),
            (1, HEADER_DEFENSE),
            (0, "stats_armor"),
            (0, "stats_poison_resist"),
        ]
    }

    fn marker(f: &Fake, data: usize, i: usize) -> i32 {
        f.read_i32(data + i * ROW_SIZE + 0x60).unwrap()
    }

    #[test]
    fn finds_headers_and_template() {
        let f = Fake::new(0x10000);
        let (vec, data, strings) = (0x10000, 0x10100, 0x18000);
        build(&f, vec, data, strings, &page_rows());
        let l = layout(&f, vec).unwrap();
        assert_eq!((l.count, l.offense, l.defense), (10, Some(5), Some(7)));
        let t = l.template.unwrap();
        assert_eq!(i32::from_le_bytes(t[0x60..0x64].try_into().unwrap()), 1003);
        assert_eq!(insert_points(&layout(&f, vec).unwrap()), [5, 7, 10]);
    }

    #[test]
    fn places_rows_at_section_ends() {
        let f = Fake::new(0x10000);
        let (vec, data, strings) = (0x10000, 0x10100, 0x18000);
        let mut rows = page_rows();
        // Appended: 2 Hero, 1 Offense, 2 Defense.
        rows.extend([(0, "h1"), (0, "h2"), (0, "o1"), (0, "d1"), (0, "d2")]);
        build(&f, vec, data, strings, &rows);
        let l = Layout {
            count: 10,
            offense: Some(5),
            defense: Some(7),
            template: None,
        };
        place(&f, data, &l, [2, 1, 2]).unwrap();
        let order: Vec<i32> = (0..15).map(|i| marker(&f, data, i) - 1000).collect();
        assert_eq!(order, [0, 1, 2, 3, 4, 10, 11, 5, 6, 12, 7, 8, 9, 13, 14]);
    }

    #[test]
    fn missing_headers_append() {
        let l = Layout {
            count: 4,
            offense: None,
            defense: None,
            template: None,
        };
        assert_eq!(insert_points(&l), [4, 4, 4]);
        let l = Layout {
            count: 4,
            offense: Some(3),
            defense: Some(1),
            template: None,
        };
        assert_eq!(
            insert_points(&l),
            [1, 1, 4],
            "offense after defense: ignored"
        );
    }

    #[test]
    fn rows_are_built_from_the_template() {
        let mut t = vec![0u8; ROW_SIZE];
        t[0x38] = 7;
        t[0] = 9;
        let k = GString::literal("uk_qol_hs_x");
        let v = GString {
            buf: 0x1234,
            data: 0x5678,
            len: 3,
            pad: 0,
        };
        let r = make_row(&t, k, v);
        assert_eq!(r[0], 0);
        assert_eq!(r[0x38], 7);
        assert_eq!(
            usize::from_le_bytes(r[0x20..0x28].try_into().unwrap()),
            0x1234
        );
        assert_eq!(i32::from_le_bytes(r[0x18..0x1c].try_into().unwrap()), 11);
        assert_eq!(i32::from_le_bytes(r[0x30..0x34].try_into().unwrap()), 3);
    }

    #[test]
    fn rejects_a_broken_vector() {
        let f = Fake::new(0x1000);
        f.put(0x10000, &(-1i32).to_le_bytes());
        assert!(header(&f, 0x10000).is_none());
        f.put(0x10000, &5i32.to_le_bytes());
        f.put(0x10004, &4i32.to_le_bytes());
        assert!(header(&f, 0x10000).is_none(), "count over capacity");
    }
}
