//! Reader for the game's asset databases (`base/_*.db`: `_sprite.db`,
//! `_texture.db`, ...), a format separate from DfT. Read-only.
//!
//! Layout (integers little-endian):
//!
//! ```text
//! magic   : FF 44 65 66            ("\xFFDef")
//! version : u8                     (1)
//! strtab  : u32                    file offset of the string table
//! (padding up to offset 0x10)
//! root    : value                  at 0x10, ending at `strtab`
//! strings : u32 count, then count x (varint len, bytes)
//!
//! value   : u8 tag, then
//!           1 = u8, 2 = i32, 4 = f32,
//!           5 = string (u8 index), 6 = string (varint index, may be padded),
//!           7 = array (u32 count, values),
//!           8 = map (u32 count, key value pairs)
//! ```
//!
//! Tags 1, 2 and 4 to 8 cover every value of `_sprite.db`, `_texture.db`
//! and most other databases of game 1.0.5; `_font.db` and `_sound.db` use
//! others and aren't readable here.
//!
//! A record of `_sprite.db` is a map with `name` and `metadata`, which holds
//! `texture` (a `_texture.db` record name), `rect` (`[x, y, w, h]` in the
//! texture's pixels, y down) and `pivot`. A `_texture.db` record's
//! `metadata.file` is the image file in `base/`.

use crate::{Error, Result};

pub const MAGIC: [u8; 4] = [0xFF, b'D', b'e', b'f'];
const ROOT: usize = 0x10;

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    U8(u8),
    I32(i32),
    F32(f32),
    Str(String),
    Array(Vec<Value>),
    Map(Vec<(Value, Value)>),
}

impl Value {
    /// The value under string key `key`, for a map.
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Map(m) => m
                .iter()
                .find(|(k, _)| matches!(k, Value::Str(s) if s == key))
                .map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    /// A number as f32 (from any numeric tag).
    pub fn as_f32(&self) -> Option<f32> {
        match *self {
            Value::U8(v) => Some(f32::from(v)),
            Value::I32(v) => Some(v as f32),
            Value::F32(v) => Some(v),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Value]> {
        match self {
            Value::Array(a) => Some(a),
            _ => None,
        }
    }
}

fn err<T>(msg: impl Into<String>) -> Result<T> {
    Err(Error(msg.into()))
}

struct Reader<'a> {
    d: &'a [u8],
    p: usize,
    end: usize,
}

impl<'a> Reader<'a> {
    fn bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        if n > self.end.saturating_sub(self.p) {
            return err(format!("unexpected end of data at offset {}", self.p));
        }
        let s = &self.d[self.p..self.p + n];
        self.p += n;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.bytes(1)?[0])
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.bytes(4)?.try_into().unwrap()))
    }
    fn varint(&mut self) -> Result<u32> {
        let mut r: u64 = 0;
        for shift in (0..35).step_by(7) {
            let b = self.u8()?;
            r |= u64::from(b & 0x7F) << shift;
            if b < 0x80 {
                return u32::try_from(r).or_else(|_| err("varint too large"));
            }
        }
        err(format!("varint too long at offset {}", self.p))
    }
    /// A count that can't exceed the bytes left (each element takes one).
    fn count(&mut self) -> Result<usize> {
        let n = self.u32()? as usize;
        if n > self.end - self.p {
            return err(format!("bad count {n} at offset {}", self.p - 4));
        }
        Ok(n)
    }
}

fn string(strings: &[String], i: u32) -> Result<Value> {
    match strings.get(i as usize) {
        Some(s) => Ok(Value::Str(s.clone())),
        None => err(format!("string index {i} out of range")),
    }
}

fn value(r: &mut Reader, strings: &[String], depth: u32) -> Result<Value> {
    if depth > 64 {
        return err("nested too deeply");
    }
    let at = r.p;
    Ok(match r.u8()? {
        1 => Value::U8(r.u8()?),
        2 => Value::I32(r.u32()? as i32),
        4 => Value::F32(f32::from_bits(r.u32()?)),
        5 => string(strings, u32::from(r.u8()?))?,
        6 => string(strings, r.varint()?)?,
        7 => {
            let n = r.count()?;
            let mut a = Vec::with_capacity(n);
            for _ in 0..n {
                a.push(value(r, strings, depth + 1)?);
            }
            Value::Array(a)
        }
        8 => {
            let n = r.count()?;
            let mut m = Vec::with_capacity(n);
            for _ in 0..n {
                let k = value(r, strings, depth + 1)?;
                let v = value(r, strings, depth + 1)?;
                m.push((k, v));
            }
            Value::Map(m)
        }
        t => return err(format!("unknown value tag {t} at offset {at}")),
    })
}

/// Parses an asset database into its root value.
pub fn parse(d: &[u8]) -> Result<Value> {
    if d.len() < ROOT || d[..4] != MAGIC {
        return err("not an Underkeep asset database (bad magic)");
    }
    if d[4] != 1 {
        return err(format!("unknown asset database version {}", d[4]));
    }
    let strtab = u32::from_le_bytes(d[5..9].try_into().unwrap()) as usize;
    if strtab < ROOT || strtab > d.len() {
        return err("bad string table offset");
    }
    let mut r = Reader {
        d,
        p: strtab,
        end: d.len(),
    };
    let n = r.count()?;
    let mut strings = Vec::with_capacity(n);
    for _ in 0..n {
        let len = r.varint()? as usize;
        strings.push(String::from_utf8_lossy(r.bytes(len)?).into_owned());
    }
    let mut r = Reader {
        d,
        p: ROOT,
        end: strtab,
    };
    let root = value(&mut r, &strings, 0)?;
    if r.p != strtab {
        return err(format!("{} bytes left after the data", strtab - r.p));
    }
    Ok(root)
}

/// The record named `name` in a database whose root is an array of maps
/// with a `name` key (`_sprite.db`, `_texture.db`).
pub fn record<'a>(root: &'a Value, name: &str) -> Option<&'a Value> {
    root.as_array()?
        .iter()
        .find(|r| r.get("name").and_then(Value::as_str) == Some(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a database from a root value encoder and a string table.
    fn db(root: &[u8], strings: &[&str]) -> Vec<u8> {
        let mut d = MAGIC.to_vec();
        d.push(1);
        d.extend_from_slice(&((ROOT + root.len()) as u32).to_le_bytes());
        d.resize(ROOT, 0);
        d.extend_from_slice(root);
        d.extend_from_slice(&(strings.len() as u32).to_le_bytes());
        for s in strings {
            d.push(s.len() as u8);
            d.extend_from_slice(s.as_bytes());
        }
        d.extend_from_slice(&[0; 4]);
        d
    }

    #[test]
    fn sprite_like_record() {
        let mut root = vec![7, 1, 0, 0, 0, 8, 2, 0, 0, 0];
        root.extend([5, 0, 5, 1]); // name = "icon"
        root.extend([5, 2, 8, 2, 0, 0, 0]); // metadata = {
        root.extend([6, 0x83, 0x00, 5, 4]); //   texture (padded varint) = "atlas"
        root.extend([5, 5, 7, 2, 0, 0, 0, 4]); //   rect = [1.5, 7]
        root.extend(1.5f32.to_le_bytes());
        root.extend([2]);
        root.extend(7i32.to_le_bytes()); // }
        let d = db(
            &root,
            &["name", "icon", "metadata", "texture", "atlas", "rect"],
        );
        let v = parse(&d).unwrap();
        let r = record(&v, "icon").unwrap();
        let m = r.get("metadata").unwrap();
        assert_eq!(m.get("texture").and_then(Value::as_str), Some("atlas"));
        let rect: Vec<f32> = m
            .get("rect")
            .and_then(Value::as_array)
            .unwrap()
            .iter()
            .filter_map(Value::as_f32)
            .collect();
        assert_eq!(rect, [1.5, 7.0]);
        assert!(record(&v, "other").is_none());
    }

    #[test]
    fn rejects_bad_input() {
        assert!(parse(b"\xFFDfT\x01\x00\x00\x00").is_err());
        // Unknown tag, string out of range, data not ending at the table.
        assert!(parse(&db(&[3, 0, 0, 0, 0], &[])).is_err());
        assert!(parse(&db(&[5, 9], &["a"])).is_err());
        assert!(parse(&db(&[1, 0, 1], &[])).is_err());
        // Huge counts don't allocate.
        assert!(parse(&db(&[7, 0xff, 0xff, 0xff, 0x7f], &[])).is_err());
        assert_eq!(parse(&db(&[1, 9], &[])).unwrap(), Value::U8(9));
    }

    /// With `UNDERKEEP_DIR` set: the databases the runtime reads parse to
    /// their end and have the records it looks up.
    #[test]
    fn installed_databases() {
        let Some(dir) = std::env::var_os("UNDERKEEP_DIR") else {
            return;
        };
        let base = std::path::PathBuf::from(dir).join("base");
        for f in ["_sprite.db", "_texture.db"] {
            let d = std::fs::read(base.join(f)).unwrap();
            let v = parse(&d).unwrap_or_else(|e| panic!("{f}: {e}"));
            assert!(v.as_array().is_some_and(|a| !a.is_empty()), "{f}");
        }
        let s = parse(&std::fs::read(base.join("_sprite.db")).unwrap()).unwrap();
        let m = record(&s, "hud_barbie_sword")
            .and_then(|r| r.get("metadata"))
            .unwrap();
        assert!(m.get("texture").and_then(Value::as_str).is_some());
    }
}
