//! Declarative data patches for DfT documents (PLAN §3): path-based
//! set / add / remove on records, written as TOML.
//!
//! ```toml
//! # Which document the patch applies to: the game's own name for it, as in
//! # `def/def.txt`'s include list (`base/def_def_skill.txt.rfile` is
//! # `def/def_skill.txt`).
//! file = "def/def_skill.txt"
//!
//! [[op]]
//! set = "hud_skill_slot_1.pos[0]"   # change an existing value
//! value = 120
//!
//! [[op]]
//! add = "my_record"                 # insert a new entry (fails if it exists)
//! value = { parent = "engine_item", size = [1, 2] }
//!
//! [[op]]
//! add = "my_record.size[]"          # append to a list
//! value = 3
//!
//! [[op]]
//! remove = "old_record.unused"
//! ```
//!
//! Paths: names separated by `.`, list positions in brackets (`a.b[2].c`).
//! A name with `.`, `[`, `]`, `"` or spaces is written in double quotes
//! (`"odd.name".x`, `\"` and `\\` escape inside). `[]` (append) is only
//! allowed as the last step of an `add`.
//!
//! Values map from TOML: integer → int (must fit i32), float → float (f32),
//! bool → int 0/1, string → string (added to the string table if new),
//! array → list (a table keyed 0, 1, …), table → table of named entries
//! (in key order). `set` keeps the type of the existing value: an int target
//! takes an integer or bool, a float target takes a float or integer, a
//! string target a string, a table target a table or array (replacing it
//! whole).
//!
//! A patch applies all of its ops or none: on an error the document is left
//! unchanged. Errors name the op, the path and what went wrong.

use std::fmt;

use crate::{Doc, Entry, Key, Value};

/// One step of a [`Path`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Seg {
    /// A named entry.
    Name(String),
    /// A list position.
    Index(u32),
    /// `[]`: one past the end of a list (`add` only).
    Append,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Path(pub Vec<Seg>);

fn needs_quotes(s: &str) -> bool {
    s.is_empty()
        || s.chars()
            .any(|c| matches!(c, '.' | '[' | ']' | '"' | '\\') || c.is_whitespace())
}

impl fmt::Display for Path {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, s) in self.0.iter().enumerate() {
            match s {
                Seg::Name(n) => {
                    if i > 0 {
                        f.write_str(".")?;
                    }
                    if needs_quotes(n) {
                        f.write_str("\"")?;
                        for c in n.chars() {
                            if matches!(c, '"' | '\\') {
                                f.write_str("\\")?;
                            }
                            write!(f, "{c}")?;
                        }
                        f.write_str("\"")?;
                    } else {
                        f.write_str(n)?;
                    }
                }
                Seg::Index(n) => write!(f, "[{n}]")?,
                Seg::Append => f.write_str("[]")?,
            }
        }
        Ok(())
    }
}

impl std::str::FromStr for Path {
    type Err = String;

    fn from_str(s: &str) -> Result<Path, String> {
        let c: Vec<char> = s.chars().collect();
        let mut i = 0;
        let mut segs = Vec::new();
        // Whether a `.` is needed before the next name.
        let mut after_seg = false;
        while i < c.len() {
            match c[i] {
                '[' => {
                    let end = c[i..]
                        .iter()
                        .position(|&x| x == ']')
                        .ok_or("unclosed `[`")?
                        + i;
                    let inner: String = c[i + 1..end].iter().collect();
                    segs.push(if inner.is_empty() {
                        Seg::Append
                    } else if inner.bytes().all(|b| b.is_ascii_digit()) {
                        Seg::Index(
                            inner
                                .parse()
                                .map_err(|_| format!("index {inner} too large"))?,
                        )
                    } else {
                        return Err(format!("bad list index `[{inner}]`"));
                    });
                    i = end + 1;
                    after_seg = true;
                }
                '.' if after_seg => {
                    i += 1;
                    after_seg = false;
                    if i == c.len() {
                        return Err("path ends with `.`".into());
                    }
                    if c[i] == '.' || c[i] == '[' {
                        return Err(format!("empty name at position {i}"));
                    }
                }
                _ if after_seg => {
                    return Err(format!("expected `.` or `[` at position {i}"));
                }
                '"' => {
                    let mut name = String::new();
                    i += 1;
                    loop {
                        match c.get(i) {
                            None => return Err("unclosed `\"`".into()),
                            Some('"') => break,
                            Some('\\') => {
                                name.push(*c.get(i + 1).ok_or("unclosed `\"`")?);
                                i += 2;
                            }
                            Some(&ch) => {
                                name.push(ch);
                                i += 1;
                            }
                        }
                    }
                    i += 1;
                    segs.push(Seg::Name(name));
                    after_seg = true;
                }
                ']' => return Err(format!("unexpected `]` at position {i}")),
                '.' => return Err(format!("empty name at position {i}")),
                _ => {
                    let start = i;
                    while i < c.len() && !matches!(c[i], '.' | '[' | ']' | '"') {
                        i += 1;
                    }
                    let name: String = c[start..i].iter().collect();
                    if name.chars().any(char::is_whitespace) {
                        return Err(format!("name `{name}` has spaces; quote it"));
                    }
                    segs.push(Seg::Name(name));
                    after_seg = true;
                }
            }
        }
        if segs.is_empty() {
            return Err("empty path".into());
        }
        if segs[..segs.len() - 1].contains(&Seg::Append) {
            return Err("`[]` can only be the last step".into());
        }
        Ok(Path(segs))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Op {
    Set { path: Path, value: toml::Value },
    Add { path: Path, value: toml::Value },
    Remove { path: Path },
}

impl Op {
    pub fn path(&self) -> &Path {
        match self {
            Op::Set { path, .. } | Op::Add { path, .. } | Op::Remove { path } => path,
        }
    }

    fn verb(&self) -> &'static str {
        match self {
            Op::Set { .. } => "set",
            Op::Add { .. } => "add",
            Op::Remove { .. } => "remove",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    /// The patch file itself is malformed.
    Parse(String),
    /// `at` (a path prefix) has no entry for the next step.
    NotFound { at: String },
    /// `at` is a value, not a table, so the path can't go further.
    NotATable { at: String },
    /// A list step (`[n]`, `[]`) on a table that isn't a list (keys 0..n).
    NotAList { at: String },
    /// `add` of an entry that already exists.
    Exists,
    /// `add` at a list position past the end.
    OutOfRange { len: usize },
    TypeMismatch {
        expected: &'static str,
        found: &'static str,
    },
    /// A TOML value that can't be stored (datetime, int outside i32, …).
    BadValue(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PatchError {
    /// 1-based op number; 0 for errors in the file as a whole.
    pub op: usize,
    /// `set a.b`, empty for file-level errors.
    pub what: String,
    pub kind: ErrorKind,
}

impl fmt::Display for PatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.op > 0 {
            write!(f, "op {} ({}): ", self.op, self.what)?;
        }
        match &self.kind {
            ErrorKind::Parse(m) => write!(f, "{m}"),
            ErrorKind::NotFound { at } => write!(f, "path not found: no `{at}`"),
            ErrorKind::NotATable { at } => write!(f, "`{at}` is a value, not a table"),
            ErrorKind::NotAList { at } => write!(f, "`{at}` is not a list"),
            ErrorKind::Exists => write!(f, "entry already exists"),
            ErrorKind::OutOfRange { len } => write!(f, "list position past the end (length {len})"),
            ErrorKind::TypeMismatch { expected, found } => {
                write!(
                    f,
                    "type mismatch: the entry is {expected}, the value is {found}"
                )
            }
            ErrorKind::BadValue(m) => write!(f, "bad value: {m}"),
        }
    }
}

impl std::error::Error for PatchError {}

fn parse_err(m: impl Into<String>) -> PatchError {
    PatchError {
        op: 0,
        what: String::new(),
        kind: ErrorKind::Parse(m.into()),
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Patch {
    /// The document this patch is for (the game's name for it).
    pub file: String,
    pub ops: Vec<Op>,
}

impl Patch {
    pub fn parse(text: &str) -> Result<Patch, PatchError> {
        let t: toml::Table = text.parse().map_err(|e| parse_err(format!("{e}")))?;
        let mut file = None;
        let mut ops = Vec::new();
        for (k, v) in &t {
            match k.as_str() {
                "file" => {
                    file = Some(
                        v.as_str()
                            .ok_or_else(|| parse_err("`file` must be a string"))?
                            .to_owned(),
                    )
                }
                "op" => {
                    let arr = v
                        .as_array()
                        .ok_or_else(|| parse_err("`op` must be an array of tables ([[op]])"))?;
                    for (i, o) in arr.iter().enumerate() {
                        let o = o
                            .as_table()
                            .ok_or_else(|| parse_err(format!("op {}: not a table", i + 1)))?;
                        ops.push(parse_op(o).map_err(|m| PatchError {
                            op: i + 1,
                            what: String::new(),
                            kind: ErrorKind::Parse(m),
                        })?);
                    }
                }
                _ => return Err(parse_err(format!("unknown key `{k}`"))),
            }
        }
        Ok(Patch {
            file: file.ok_or_else(|| parse_err("missing `file`"))?,
            ops,
        })
    }

    /// Applies every op in order. On error `doc` is unchanged.
    pub fn apply(&self, doc: &mut Doc) -> Result<(), PatchError> {
        let mut work = doc.clone();
        for (i, op) in self.ops.iter().enumerate() {
            apply_op(&mut work, op).map_err(|kind| PatchError {
                op: i + 1,
                what: format!("{} {}", op.verb(), op.path()),
                kind,
            })?;
        }
        *doc = work;
        Ok(())
    }

    /// Parses `bytes`, applies the patch and serializes the result.
    pub fn apply_bytes(&self, bytes: &[u8]) -> Result<Vec<u8>, String> {
        let mut doc = Doc::parse(bytes).map_err(|e| e.to_string())?;
        self.apply(&mut doc).map_err(|e| e.to_string())?;
        Ok(doc.to_bytes())
    }
}

fn parse_op(t: &toml::Table) -> Result<Op, String> {
    let mut verb = None;
    let mut value = None;
    for (k, v) in t {
        match k.as_str() {
            "set" | "add" | "remove" => {
                if let Some((other, _)) = verb {
                    return Err(format!("both `{other}` and `{k}`; one op per [[op]]"));
                }
                let p = v
                    .as_str()
                    .ok_or_else(|| format!("`{k}` must be a path string"))?;
                let path: Path = p.parse().map_err(|e| format!("bad path `{p}`: {e}"))?;
                verb = Some((k.as_str(), path));
            }
            "value" => value = Some(v.clone()),
            _ => return Err(format!("unknown key `{k}`")),
        }
    }
    let (verb, path) = verb.ok_or("needs one of `set`, `add`, `remove`")?;
    if path.0.contains(&Seg::Append) && verb != "add" {
        return Err("`[]` (append) is only valid in `add`".into());
    }
    match (verb, value) {
        ("set", Some(value)) => Ok(Op::Set { path, value }),
        ("add", Some(value)) => Ok(Op::Add { path, value }),
        ("remove", None) => Ok(Op::Remove { path }),
        ("remove", Some(_)) => Err("`remove` takes no `value`".into()),
        (_, None) => Err(format!("`{verb}` needs a `value`")),
        _ => unreachable!(),
    }
}

fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Int(_) => "an int",
        Value::Float(_) => "a float",
        Value::Str(_) => "a string",
        Value::Table(_) => "a table",
    }
}

fn toml_type_name(v: &toml::Value) -> &'static str {
    match v {
        toml::Value::String(_) => "a string",
        toml::Value::Integer(_) => "an integer",
        toml::Value::Float(_) => "a float",
        toml::Value::Boolean(_) => "a bool",
        toml::Value::Datetime(_) => "a datetime",
        toml::Value::Array(_) => "an array",
        toml::Value::Table(_) => "a table",
    }
}

/// The string table, borrowed apart from the root so both can be changed.
struct Strings<'a> {
    strings: &'a mut Vec<Vec<u8>>,
    widths: &'a mut Vec<u8>,
}

impl Strings<'_> {
    fn intern(&mut self, s: &str) -> u32 {
        if let Some(i) = self.strings.iter().position(|b| b == s.as_bytes()) {
            return i as u32;
        }
        self.strings.push(s.as_bytes().to_vec());
        self.widths.push(1);
        (self.strings.len() - 1) as u32
    }

    fn is(&self, idx: u32, s: &str) -> bool {
        self.strings
            .get(idx as usize)
            .is_some_and(|b| b == s.as_bytes())
    }
}

fn int(i: i64) -> Result<Value, ErrorKind> {
    i32::try_from(i)
        .map(Value::Int)
        .map_err(|_| ErrorKind::BadValue(format!("{i} doesn't fit in a 32-bit int")))
}

/// Converts a TOML value with no existing entry to match.
fn new_value(v: &toml::Value, s: &mut Strings) -> Result<Value, ErrorKind> {
    Ok(match v {
        toml::Value::Integer(i) => int(*i)?,
        toml::Value::Float(f) => Value::Float((*f as f32).to_bits()),
        toml::Value::Boolean(b) => Value::Int(*b as i32),
        toml::Value::String(st) => Value::Str(s.intern(st)),
        toml::Value::Array(a) => Value::Table(
            a.iter()
                .enumerate()
                .map(|(i, v)| Ok(Entry::new(Key::Index(i as u32), new_value(v, s)?)))
                .collect::<Result<_, _>>()?,
        ),
        toml::Value::Table(t) => Value::Table(
            t.iter()
                .map(|(k, v)| Ok(Entry::new(Key::Name(s.intern(k)), new_value(v, s)?)))
                .collect::<Result<_, _>>()?,
        ),
        toml::Value::Datetime(d) => return Err(ErrorKind::BadValue(format!("datetime {d}"))),
    })
}

/// Converts a TOML value to replace `old`, keeping its type. A value equal
/// to the old one keeps the old encoding (string index, float bits).
fn set_value(old: &Value, v: &toml::Value, s: &mut Strings) -> Result<Value, ErrorKind> {
    let mismatch = || ErrorKind::TypeMismatch {
        expected: type_name(old),
        found: toml_type_name(v),
    };
    Ok(match (old, v) {
        (Value::Int(_), toml::Value::Integer(i)) => int(*i)?,
        (Value::Int(_), toml::Value::Boolean(b)) => Value::Int(*b as i32),
        (Value::Float(bits), toml::Value::Float(_) | toml::Value::Integer(_)) => {
            let f = match v {
                toml::Value::Float(f) => *f as f32,
                toml::Value::Integer(i) => *i as f32,
                _ => unreachable!(),
            };
            if f32::from_bits(*bits) == f && !f.is_nan() {
                Value::Float(*bits)
            } else {
                Value::Float(f.to_bits())
            }
        }
        (Value::Str(idx), toml::Value::String(st)) => {
            if s.is(*idx, st) {
                Value::Str(*idx)
            } else {
                Value::Str(s.intern(st))
            }
        }
        (Value::Table(_), toml::Value::Table(_) | toml::Value::Array(_)) => new_value(v, s)?,
        _ => return Err(mismatch()),
    })
}

fn is_list(t: &[Entry]) -> bool {
    t.iter()
        .enumerate()
        .all(|(i, e)| e.key == Key::Index(i as u32))
}

fn find(t: &[Entry], seg: &Seg, s: &Strings) -> Option<usize> {
    t.iter().position(|e| match (seg, e.key) {
        (Seg::Name(n), Key::Name(k)) => s.is(k, n),
        (Seg::Index(n), Key::Index(k)) => *n == k,
        _ => false,
    })
}

/// The table that `segs` leads to.
fn walk<'t>(
    root: &'t mut Vec<Entry>,
    segs: &[Seg],
    s: &Strings,
) -> Result<&'t mut Vec<Entry>, ErrorKind> {
    let mut t = root;
    for (n, seg) in segs.iter().enumerate() {
        let at = || Path(segs[..=n].to_vec()).to_string();
        if matches!(seg, Seg::Index(_)) && !is_list(t) {
            return Err(ErrorKind::NotAList {
                at: Path(segs[..n].to_vec()).to_string(),
            });
        }
        let i = find(t, seg, s).ok_or_else(|| ErrorKind::NotFound { at: at() })?;
        t = match &mut t[i].value {
            Value::Table(child) => child,
            _ => return Err(ErrorKind::NotATable { at: at() }),
        };
    }
    Ok(t)
}

fn apply_op(doc: &mut Doc, op: &Op) -> Result<(), ErrorKind> {
    let Doc {
        strings,
        len_widths,
        root,
        ..
    } = doc;
    let mut s = Strings {
        strings,
        widths: len_widths,
    };
    let segs = &op.path().0;
    let (last, parent_segs) = segs.split_last().expect("paths are never empty");
    let parent = walk(root, parent_segs, &s)?;
    let parent_path = || Path(parent_segs.to_vec()).to_string();
    if matches!(last, Seg::Index(_) | Seg::Append) && !is_list(parent) {
        return Err(ErrorKind::NotAList { at: parent_path() });
    }
    let found = find(parent, last, &s);
    let not_found = || ErrorKind::NotFound {
        at: op.path().to_string(),
    };
    match op {
        Op::Set { value, .. } => {
            let i = found.ok_or_else(not_found)?;
            let v = set_value(&parent[i].value, value, &mut s)?;
            parent[i].value = v;
        }
        Op::Remove { .. } => {
            let i = found.ok_or_else(not_found)?;
            parent.remove(i);
            if let Seg::Index(_) = last {
                renumber(parent);
            }
        }
        Op::Add { value, .. } => {
            let v = new_value(value, &mut s)?;
            match last {
                Seg::Name(n) => {
                    if found.is_some() {
                        return Err(ErrorKind::Exists);
                    }
                    let k = s.intern(n);
                    parent.push(Entry::new(Key::Name(k), v));
                }
                Seg::Index(n) => {
                    let n = *n as usize;
                    if n > parent.len() {
                        return Err(ErrorKind::OutOfRange { len: parent.len() });
                    }
                    parent.insert(n, Entry::new(Key::Index(n as u32), v));
                    renumber(parent);
                }
                Seg::Append => {
                    let n = parent.len() as u32;
                    parent.push(Entry::new(Key::Index(n), v));
                }
            }
        }
    }
    Ok(())
}

/// Renumbers a list's keys to 0..n after an insert or removal. Entries whose
/// index doesn't change keep their encoding.
fn renumber(list: &mut [Entry]) {
    for (i, e) in list.iter_mut().enumerate() {
        e.key = Key::Index(i as u32);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Path {
        s.parse().unwrap()
    }

    #[test]
    fn paths() {
        assert_eq!(
            p("a.b[2].c").0,
            vec![
                Seg::Name("a".into()),
                Seg::Name("b".into()),
                Seg::Index(2),
                Seg::Name("c".into())
            ]
        );
        assert_eq!(p("[0][1]").0, vec![Seg::Index(0), Seg::Index(1)]);
        assert_eq!(p("x[]").0, vec![Seg::Name("x".into()), Seg::Append]);
        assert_eq!(
            p(r#""a.b"."c\"d".e"#).0,
            vec![
                Seg::Name("a.b".into()),
                Seg::Name("c\"d".into()),
                Seg::Name("e".into())
            ]
        );
        for s in [
            "a.b[2].c",
            "[0][1]",
            "x[]",
            r#""a.b"."c\"d".e"#,
            r#""""#,
            "\"a b\"",
        ] {
            assert_eq!(p(s).to_string(), s);
        }
        for bad in [
            "", "a..b", "a.", ".a", "a[", "a[x]", "a]", "a[]b", "a[].b", "a b", "\"a", "a\"b\"",
            "a.[0]",
        ] {
            assert!(bad.parse::<Path>().is_err(), "{bad:?} should not parse");
        }
    }

    /// strings: hud, x, y, name, sword, list
    /// root: hud = { x = 10, y = 2.5, name = "sword" }, list = [1, 2, 3]
    fn doc() -> Doc {
        let strings: Vec<Vec<u8>> = ["hud", "x", "y", "name", "sword", "list"]
            .iter()
            .map(|s| s.as_bytes().to_vec())
            .collect();
        let n = |i| Key::Name(i);
        let hud = vec![
            Entry::new(n(1), Value::Int(10)),
            Entry::new(n(2), Value::Float(2.5f32.to_bits())),
            Entry::new(n(3), Value::Str(4)),
        ];
        let list = (0..3)
            .map(|i| Entry::new(Key::Index(i), Value::Int(i as i32 + 1)))
            .collect();
        Doc {
            version: 1,
            len_widths: vec![1; strings.len()],
            strings,
            root: vec![
                Entry::new(n(0), Value::Table(hud)),
                Entry::new(n(5), Value::Table(list)),
            ],
        }
    }

    fn patch(ops: &str) -> Patch {
        Patch::parse(&format!("file = \"def/test.txt\"\n{ops}")).unwrap()
    }

    fn apply(ops: &str) -> Result<Doc, PatchError> {
        let mut d = doc();
        patch(ops).apply(&mut d)?;
        // Always serializes to something that parses back the same.
        assert_eq!(Doc::parse(&d.to_bytes()).unwrap(), d);
        Ok(d)
    }

    fn get<'d>(d: &'d Doc, path: &str) -> &'d Value {
        let path = p(path);
        let mut t = &d.root;
        let mut v = None;
        for seg in &path.0 {
            let e = t
                .iter()
                .find(|e| match (seg, e.key) {
                    (Seg::Name(n), Key::Name(k)) => d.str(k) == *n,
                    (Seg::Index(n), Key::Index(k)) => *n == k,
                    _ => false,
                })
                .unwrap_or_else(|| panic!("{path} not found"));
            v = Some(&e.value);
            if let Value::Table(c) = &e.value {
                t = c;
            }
        }
        v.unwrap()
    }

    fn list(d: &Doc, path: &str) -> Vec<Value> {
        let Value::Table(t) = get(d, path) else {
            panic!("not a table")
        };
        assert!(is_list(t));
        t.iter().map(|e| e.value.clone()).collect()
    }

    #[test]
    fn parse_patch() {
        let pt = patch(
            "[[op]]\nset = \"hud.x\"\nvalue = 3\n\
             [[op]]\nadd = \"list[]\"\nvalue = 4\n\
             [[op]]\nremove = \"hud.y\"\n",
        );
        assert_eq!(pt.file, "def/test.txt");
        assert_eq!(pt.ops.len(), 3);
        assert_eq!(pt.ops[2], Op::Remove { path: p("hud.y") });

        let bad = |text: &str| Patch::parse(text).unwrap_err().to_string();
        assert!(bad("[[op]]\nremove = \"a\"").contains("missing `file`"));
        assert!(bad("file = \"f\"\nops = []").contains("unknown key `ops`"));
        let e = bad("file = \"f\"\n[[op]]\nremove = \"a\"\n[[op]]\nset = \"a\"");
        assert!(
            e.starts_with("op 2") && e.contains("needs a `value`"),
            "{e}"
        );
        assert!(bad("file = \"f\"\n[[op]]\nremove = \"a\"\nvalue = 1").contains("no `value`"));
        assert!(bad("file = \"f\"\n[[op]]\nset = \"a\"\nadd = \"b\"\nvalue = 1").contains("both"));
        assert!(
            bad("file = \"f\"\n[[op]]\nset = \"a[]\"\nvalue = 1").contains("only valid in `add`")
        );
        assert!(bad("file = \"f\"\n[[op]]\nset = \"a..b\"\nvalue = 1").contains("bad path"));
        assert!(bad("file = \"f\"\n[[op]]\nvalue = 1").contains("needs one of"));
    }

    #[test]
    fn set_values() {
        let d = apply(
            "[[op]]\nset = \"hud.x\"\nvalue = -4\n\
             [[op]]\nset = \"hud.y\"\nvalue = 7\n\
             [[op]]\nset = \"hud.name\"\nvalue = \"axe\"\n\
             [[op]]\nset = \"list[1]\"\nvalue = true\n",
        )
        .unwrap();
        assert_eq!(get(&d, "hud.x"), &Value::Int(-4));
        assert_eq!(get(&d, "hud.y"), &Value::Float(7.0f32.to_bits()));
        let Value::Str(i) = get(&d, "hud.name") else {
            panic!()
        };
        assert_eq!(d.str(*i), "axe");
        assert_eq!(d.strings.len(), 7, "new string appended");
        assert_eq!(get(&d, "list[1]"), &Value::Int(1));

        // Setting a string that's already in the table reuses it.
        let d = apply("[[op]]\nset = \"hud.name\"\nvalue = \"list\"").unwrap();
        assert_eq!(get(&d, "hud.name"), &Value::Str(5));
        assert_eq!(d.strings.len(), 6);

        // Replace a whole table.
        let d = apply("[[op]]\nset = \"list\"\nvalue = [9]").unwrap();
        assert_eq!(list(&d, "list"), [Value::Int(9)]);
    }

    #[test]
    fn identity_set_is_byte_identical() {
        let before = doc().to_bytes();
        let d = apply(
            "[[op]]\nset = \"hud.x\"\nvalue = 10\n\
             [[op]]\nset = \"hud.y\"\nvalue = 2.5\n\
             [[op]]\nset = \"hud.name\"\nvalue = \"sword\"\n",
        )
        .unwrap();
        assert_eq!(d.to_bytes(), before);
    }

    #[test]
    fn add_and_remove() {
        let d = apply(
            "[[op]]\nadd = \"slot\"\nvalue = { x = 1, pos = [1.5, 2], tag = \"sword\" }\n\
             [[op]]\nadd = \"list[]\"\nvalue = 4\n\
             [[op]]\nadd = \"list[0]\"\nvalue = 0\n\
             [[op]]\nadd = \"hud.w\"\nvalue = 5\n\
             [[op]]\nremove = \"hud.y\"\n\
             [[op]]\nremove = \"list[2]\"\n",
        )
        .unwrap();
        assert_eq!(get(&d, "slot.x"), &Value::Int(1));
        assert_eq!(get(&d, "slot.pos[0]"), &Value::Float(1.5f32.to_bits()));
        assert_eq!(get(&d, "slot.pos[1]"), &Value::Int(2));
        assert_eq!(get(&d, "slot.tag"), &Value::Str(4));
        assert_eq!(get(&d, "hud.w"), &Value::Int(5));
        // [1,2,3] → append 4 → insert 0 at 0 → remove [2] (the 2).
        assert_eq!(
            list(&d, "list"),
            [0, 1, 3, 4].map(Value::Int).to_vec(),
            "indices renumbered"
        );
        let Value::Table(hud) = get(&d, "hud") else {
            panic!()
        };
        assert_eq!(hud.len(), 3);
        assert!(d.find_str("slot").is_some() && d.find_str("pos").is_some());
    }

    fn kind(ops: &str) -> ErrorKind {
        apply(ops).unwrap_err().kind
    }

    #[test]
    fn errors_are_precise() {
        assert_eq!(
            kind("[[op]]\nset = \"hud.z\"\nvalue = 1"),
            ErrorKind::NotFound { at: "hud.z".into() }
        );
        assert_eq!(
            kind("[[op]]\nset = \"nope.x.y\"\nvalue = 1"),
            ErrorKind::NotFound { at: "nope".into() }
        );
        assert_eq!(
            kind("[[op]]\nset = \"hud.x.y\"\nvalue = 1"),
            ErrorKind::NotATable { at: "hud.x".into() }
        );
        assert_eq!(
            kind("[[op]]\nset = \"hud[0]\"\nvalue = 1"),
            ErrorKind::NotAList { at: "hud".into() }
        );
        assert_eq!(
            kind("[[op]]\nset = \"hud.x\"\nvalue = 1.5"),
            ErrorKind::TypeMismatch {
                expected: "an int",
                found: "a float"
            }
        );
        assert_eq!(
            kind("[[op]]\nset = \"hud.name\"\nvalue = 1"),
            ErrorKind::TypeMismatch {
                expected: "a string",
                found: "an integer"
            }
        );
        assert_eq!(
            kind("[[op]]\nset = \"hud\"\nvalue = 1"),
            ErrorKind::TypeMismatch {
                expected: "a table",
                found: "an integer"
            }
        );
        assert!(matches!(
            kind("[[op]]\nset = \"hud.x\"\nvalue = 3000000000"),
            ErrorKind::BadValue(_)
        ));
        assert_eq!(
            kind("[[op]]\nadd = \"hud.x\"\nvalue = 1"),
            ErrorKind::Exists
        );
        assert_eq!(
            kind("[[op]]\nadd = \"list[4]\"\nvalue = 1"),
            ErrorKind::OutOfRange { len: 3 }
        );
        assert_eq!(
            kind("[[op]]\nremove = \"list[3]\""),
            ErrorKind::NotFound {
                at: "list[3]".into()
            }
        );

        // The message names the op and path, and the document is untouched.
        let mut d = doc();
        let e = patch(
            "[[op]]\nset = \"hud.x\"\nvalue = 99\n\
             [[op]]\nremove = \"hud.q\"\n",
        )
        .apply(&mut d)
        .unwrap_err();
        assert_eq!(
            e.to_string(),
            "op 2 (remove hud.q): path not found: no `hud.q`"
        );
        assert_eq!(d, doc());
    }

    /// With `UNDERKEEP_DIR` set: for every DfT document of the install, set
    /// every leaf value to itself through a patch and check the file is
    /// unchanged byte for byte. Reads the player's install; stores nothing.
    #[test]
    fn game_files_identity_patch() {
        let Some(dir) = std::env::var_os("UNDERKEEP_DIR") else {
            return;
        };
        let mut files = 0;
        let mut ops_total = 0;
        for f in std::fs::read_dir(std::path::Path::new(&dir).join("base"))
            .unwrap()
            .flatten()
        {
            let name = f.file_name().to_string_lossy().into_owned();
            if !name.ends_with(".txt.rfile") {
                continue;
            }
            let bytes = std::fs::read(f.path()).unwrap();
            let d = Doc::parse(&bytes).unwrap();
            let mut text = String::from("file = \"x\"\n");
            let mut n = 0;
            leaves(&d, &d.root, &mut Vec::new(), &mut |path, v| {
                let value = match v {
                    Value::Int(i) => i.to_string(),
                    Value::Float(b) => {
                        let f = f32::from_bits(*b);
                        if !f.is_finite() {
                            return;
                        }
                        format!("{:?}", f as f64)
                    }
                    Value::Str(s) => toml::Value::String(d.str(*s)).to_string(),
                    Value::Table(_) => return,
                };
                let path = toml::Value::String(path.to_string()).to_string();
                text.push_str(&format!("[[op]]\nset = {path}\nvalue = {value}\n"));
                n += 1;
            });
            let out = Patch::parse(&text)
                .unwrap()
                .apply_bytes(&bytes)
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(out == bytes, "{name}: identity patch changed the bytes");
            files += 1;
            ops_total += n;
        }
        eprintln!("identity-patched {files} files ({ops_total} ops)");
    }

    fn leaves(d: &Doc, t: &[Entry], path: &mut Vec<Seg>, f: &mut impl FnMut(&Path, &Value)) {
        for e in t {
            path.push(match e.key {
                Key::Name(k) => Seg::Name(d.str(k)),
                Key::Index(i) => Seg::Index(i),
            });
            match &e.value {
                Value::Table(c) => leaves(d, c, path, f),
                v => f(&Path(path.clone()), v),
            }
            path.pop();
        }
    }
}
