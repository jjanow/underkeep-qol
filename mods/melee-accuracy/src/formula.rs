//! A small arithmetic language for the melee hit chance formula.
//!
//! A formula gives the base hit chance that takes the place of the game's
//! flat 100; the game still adds melee hit bonuses and subtracts evasion.
//! Syntax: numbers, `+ - * /`, parentheses, unary minus, `min(a, b, ...)`,
//! `max(a, b, ...)` and the variables in [`Vars`] (any case). Math is done
//! in floating point and rounded to the nearest whole number at the end, so
//! `(STR + DEX + 5) / 2 * 10` is exact for odd sums.

/// What a formula can read, for one attack.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Vars {
    /// Effective Strength (gear and mutations in).
    pub str_: f64,
    /// Effective Dexterity.
    pub dex: f64,
    /// Effective Endurance.
    pub end: f64,
    /// Effective Wisdom.
    pub wis: f64,
    /// The weapon's stat: DEX for weapons that scale with it, else STR.
    pub wstat: f64,
    /// 1 when the attack carries Righteous Strike, else 0.
    pub rs: f64,
}

/// The variable names, as shown to players.
pub const VAR_NAMES: &str = "STR, DEX, END, WIS, WSTAT, RS";

/// Longest formula accepted, in bytes.
const MAX_LEN: usize = 300;
/// Deepest nesting of parentheses, calls and unary minus accepted.
const MAX_DEPTH: usize = 32;
/// The result is clamped to this range before it is used.
const LIMIT: f64 = 100_000.0;

#[derive(Debug, Clone, Copy, PartialEq)]
enum Var {
    Str,
    Dex,
    End,
    Wis,
    WStat,
    Rs,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Op {
    Add,
    Sub,
    Mul,
    Div,
}

#[derive(Debug, Clone, PartialEq)]
enum Expr {
    Num(f64),
    Var(Var),
    Neg(Box<Expr>),
    Bin(Op, Box<Expr>, Box<Expr>),
    Min(Vec<Expr>),
    Max(Vec<Expr>),
}

/// A parsed formula.
#[derive(Debug, Clone, PartialEq)]
pub struct Formula(Expr);

impl Formula {
    /// Parses `text`; the error says what is wrong and where.
    pub fn parse(text: &str) -> Result<Formula, String> {
        if text.len() > MAX_LEN {
            return Err(format!("longer than {MAX_LEN} characters"));
        }
        let toks = lex(text)?;
        if toks.is_empty() {
            return Err("empty".into());
        }
        let mut p = Parser { toks, pos: 0 };
        let e = p.expr(0)?;
        match p.toks.get(p.pos) {
            None => Ok(Formula(e)),
            Some(t) => Err(format!("unexpected {} at the end", t.show())),
        }
    }

    /// The base hit chance for `v`, rounded; `None` when it isn't a finite
    /// number (a division by zero).
    pub fn eval(&self, v: &Vars) -> Option<i32> {
        let r = eval(&self.0, v);
        // Clamped well inside i32, so the cast is exact.
        r.is_finite().then(|| r.round().clamp(-LIMIT, LIMIT) as i32)
    }
}

fn eval(e: &Expr, v: &Vars) -> f64 {
    match e {
        Expr::Num(n) => *n,
        Expr::Var(x) => match x {
            Var::Str => v.str_,
            Var::Dex => v.dex,
            Var::End => v.end,
            Var::Wis => v.wis,
            Var::WStat => v.wstat,
            Var::Rs => v.rs,
        },
        Expr::Neg(a) => -eval(a, v),
        Expr::Bin(op, a, b) => {
            let (a, b) = (eval(a, v), eval(b, v));
            match op {
                Op::Add => a + b,
                Op::Sub => a - b,
                Op::Mul => a * b,
                Op::Div => a / b,
            }
        }
        // NaN propagates: a broken argument makes the whole result unusable.
        Expr::Min(xs) => xs.iter().map(|x| eval(x, v)).fold(f64::INFINITY, |a, b| {
            if a.is_nan() || b.is_nan() {
                f64::NAN
            } else {
                a.min(b)
            }
        }),
        Expr::Max(xs) => xs
            .iter()
            .map(|x| eval(x, v))
            .fold(f64::NEG_INFINITY, |a, b| {
                if a.is_nan() || b.is_nan() {
                    f64::NAN
                } else {
                    a.max(b)
                }
            }),
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Num(f64),
    Ident(String),
    Op(Op),
    Open,
    Close,
    Comma,
}

impl Tok {
    fn show(&self) -> String {
        match self {
            Tok::Num(n) => format!("number {n}"),
            Tok::Ident(s) => format!("\"{s}\""),
            Tok::Op(Op::Add) => "\"+\"".into(),
            Tok::Op(Op::Sub) => "\"-\"".into(),
            Tok::Op(Op::Mul) => "\"*\"".into(),
            Tok::Op(Op::Div) => "\"/\"".into(),
            Tok::Open => "\"(\"".into(),
            Tok::Close => "\")\"".into(),
            Tok::Comma => "\",\"".into(),
        }
    }
}

fn lex(text: &str) -> Result<Vec<Tok>, String> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        match c {
            b' ' | b'\t' => i += 1,
            b'+' | b'-' | b'*' | b'x' | b'X' | b'/' if !is_ident_at(b, i) => {
                out.push(Tok::Op(match c {
                    b'+' => Op::Add,
                    b'-' => Op::Sub,
                    b'/' => Op::Div,
                    _ => Op::Mul,
                }));
                i += 1;
            }
            b'(' => {
                out.push(Tok::Open);
                i += 1;
            }
            b')' => {
                out.push(Tok::Close);
                i += 1;
            }
            b',' => {
                out.push(Tok::Comma);
                i += 1;
            }
            b'0'..=b'9' | b'.' => {
                let s = i;
                while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'.') {
                    i += 1;
                }
                let n = &text[s..i];
                out.push(Tok::Num(
                    n.parse().map_err(|_| format!("bad number \"{n}\""))?,
                ));
            }
            c if c.is_ascii_alphabetic() || c == b'_' => {
                let s = i;
                while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                    i += 1;
                }
                out.push(Tok::Ident(text[s..i].to_ascii_uppercase()));
            }
            _ => {
                let ch = text[i..].chars().next().unwrap_or('?');
                return Err(format!("unexpected \"{ch}\""));
            }
        }
    }
    Ok(out)
}

/// Whether an `x` at `i` is the start of a word (a variable or function)
/// rather than a times sign: it is a times sign only when no letter, digit
/// or underscore follows it. `2x3`, `(a) x 10` and `STR x 2` multiply.
fn is_ident_at(b: &[u8], i: usize) -> bool {
    matches!(b[i], b'x' | b'X')
        && b.get(i + 1)
            .is_some_and(|&n| n.is_ascii_alphabetic() || n == b'_')
}

struct Parser {
    toks: Vec<Tok>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }

    fn next(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.pos).cloned();
        self.pos += 1;
        t
    }

    fn expect(&mut self, want: &Tok) -> Result<(), String> {
        match self.next() {
            Some(ref t) if t == want => Ok(()),
            Some(t) => Err(format!("expected {}, found {}", want.show(), t.show())),
            None => Err(format!("expected {} before the end", want.show())),
        }
    }

    /// expr := term (('+' | '-') term)*
    fn expr(&mut self, depth: usize) -> Result<Expr, String> {
        let mut e = self.term(depth)?;
        while let Some(Tok::Op(op @ (Op::Add | Op::Sub))) = self.peek().cloned() {
            self.pos += 1;
            e = Expr::Bin(op, Box::new(e), Box::new(self.term(depth)?));
        }
        Ok(e)
    }

    /// term := unary (('*' | '/') unary)*
    fn term(&mut self, depth: usize) -> Result<Expr, String> {
        let mut e = self.unary(depth)?;
        while let Some(Tok::Op(op @ (Op::Mul | Op::Div))) = self.peek().cloned() {
            self.pos += 1;
            e = Expr::Bin(op, Box::new(e), Box::new(self.unary(depth)?));
        }
        Ok(e)
    }

    /// unary := ('-' | '+') unary | atom
    fn unary(&mut self, depth: usize) -> Result<Expr, String> {
        if depth > MAX_DEPTH {
            return Err(format!("nested more than {MAX_DEPTH} deep"));
        }
        match self.peek() {
            Some(Tok::Op(Op::Sub)) => {
                self.pos += 1;
                Ok(Expr::Neg(Box::new(self.unary(depth + 1)?)))
            }
            Some(Tok::Op(Op::Add)) => {
                self.pos += 1;
                self.unary(depth + 1)
            }
            _ => self.atom(depth),
        }
    }

    /// atom := number | variable | func '(' expr (',' expr)* ')' | '(' expr ')'
    fn atom(&mut self, depth: usize) -> Result<Expr, String> {
        match self.next() {
            Some(Tok::Num(n)) => Ok(Expr::Num(n)),
            Some(Tok::Open) => {
                let e = self.expr(depth + 1)?;
                self.expect(&Tok::Close)?;
                Ok(e)
            }
            Some(Tok::Ident(name)) => {
                let var = match name.as_str() {
                    "STR" => Some(Var::Str),
                    "DEX" => Some(Var::Dex),
                    "END" => Some(Var::End),
                    "WIS" => Some(Var::Wis),
                    "WSTAT" => Some(Var::WStat),
                    "RS" => Some(Var::Rs),
                    _ => None,
                };
                if let Some(v) = var {
                    return Ok(Expr::Var(v));
                }
                let is_min = match name.as_str() {
                    "MIN" => true,
                    "MAX" => false,
                    _ => {
                        return Err(format!(
                            "unknown name \"{name}\" (variables: {VAR_NAMES}; functions: min, max)"
                        ));
                    }
                };
                self.expect(&Tok::Open)?;
                let mut args = vec![self.expr(depth + 1)?];
                while self.peek() == Some(&Tok::Comma) {
                    self.pos += 1;
                    args.push(self.expr(depth + 1)?);
                }
                self.expect(&Tok::Close)?;
                Ok(if is_min {
                    Expr::Min(args)
                } else {
                    Expr::Max(args)
                })
            }
            Some(t) => Err(format!("unexpected {}", t.show())),
            None => Err("unexpected end".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(str_: f64, dex: f64) -> Vars {
        Vars {
            str_,
            dex,
            end: 4.0,
            wis: 3.0,
            wstat: str_,
            rs: 0.0,
        }
    }

    fn ev(f: &str, vars: &Vars) -> Option<i32> {
        Formula::parse(f).unwrap().eval(vars)
    }

    #[test]
    fn ranged_mirror_rounds_at_the_end() {
        let f = "(STR + DEX + 5) / 2 * 10";
        assert_eq!(ev(f, &v(8.0, 7.0)), Some(100));
        assert_eq!(ev(f, &v(8.0, 6.0)), Some(95));
        assert_eq!(ev(f, &v(3.0, 2.0)), Some(50));
        // The player's own notation.
        assert_eq!(ev("(STR+DEX+5) / 2 x 10", &v(8.0, 6.0)), Some(95));
    }

    #[test]
    fn precedence_and_functions() {
        let z = Vars::default();
        assert_eq!(ev("1 + 2 * 3", &z), Some(7));
        assert_eq!(ev("(1 + 2) * 3", &z), Some(9));
        assert_eq!(ev("10 - 4 - 3", &z), Some(3));
        assert_eq!(ev("-2 * -3", &z), Some(6));
        assert_eq!(ev("max(100, 2 * 30)", &z), Some(100));
        assert_eq!(ev("min(100, 2 * 30, 70)", &z), Some(60));
        assert_eq!(ev("2.5 * 3", &z), Some(8));
        assert_eq!(ev("2x3", &z), Some(6));
        let vars = Vars {
            end: 4.0,
            wis: 3.0,
            wstat: 6.0,
            rs: 1.0,
            ..Vars::default()
        };
        assert_eq!(ev("end + Wis + WSTAT x 2 + rs * 10", &vars), Some(29));
        assert_eq!(ev("100 + max(0, WSTAT * 2)", &vars), Some(112));
    }

    #[test]
    fn broken_results_are_none() {
        let z = Vars::default();
        assert_eq!(ev("1 / 0", &z), None);
        assert_eq!(ev("0 / 0", &z), None);
        assert_eq!(ev("max(1, 0 / 0)", &z), None);
        assert_eq!(ev("99999999 * 99999999", &z), Some(100_000));
    }

    #[test]
    fn errors() {
        for bad in [
            "",
            "   ",
            "STR +",
            "(STR",
            "STR)",
            "LUCK * 10",
            "max(1, 2",
            "max 1",
            "1 2",
            "5 % 2",
            "1..2",
            "foo(1)",
        ] {
            assert!(Formula::parse(bad).is_err(), "{bad:?} should fail");
        }
        let deep = format!("{}1{}", "(".repeat(40), ")".repeat(40));
        assert!(Formula::parse(&deep).unwrap_err().contains("nested"));
        let negs = format!("{}1", "-".repeat(40));
        assert!(Formula::parse(&negs).is_err());
        assert!(
            Formula::parse(&"1+".repeat(200))
                .unwrap_err()
                .contains("longer")
        );
    }
}
