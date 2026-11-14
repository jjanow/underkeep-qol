//! A small reader for Valve's text KeyValues files (`libraryfolders.vdf`,
//! `appmanifest_*.acf`, `localconfig.vdf`).
//!
//! It keeps the byte span of every value, so a single value can be changed by
//! splicing the original text instead of rewriting the whole file.

use std::ops::Range;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Str(String),
    Obj(Vec<Entry>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub key: String,
    pub value: Value,
    /// For a string: the quoted (or bare) token. For an object: `{` to `}`
    /// inclusive.
    pub span: Range<usize>,
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Str(String),
    Open,
    Close,
}

struct Lexer<'a> {
    s: &'a [u8],
    i: usize,
}

impl Lexer<'_> {
    fn skip_ws(&mut self) {
        loop {
            while self.i < self.s.len() && self.s[self.i].is_ascii_whitespace() {
                self.i += 1;
            }
            if self.s[self.i..].starts_with(b"//") {
                while self.i < self.s.len() && self.s[self.i] != b'\n' {
                    self.i += 1;
                }
                continue;
            }
            break;
        }
    }

    /// Next token and its span, or `None` at the end.
    fn next(&mut self) -> Result<Option<(Tok, Range<usize>)>, String> {
        loop {
            self.skip_ws();
            let start = self.i;
            let Some(&c) = self.s.get(self.i) else {
                return Ok(None);
            };
            match c {
                b'{' => {
                    self.i += 1;
                    return Ok(Some((Tok::Open, start..self.i)));
                }
                b'}' => {
                    self.i += 1;
                    return Ok(Some((Tok::Close, start..self.i)));
                }
                b'"' => {
                    self.i += 1;
                    let mut out = Vec::new();
                    loop {
                        let Some(&c) = self.s.get(self.i) else {
                            return Err(format!("unterminated string at byte {start}"));
                        };
                        self.i += 1;
                        match c {
                            b'"' => break,
                            b'\\' => {
                                let e = self.s.get(self.i).copied().unwrap_or(b'\\');
                                self.i += 1;
                                out.push(match e {
                                    b'n' => b'\n',
                                    b't' => b'\t',
                                    b'r' => b'\r',
                                    other => other,
                                });
                            }
                            other => out.push(other),
                        }
                    }
                    let s = String::from_utf8_lossy(&out).into_owned();
                    return Ok(Some((Tok::Str(s), start..self.i)));
                }
                b'[' => {
                    // Platform conditional (`[$WIN32]`): ignored.
                    while self.i < self.s.len() && self.s[self.i] != b']' {
                        self.i += 1;
                    }
                    self.i += 1;
                    continue;
                }
                _ => {
                    while self.i < self.s.len()
                        && !self.s[self.i].is_ascii_whitespace()
                        && !matches!(self.s[self.i], b'{' | b'}' | b'"')
                    {
                        self.i += 1;
                    }
                    let s = String::from_utf8_lossy(&self.s[start..self.i]).into_owned();
                    return Ok(Some((Tok::Str(s), start..self.i)));
                }
            }
        }
    }
}

/// Parses a whole file into its top-level entries.
pub fn parse(text: &str) -> Result<Vec<Entry>, String> {
    let mut lx = Lexer {
        s: text.as_bytes(),
        i: 0,
    };
    let (entries, closed) = parse_entries(&mut lx)?;
    if closed.is_some() {
        return Err("unexpected `}`".into());
    }
    Ok(entries)
}

/// Reads entries until `}` (returns its span) or the end of input.
fn parse_entries(lx: &mut Lexer) -> Result<(Vec<Entry>, Option<Range<usize>>), String> {
    let mut out = Vec::new();
    loop {
        let Some((tok, _)) = lx.next()? else {
            return Ok((out, None));
        };
        let key = match tok {
            Tok::Str(k) => k,
            Tok::Close => {
                let end = lx.i;
                return Ok((out, Some(end - 1..end)));
            }
            Tok::Open => return Err(format!("unexpected `{{` at byte {}", lx.i - 1)),
        };
        let Some((tok, span)) = lx.next()? else {
            return Err(format!("key {key:?} has no value"));
        };
        match tok {
            Tok::Str(v) => out.push(Entry {
                key,
                value: Value::Str(v),
                span,
            }),
            Tok::Open => {
                let (children, close) = parse_entries(lx)?;
                let close = close.ok_or_else(|| format!("block {key:?} is never closed"))?;
                out.push(Entry {
                    key,
                    value: Value::Obj(children),
                    span: span.start..close.end,
                });
            }
            Tok::Close => return Err(format!("key {key:?} has no value")),
        }
    }
}

/// The entry named `key` (case-insensitive), the last one if repeated.
pub fn find<'a>(entries: &'a [Entry], key: &str) -> Option<&'a Entry> {
    entries
        .iter()
        .rev()
        .find(|e| e.key.eq_ignore_ascii_case(key))
}

/// Follows `path` through nested objects.
pub fn find_path<'a>(entries: &'a [Entry], path: &[&str]) -> Option<&'a Entry> {
    let (first, rest) = path.split_first()?;
    let e = find(entries, first)?;
    if rest.is_empty() {
        return Some(e);
    }
    match &e.value {
        Value::Obj(children) => find_path(children, rest),
        Value::Str(_) => None,
    }
}

pub fn str_at<'a>(entries: &'a [Entry], path: &[&str]) -> Option<&'a str> {
    match &find_path(entries, path)?.value {
        Value::Str(s) => Some(s),
        Value::Obj(_) => None,
    }
}

/// Quotes a string the way Steam writes it.
pub fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
"Root"
{
	// a comment
	"plain"		"value"
	"escaped"		"say \"hi\" C:\\Games"
	bare	token
	"nested"
	{
		"A"		"1"
		"cond"		"x"	[$WIN32]
	}
	"empty"
	{
	}
}
"#;

    #[test]
    fn parses_values_and_spans() {
        let doc = parse(SAMPLE).unwrap();
        assert_eq!(str_at(&doc, &["Root", "plain"]), Some("value"));
        assert_eq!(
            str_at(&doc, &["root", "ESCAPED"]),
            Some(r#"say "hi" C:\Games"#)
        );
        assert_eq!(str_at(&doc, &["Root", "bare"]), Some("token"));
        assert_eq!(str_at(&doc, &["Root", "nested", "a"]), Some("1"));
        assert_eq!(str_at(&doc, &["Root", "nested", "cond"]), Some("x"));
        let e = find_path(&doc, &["Root", "escaped"]).unwrap();
        assert_eq!(&SAMPLE[e.span.clone()], r#""say \"hi\" C:\\Games""#);
        let e = find_path(&doc, &["Root", "empty"]).unwrap();
        assert!(SAMPLE[e.span.clone()].starts_with('{'));
        assert!(SAMPLE[e.span.clone()].ends_with('}'));
    }

    #[test]
    fn quote_round_trips() {
        let s = r#"WINEDLLOVERRIDES="winmm=n,b" %command% C:\x"#;
        let text = format!("\"k\" {}", quote(s));
        assert_eq!(str_at(&parse(&text).unwrap(), &["k"]), Some(s));
    }

    #[test]
    fn rejects_broken_files() {
        assert!(parse("\"a\" {").is_err());
        assert!(parse("\"a\" \"b").is_err());
        assert!(parse("}").is_err());
        assert!(parse("\"a\"").is_err());
    }
}
