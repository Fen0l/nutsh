//! The `$filter` subset this program sends, evaluated against fixture rows.
//!
//! Off unless the builder asks (`honours_filter`), for the reason `$orderby` is: the default
//! mock is the Prism Central that ignores the parameter, and every test that reads a count off
//! the mock was written against that. A filter this grammar cannot read is a 400, not "all
//! rows": a spelling the program starts sending has to show as an error, not as a pane that
//! quietly stopped narrowing.
//!
//! Every spelling here is one the program sends (`crates/catalog/pages.toml`,
//! `crates/core/src/{stats,evidence,search}.rs`):
//!
//! ```text
//! or        := and ( 'or' and )*
//! and       := primary ( 'and' primary )*
//! primary   := '(' or ')' | 'startswith' '(' path ',' string ')' | path op literal
//! op        := eq | ne | ge | le | gt | lt
//! path      := ident ( '/' ident )*             -- '/' walks into a nested object
//! literal   := string | Ns.Type string | true | false | number | instant
//! string    := '...'                            -- '' inside stands for one quote
//! instant   := 2026-09-05T09:49:00Z             -- unquoted, as the evidence pane sends it
//! ```
//!
//! `and` binds tighter than `or`. An enum literal (`Prism.Config.TaskStatus'RUNNING'`)
//! compares its quoted part. Strings order bytewise, except that two RFC 3339 instants order
//! by time: `…:00.5Z` is after `…:00Z`, whatever their lengths.

use std::borrow::Cow;
use std::cmp::Ordering;

use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Filter {
    And(Box<Filter>, Box<Filter>),
    Or(Box<Filter>, Box<Filter>),
    Compare {
        path: Vec<String>,
        op: Op,
        literal: Literal,
    },
    StartsWith {
        path: Vec<String>,
        prefix: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Op {
    Eq,
    Ne,
    Ge,
    Le,
    Gt,
    Lt,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Literal {
    /// A quoted string, the quoted part of an enum literal, or an unquoted instant.
    Text(String),
    Bool(bool),
    Number(f64),
}

impl Filter {
    pub(crate) fn parse(input: &str) -> Result<Filter, String> {
        let mut parser = Parser {
            tokens: tokens(input)?,
            at: 0,
        };
        let filter = parser.or()?;
        if let Some(extra) = parser.tokens.get(parser.at) {
            return Err(format!("unexpected {extra:?} after the expression"));
        }
        Ok(filter)
    }

    pub(crate) fn matches(&self, row: &Value) -> bool {
        match self {
            Filter::And(a, b) => a.matches(row) && b.matches(row),
            Filter::Or(a, b) => a.matches(row) || b.matches(row),
            Filter::StartsWith { path, prefix } => lookup(row, path)
                .and_then(Value::as_str)
                .is_some_and(|s| s.starts_with(prefix.as_str())),
            Filter::Compare { path, op, literal } => compare(lookup(row, path), *op, literal),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Word(String),
    Str(String),
    Open,
    Close,
    Comma,
}

fn tokens(input: &str) -> Result<Vec<Token>, String> {
    let mut out = Vec::new();
    let mut chars = input.char_indices().peekable();
    while let Some(&(i, c)) = chars.peek() {
        match c {
            ' ' | '\t' => {
                chars.next();
            }
            '(' => {
                chars.next();
                out.push(Token::Open);
            }
            ')' => {
                chars.next();
                out.push(Token::Close);
            }
            ',' => {
                chars.next();
                out.push(Token::Comma);
            }
            '\'' => {
                chars.next();
                out.push(Token::Str(quoted(&mut chars, i)?));
            }
            _ => {
                let mut word = String::new();
                while let Some(&(_, c)) = chars.peek() {
                    if matches!(c, ' ' | '\t' | '(' | ')' | ',' | '\'') {
                        break;
                    }
                    word.push(c);
                    chars.next();
                }
                // `Prism.Config.TaskStatus'RUNNING'`: the word names the enum's type and the
                // quoted part is the value; only the value is compared.
                if let Some(&(j, '\'')) = chars.peek() {
                    chars.next();
                    out.push(Token::Str(quoted(&mut chars, j)?));
                } else {
                    out.push(Token::Word(word));
                }
            }
        }
    }
    Ok(out)
}

/// The rest of a `'...'` string after its opening quote, `''` standing for one quote.
fn quoted(
    chars: &mut std::iter::Peekable<std::str::CharIndices<'_>>,
    start: usize,
) -> Result<String, String> {
    let mut s = String::new();
    loop {
        match chars.next() {
            None => return Err(format!("unterminated string at {start}")),
            Some((_, '\'')) => {
                if chars.peek().is_some_and(|&(_, c)| c == '\'') {
                    chars.next();
                    s.push('\'');
                } else {
                    return Ok(s);
                }
            }
            Some((_, c)) => s.push(c),
        }
    }
}

struct Parser {
    tokens: Vec<Token>,
    at: usize,
}

impl Parser {
    /// Consume one token. Not named `next`: clippy's `should_implement_trait` (in `all`, which
    /// the workspace denies) takes an inherent `fn next(&mut self) -> Option<_>` for a missing
    /// `Iterator` impl.
    fn bump(&mut self) -> Option<Token> {
        let token = self.tokens.get(self.at).cloned();
        self.at += 1;
        token
    }

    fn word_is(&self, word: &str) -> bool {
        matches!(self.tokens.get(self.at), Some(Token::Word(w)) if w.eq_ignore_ascii_case(word))
    }

    fn expect(&mut self, token: Token) -> Result<(), String> {
        match self.bump() {
            Some(t) if t == token => Ok(()),
            other => Err(format!("expected {token:?}, got {other:?}")),
        }
    }

    fn or(&mut self) -> Result<Filter, String> {
        let mut left = self.and()?;
        while self.word_is("or") {
            self.at += 1;
            let right = self.and()?;
            left = Filter::Or(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn and(&mut self) -> Result<Filter, String> {
        let mut left = self.primary()?;
        while self.word_is("and") {
            self.at += 1;
            let right = self.primary()?;
            left = Filter::And(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn primary(&mut self) -> Result<Filter, String> {
        match self.bump() {
            Some(Token::Open) => {
                let inner = self.or()?;
                self.expect(Token::Close)?;
                Ok(inner)
            }
            Some(Token::Word(w)) if w.eq_ignore_ascii_case("startswith") => {
                self.expect(Token::Open)?;
                let path = match self.bump() {
                    Some(Token::Word(p)) => path_of(&p)?,
                    other => return Err(format!("startswith: expected a property, got {other:?}")),
                };
                self.expect(Token::Comma)?;
                let prefix = match self.bump() {
                    Some(Token::Str(s)) => s,
                    other => return Err(format!("startswith: expected a string, got {other:?}")),
                };
                self.expect(Token::Close)?;
                Ok(Filter::StartsWith { path, prefix })
            }
            Some(Token::Word(w)) => {
                let path = path_of(&w)?;
                let op = match self.bump() {
                    Some(Token::Word(o)) => op_of(&o)?,
                    other => return Err(format!("expected an operator after {w}, got {other:?}")),
                };
                let literal = match self.bump() {
                    Some(Token::Str(s)) => Literal::Text(s),
                    Some(Token::Word(v)) => literal_of(&v)?,
                    other => return Err(format!("expected a value after {w}, got {other:?}")),
                };
                Ok(Filter::Compare { path, op, literal })
            }
            other => Err(format!("expected a property or '(', got {other:?}")),
        }
    }
}

/// `cluster/extId` → `["cluster", "extId"]`; each segment a property name.
fn path_of(word: &str) -> Result<Vec<String>, String> {
    let segments: Vec<String> = word.split('/').map(str::to_string).collect();
    let ident = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
    };
    if segments.iter().all(|s| ident(s)) {
        Ok(segments)
    } else {
        Err(format!("{word} is not a property path"))
    }
}

fn op_of(word: &str) -> Result<Op, String> {
    Ok(match word.to_ascii_lowercase().as_str() {
        "eq" => Op::Eq,
        "ne" => Op::Ne,
        "ge" => Op::Ge,
        "le" => Op::Le,
        "gt" => Op::Gt,
        "lt" => Op::Lt,
        _ => return Err(format!("{word} is not an operator")),
    })
}

/// An unquoted literal: a boolean, a number, or an RFC 3339 instant. A bare word that is none
/// of those (`RUNNING` without its enum prefix and quotes) is an error, as it is on a Prism
/// Central.
fn literal_of(word: &str) -> Result<Literal, String> {
    match word {
        "true" => return Ok(Literal::Bool(true)),
        "false" => return Ok(Literal::Bool(false)),
        _ => {}
    }
    if let Ok(n) = word.parse::<f64>() {
        return Ok(Literal::Number(n));
    }
    if is_instant(word) {
        return Ok(Literal::Text(word.to_string()));
    }
    Err(format!("cannot read the value {word}"))
}

/// `2026-09-05T09:49:00Z`, optionally with a fraction: what the evidence pane sends unquoted.
fn is_instant(s: &str) -> bool {
    let b = s.as_bytes();
    s.len() >= 20 && b[4] == b'-' && b[7] == b'-' && b[10] == b'T' && s.ends_with('Z')
}

/// The value at `path`, walking nested objects; `null` counts as absent.
fn lookup<'a>(row: &'a Value, path: &[String]) -> Option<&'a Value> {
    path.iter()
        .try_fold(row, |v, segment| v.get(segment))
        .filter(|v| !v.is_null())
}

/// OData's rule for a property the row lacks: it is `ne` everything and nothing else.
fn compare(value: Option<&Value>, op: Op, literal: &Literal) -> bool {
    let ordering = match (value, literal) {
        (None, _) => return op == Op::Ne,
        (Some(Value::Bool(v)), Literal::Bool(l)) => {
            return match op {
                Op::Eq => v == l,
                Op::Ne => v != l,
                _ => false,
            };
        }
        (Some(_), Literal::Bool(_)) => return op == Op::Ne,
        (Some(v), Literal::Number(l)) => match v.as_f64().and_then(|v| v.partial_cmp(l)) {
            Some(ordering) => ordering,
            None => return op == Op::Ne,
        },
        (Some(v), Literal::Text(l)) => match v.as_str() {
            Some(v) => instant_key(v).cmp(&instant_key(l)),
            None => return op == Op::Ne,
        },
    };
    match op {
        Op::Eq => ordering == Ordering::Equal,
        Op::Ne => ordering != Ordering::Equal,
        Op::Ge => ordering != Ordering::Less,
        Op::Le => ordering != Ordering::Greater,
        Op::Gt => ordering == Ordering::Greater,
        Op::Lt => ordering == Ordering::Less,
    }
}

/// Two RFC 3339 instants order by time whatever their fractional digits, the way the
/// handler's `sort_key` orders a list: the fraction is padded to nine digits and an absent one
/// is zero. Anything else is its own text.
fn instant_key(s: &str) -> Cow<'_, str> {
    if !is_instant(s) {
        return Cow::Borrowed(s);
    }
    let body = &s[..s.len() - 1];
    let (head, fraction) = body.split_once('.').unwrap_or((body, ""));
    Cow::Owned(format!("{head}.{fraction:0<9}Z"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse(s: &str) -> Filter {
        Filter::parse(s).unwrap_or_else(|e| panic!("{s}: {e}"))
    }

    #[test]
    fn and_binds_tighter_than_or() {
        let f = parse("a eq 1 or b eq 2 and c eq 3");
        assert!(matches!(f, Filter::Or(_, ref right) if matches!(**right, Filter::And(..))));
        assert!(f.matches(&json!({"a": 1})));
        assert!(!f.matches(&json!({"b": 2})));
        assert!(f.matches(&json!({"b": 2, "c": 3})));
        let g = parse("(a eq 1 or b eq 2) and c eq 3");
        assert!(!g.matches(&json!({"a": 1})));
    }

    #[test]
    fn an_enum_literal_compares_its_quoted_part() {
        let f = parse("status eq Prism.Config.TaskStatus'RUNNING'");
        assert!(f.matches(&json!({"status": "RUNNING"})));
        assert!(!f.matches(&json!({"status": "QUEUED"})));
    }

    #[test]
    fn a_doubled_quote_is_one_quote() {
        let f = parse("startswith(name,'o''brien')");
        assert!(f.matches(&json!({"name": "o'brien-01"})));
        assert!(!f.matches(&json!({"name": "obrien-01"})));
    }

    #[test]
    fn a_nested_path_walks_objects() {
        let f = parse("cluster/extId eq 'c1'");
        assert!(f.matches(&json!({"cluster": {"extId": "c1"}})));
        assert!(!f.matches(&json!({"cluster": {"extId": "c2"}})));
        assert!(!f.matches(&json!({"cluster": "c1"})));
    }

    #[test]
    fn an_absent_property_is_only_ne() {
        assert!(!parse("isResolved eq false").matches(&json!({})));
        assert!(parse("isResolved ne false").matches(&json!({})));
        assert!(!parse("isResolved eq false").matches(&json!({"isResolved": null})));
    }

    #[test]
    fn instants_order_by_time_not_by_length() {
        let f =
            parse("creationTime ge 2026-09-05T09:49:00Z and creationTime le 2026-09-05T09:49:00Z");
        assert!(f.matches(&json!({"creationTime": "2026-09-05T09:49:00Z"})));
        assert!(!f.matches(&json!({"creationTime": "2026-09-05T09:49:00.5Z"})));
        assert!(
            parse("creationTime gt 2026-09-05T09:49:00Z")
                .matches(&json!({"creationTime": "2026-09-05T09:49:00.5Z"}))
        );
        assert!(
            parse("creationTime lt 2026-09-05T09:49:00.5Z")
                .matches(&json!({"creationTime": "2026-09-05T09:49:00Z"}))
        );
    }

    #[test]
    fn booleans_and_numbers_compare_as_themselves() {
        assert!(parse("isResolved eq false").matches(&json!({"isResolved": false})));
        assert!(!parse("isResolved eq false").matches(&json!({"isResolved": "false"})));
        assert!(parse("numSockets ge 2").matches(&json!({"numSockets": 4})));
        assert!(!parse("numSockets gt 4").matches(&json!({"numSockets": 4})));
    }

    #[test]
    fn what_the_grammar_refuses() {
        for bad in [
            "",
            "status eq RUNNING",
            "powerState eq",
            "name contains 'web'",
            "(isResolved eq false",
            "isResolved eq false and",
            "isResolved eq false extra",
            "startswith(name)",
            "name eq 'unterminated",
        ] {
            assert!(Filter::parse(bad).is_err(), "{bad:?} should not parse");
        }
    }
}
