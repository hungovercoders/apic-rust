//! Reads the first YAML document of a file into a small tree that keeps
//! what `apic.yaml` is decoded from: each scalar as it was written (so
//! `timeout: 30` gives the text `30`, whatever a number parser would make
//! of it), the tag a plain scalar resolves to, and the line of any node,
//! for messages.
//!
//! The parser underneath hands out values, not text or positions, so the
//! tree is built in two walks over the same document (the first finds the
//! shape, the second reads every scalar as a string), and the position of
//! a node is found on demand by a third walk that stops at it.

use std::cell::Cell;
use std::fmt;
use std::sync::LazyLock;

use regex::Regex;
use serde::de::{
    self, DeserializeSeed, Deserializer, EnumAccess, MapAccess, SeqAccess, VariantAccess, Visitor,
};
use serde::Deserialize;

/// The outline of a document: where the scalars, sequences and mappings
/// are, and whether a scalar read as text.
enum Shape {
    /// `text` is false for a scalar that read as null, a boolean or a
    /// number, which only a plain (unquoted) scalar does.
    Scalar {
        text: bool,
    },
    Seq(Vec<Shape>),
    Map(Vec<(Shape, Shape)>),
}

const NOT_TEXT: Shape = Shape::Scalar { text: false };

impl<'de> Deserialize<'de> for Shape {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Shape, D::Error> {
        d.deserialize_any(ShapeVisitor)
    }
}

struct ShapeVisitor;

impl<'de> Visitor<'de> for ShapeVisitor {
    type Value = Shape;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a YAML value")
    }

    fn visit_bool<E>(self, _: bool) -> Result<Shape, E> {
        Ok(NOT_TEXT)
    }

    fn visit_i64<E>(self, _: i64) -> Result<Shape, E> {
        Ok(NOT_TEXT)
    }

    fn visit_i128<E>(self, _: i128) -> Result<Shape, E> {
        Ok(NOT_TEXT)
    }

    fn visit_u64<E>(self, _: u64) -> Result<Shape, E> {
        Ok(NOT_TEXT)
    }

    fn visit_u128<E>(self, _: u128) -> Result<Shape, E> {
        Ok(NOT_TEXT)
    }

    fn visit_f64<E>(self, _: f64) -> Result<Shape, E> {
        Ok(NOT_TEXT)
    }

    fn visit_str<E>(self, _: &str) -> Result<Shape, E> {
        Ok(Shape::Scalar { text: true })
    }

    fn visit_unit<E>(self) -> Result<Shape, E> {
        Ok(NOT_TEXT)
    }

    fn visit_none<E>(self) -> Result<Shape, E> {
        Ok(NOT_TEXT)
    }

    fn visit_some<D: Deserializer<'de>>(self, d: D) -> Result<Shape, D::Error> {
        Shape::deserialize(d)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Shape, A::Error> {
        let mut items = Vec::new();
        while let Some(item) = seq.next_element()? {
            items.push(item);
        }
        Ok(Shape::Seq(items))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Shape, A::Error> {
        let mut pairs = Vec::new();
        while let Some(pair) = map.next_entry()? {
            pairs.push(pair);
        }
        Ok(Shape::Map(pairs))
    }

    /// A value under a tag of the file's own (`!thing value`): the value,
    /// with a scalar taken as text.
    fn visit_enum<A: EnumAccess<'de>>(self, data: A) -> Result<Shape, A::Error> {
        let (_, variant): (String, _) = data.variant()?;
        Ok(match variant.newtype_variant()? {
            Shape::Scalar { .. } => Shape::Scalar { text: true },
            other => other,
        })
    }
}

/// One node of the document. `id` numbers the nodes in document order.
pub(crate) struct Node {
    id: usize,
    pub(crate) kind: Kind,
}

pub(crate) enum Kind {
    /// A scalar as written, without its quotes.
    Scalar {
        raw: String,
        text: bool,
    },
    Seq(Vec<Node>),
    Map(Vec<(Node, Node)>),
}

/// Reads the node a [`Shape`] describes. When `stop` is the node's id the
/// read fails there instead, which makes the parser report where it is.
#[derive(Clone, Copy)]
struct Build<'a> {
    shape: &'a Shape,
    next: &'a Cell<usize>,
    stop: Option<usize>,
}

const STOP: &str = "stop";

impl<'de> DeserializeSeed<'de> for Build<'_> {
    type Value = Node;

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<Node, D::Error> {
        let id = self.next.get();
        self.next.set(id + 1);
        let visitor = BuildVisitor {
            build: self,
            stop: self.stop == Some(id),
        };
        let kind = match self.shape {
            Shape::Scalar { .. } => d.deserialize_str(visitor)?,
            Shape::Seq(_) => d.deserialize_seq(visitor)?,
            Shape::Map(_) => d.deserialize_map(visitor)?,
        };
        Ok(Node { id, kind })
    }
}

struct BuildVisitor<'a> {
    build: Build<'a>,
    stop: bool,
}

impl<'a> BuildVisitor<'a> {
    fn child(&self, shape: &'a Shape) -> Build<'a> {
        Build {
            shape,
            next: self.build.next,
            stop: self.build.stop,
        }
    }
}

fn changed<E: de::Error>() -> E {
    E::custom("the document changed while it was read")
}

impl<'de> Visitor<'de> for BuildVisitor<'_> {
    type Value = Kind;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a YAML value")
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<Kind, E> {
        if self.stop {
            return Err(E::custom(STOP));
        }
        let Shape::Scalar { text } = self.build.shape else {
            return Err(changed());
        };
        Ok(Kind::Scalar {
            raw: v.to_string(),
            text: *text,
        })
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Kind, A::Error> {
        if self.stop {
            return Err(de::Error::custom(STOP));
        }
        let Shape::Seq(shapes) = self.build.shape else {
            return Err(changed());
        };
        let mut items = Vec::with_capacity(shapes.len());
        for shape in shapes {
            let item = seq.next_element_seed(self.child(shape))?;
            items.push(item.ok_or_else(changed)?);
        }
        Ok(Kind::Seq(items))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Kind, A::Error> {
        if self.stop {
            return Err(de::Error::custom(STOP));
        }
        let Shape::Map(shapes) = self.build.shape else {
            return Err(changed());
        };
        let mut pairs = Vec::with_capacity(shapes.len());
        for (key, value) in shapes {
            let key = map.next_key_seed(self.child(key))?;
            let key = key.ok_or_else(changed)?;
            pairs.push((key, map.next_value_seed(self.child(value))?));
        }
        Ok(Kind::Map(pairs))
    }
}

/// What a plain scalar is, by the rules `apic.yaml` has always been read
/// with: YAML 1.2's core types, plus the older spellings of octal numbers
/// (`0777`) and digit groups (`1_000`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Resolved {
    Null,
    Bool(bool),
    Int(i64),
    /// A whole number too large for `Int`.
    Uint(u64),
    Float(f64),
    Timestamp,
    /// The merge key, `<<`.
    Merge,
    Str,
}

impl Resolved {
    /// The tag as messages name it.
    pub fn tag(self) -> &'static str {
        match self {
            Resolved::Null => "!!null",
            Resolved::Bool(_) => "!!bool",
            Resolved::Int(_) | Resolved::Uint(_) => "!!int",
            Resolved::Float(_) => "!!float",
            Resolved::Timestamp => "!!timestamp",
            Resolved::Merge => "!!merge",
            Resolved::Str => "!!str",
        }
    }
}

static FLOAT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[-+]?(\.[0-9]+|[0-9]+(\.[0-9]*)?)([eE][-+]?[0-9]+)?$").expect("a valid pattern")
});

static DOT_FLOAT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\.[0-9]+([eE][-+]?[0-9]+)?$").expect("a valid pattern"));

static TIMESTAMP: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^([0-9]{4})-([0-9]{1,2})-([0-9]{1,2})(?:(?:[Tt]([0-9]{1,2}):([0-9]{1,2}):([0-9]{1,2})(?:\.[0-9]+)?(?:Z|[-+]([0-9]{2}):([0-9]{2})))|(?: ([0-9]{1,2}):([0-9]{1,2}):([0-9]{1,2})(?:\.[0-9]+)?))?$",
    )
    .expect("a valid pattern")
});

/// Whether a plain scalar is a date or a date and time: `2001-12-14`,
/// `2001-12-14T21:59:43.10-05:00` or `2001-12-14 21:59:43.10`.
fn is_timestamp(s: &str) -> bool {
    let Some(m) = TIMESTAMP.captures(s) else {
        return false;
    };
    let num = |i: usize| {
        m.get(i)
            .map(|g| g.as_str().parse::<u32>().unwrap_or(u32::MAX))
    };
    let (year, month, day) = (
        num(1).unwrap_or(0),
        num(2).unwrap_or(0),
        num(3).unwrap_or(0),
    );
    if !(1..=12).contains(&month) {
        return false;
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days = match month {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    if day < 1 || day > days {
        return false;
    }
    let in_range = |i: usize, max: u32| num(i).is_none_or(|v| v <= max);
    in_range(4, 23)
        && in_range(5, 59)
        && in_range(6, 59)
        && in_range(7, 23)
        && in_range(8, 59)
        && in_range(9, 23)
        && in_range(10, 59)
        && in_range(11, 59)
}

/// A whole number with an optional sign and a base prefix (`0x`, `0b`,
/// `0o`, or a bare leading `0` for octal), as a sign and a magnitude.
fn parse_int(s: &str) -> Option<(bool, u64)> {
    let (negative, digits) = match s.as_bytes().first()? {
        b'-' => (true, &s[1..]),
        b'+' => (false, &s[1..]),
        _ => (false, s),
    };
    let lower = digits.to_ascii_lowercase();
    let (radix, digits) = if let Some(rest) = lower.strip_prefix("0x") {
        (16, rest)
    } else if let Some(rest) = lower.strip_prefix("0b") {
        (2, rest)
    } else if let Some(rest) = lower.strip_prefix("0o") {
        (8, rest)
    } else if lower.len() > 1 && lower.starts_with('0') {
        (8, &lower[1..])
    } else {
        (10, lower.as_str())
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return None;
    }
    u64::from_str_radix(digits, radix)
        .ok()
        .map(|magnitude| (negative, magnitude))
}

/// Resolves a plain scalar.
pub fn resolve(raw: &str) -> Resolved {
    match raw {
        "" | "~" | "null" | "Null" | "NULL" => return Resolved::Null,
        "true" | "True" | "TRUE" => return Resolved::Bool(true),
        "false" | "False" | "FALSE" => return Resolved::Bool(false),
        ".nan" | ".NaN" | ".NAN" => return Resolved::Float(f64::NAN),
        ".inf" | ".Inf" | ".INF" | "+.inf" | "+.Inf" | "+.INF" => {
            return Resolved::Float(f64::INFINITY)
        }
        "-.inf" | "-.Inf" | "-.INF" => return Resolved::Float(f64::NEG_INFINITY),
        "<<" => return Resolved::Merge,
        _ => {}
    }
    match raw.as_bytes()[0] {
        b'.' => {
            if DOT_FLOAT.is_match(raw) {
                if let Ok(f) = raw.parse::<f64>() {
                    return Resolved::Float(f);
                }
            }
        }
        b'+' | b'-' | b'0'..=b'9' => {
            if is_timestamp(raw) {
                return Resolved::Timestamp;
            }
            let plain = raw.replace('_', "");
            match parse_int(&plain) {
                Some((false, n)) => {
                    return i64::try_from(n).map_or(Resolved::Uint(n), Resolved::Int);
                }
                Some((true, n)) if n <= i64::MAX as u64 + 1 => {
                    return Resolved::Int((n as i64).wrapping_neg());
                }
                _ => {}
            }
            if FLOAT.is_match(&plain) {
                match plain.parse::<f64>() {
                    Ok(f) if f.is_finite() => return Resolved::Float(f),
                    _ => {}
                }
            }
        }
        _ => {}
    }
    Resolved::Str
}

const NO_DOCUMENT: &str = "EOF while parsing a value";

/// The first document of a YAML file.
pub(crate) struct Doc<'a> {
    data: &'a [u8],
    shape: Shape,
    pub(crate) root: Node,
}

impl<'a> Doc<'a> {
    /// Reads the first document; `None` for a file that holds none (empty,
    /// or comments only). The error is the message to show, without the
    /// `yaml: ` that goes before it.
    pub(crate) fn parse(data: &'a [u8]) -> Result<Option<Doc<'a>>, String> {
        let Some(de) = serde_yaml_ng::Deserializer::from_slice(data).next() else {
            return Ok(None);
        };
        if let Some(problem) = incompatible_directive(data) {
            return Err(problem);
        }
        let shape = Shape::deserialize(de).map_err(|err| syntax_error(data, &err))?;
        let root = match build(data, &shape, None) {
            Ok(root) => root,
            // The parser's way of saying the file holds no document.
            Err(err) if err.to_string() == NO_DOCUMENT => return Ok(None),
            Err(err) => return Err(syntax_error(data, &err)),
        };
        Ok(Some(Doc { data, shape, root }))
    }

    /// Where a node starts: its 1-based line and the index of its first
    /// character.
    fn mark(&self, node: &Node) -> Option<(usize, usize)> {
        let err = build(self.data, &self.shape, Some(node.id)).err()?;
        let at = err.location()?;
        Some((at.line(), at.index()))
    }

    /// The 1-based line a node starts on; 0 when it cannot be told.
    pub(crate) fn line(&self, node: &Node) -> usize {
        self.mark(node).map_or(0, |(line, _)| line)
    }

    /// Whether a scalar was written plain: not quoted, not a block scalar
    /// and not tagged.
    fn plain(&self, node: &Node) -> bool {
        let Some((_, index)) = self.mark(node) else {
            return true;
        };
        let Ok(text) = std::str::from_utf8(self.data) else {
            return true;
        };
        let mut rest = text.chars().skip(index).peekable();
        // An anchor (`&name`) may come before the value.
        while rest.peek() == Some(&'&') {
            while rest.next_if(|c| !c.is_whitespace()).is_some() {}
            while rest.next_if(|c| c.is_whitespace()).is_some() {}
        }
        !matches!(rest.peek(), Some('"' | '\'' | '|' | '>' | '!'))
    }

    /// What a scalar node resolves to: [`Resolved::Str`] for anything that
    /// is not a plain scalar.
    pub(crate) fn resolved(&self, node: &Node) -> Resolved {
        let Kind::Scalar { raw, text } = &node.kind else {
            return Resolved::Str;
        };
        let resolved = resolve(raw);
        if *text && resolved != Resolved::Str && !self.plain(node) {
            return Resolved::Str;
        }
        resolved
    }
}

fn build(data: &[u8], shape: &Shape, stop: Option<usize>) -> Result<Node, serde_yaml_ng::Error> {
    let next = Cell::new(0);
    let seed = Build {
        shape,
        next: &next,
        stop,
    };
    match serde_yaml_ng::Deserializer::from_slice(data).next() {
        Some(de) => seed.deserialize(de),
        None => Err(de::Error::custom("the document changed while it was read")),
    }
}

static KIND: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^Error \{ kind: ([A-Z]+),").expect("a valid pattern"));
static PROBLEM_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"problem_mark: Mark \{ line: ([0-9]+),").expect("a valid pattern")
});
static CONTEXT_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"context_mark: Mark \{ line: ([0-9]+),").expect("a valid pattern")
});
static PROBLEM: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?s)^(.*?)(?: at line [0-9]+ column [0-9]+| at position [0-9]+)?(?:, .*)?$")
        .expect("a valid pattern")
});

/// Words a parser failure as `line 3: did not find expected key`. The line
/// is the one such messages have always carried: where the construct being
/// read began when the parser knows it, else where the problem is, counted
/// from zero for a problem of structure and from one for a problem of
/// characters; and no line at all on the first.
fn syntax_error(data: &[u8], err: &serde_yaml_ng::Error) -> String {
    let debug = format!("{err:?}");
    let display = err.to_string();
    let Some(kind) = KIND.captures(&debug) else {
        if display.starts_with("unknown anchor") {
            if let Some(name) = err.location().and_then(|at| alias_at(data, at.index())) {
                return format!("unknown anchor '{name}' referenced");
            }
        }
        // Not from the parser: the message as it is.
        return display;
    };
    let line = |re: &Regex| -> usize {
        re.captures(&debug)
            .and_then(|m| m[1].parse::<usize>().ok())
            .map_or(0, |line| line - 1)
    };
    let mut at = line(&CONTEXT_LINE);
    if at == 0 {
        at = line(&PROBLEM_LINE);
    }
    if at != 0 && &kind[1] == "SCANNER" {
        at += 1;
    }
    let problem = PROBLEM
        .captures(&display)
        .map_or(display.as_str(), |m| m.get(1).map_or("", |g| g.as_str()))
        // The spelling this message has always had.
        .replace("hexadecimal number", "hexdecimal number");
    if at == 0 {
        problem
    } else {
        format!("line {at}: {problem}")
    }
}

/// The name of the alias (`*name`) at a character index.
fn alias_at(data: &[u8], index: usize) -> Option<String> {
    let text = std::str::from_utf8(data).ok()?;
    let mut rest = text.chars().skip(index);
    if rest.next() != Some('*') {
        return None;
    }
    let name: String = rest
        .take_while(|c| !c.is_whitespace() && !",[]{}".contains(*c))
        .collect();
    Some(name)
}

/// A `%YAML` directive before the first document that names a version
/// other than 1.1 has always been refused, 1.2 included.
fn incompatible_directive(data: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(data);
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    for (at, line) in text.lines().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if !line.starts_with('%') {
            break;
        }
        let Some(version) = line.strip_prefix("%YAML") else {
            continue;
        };
        if !version.starts_with([' ', '\t']) {
            continue;
        }
        let version = version.split('#').next().unwrap_or("").trim();
        if version.is_empty() || version == "1.1" {
            continue;
        }
        // A version that is not two numbers is the parser's to report.
        let two_numbers = version.split_once('.').is_some_and(|(major, minor)| {
            let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
            digits(major) && digits(minor)
        });
        if !two_numbers {
            break;
        }
        let problem = "found incompatible YAML document";
        return Some(if at == 0 {
            problem.to_string()
        } else {
            format!("line {at}: {problem}")
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_plain_scalars() {
        let cases: [(&str, Resolved); 30] = [
            ("", Resolved::Null),
            ("~", Resolved::Null),
            ("NULL", Resolved::Null),
            ("nULL", Resolved::Str),
            ("true", Resolved::Bool(true)),
            ("FALSE", Resolved::Bool(false)),
            ("yes", Resolved::Str),
            ("0", Resolved::Int(0)),
            ("-12", Resolved::Int(-12)),
            ("+12", Resolved::Int(12)),
            ("1_000", Resolved::Int(1000)),
            ("0x1F", Resolved::Int(31)),
            ("0b101", Resolved::Int(5)),
            ("-0b101", Resolved::Int(-5)),
            ("0o17", Resolved::Int(15)),
            ("0777", Resolved::Int(511)),
            ("089", Resolved::Float(89.0)),
            ("9223372036854775807", Resolved::Int(i64::MAX)),
            ("-9223372036854775808", Resolved::Int(i64::MIN)),
            ("9223372036854775808", Resolved::Uint(9223372036854775808)),
            ("1.5", Resolved::Float(1.5)),
            ("1e3", Resolved::Float(1000.0)),
            (".5", Resolved::Float(0.5)),
            ("-.inf", Resolved::Float(f64::NEG_INFINITY)),
            ("1e999", Resolved::Str),
            ("2001-12-14", Resolved::Timestamp),
            ("2001-12-14T21:59:43.10-05:00", Resolved::Timestamp),
            ("2001-13-14", Resolved::Str),
            ("30s", Resolved::Str),
            ("<<", Resolved::Merge),
        ];
        for (raw, want) in cases {
            assert_eq!(resolve(raw), want, "resolve({raw:?})");
        }
        assert!(matches!(resolve(".nan"), Resolved::Float(f) if f.is_nan()));
    }

    #[test]
    fn finds_lines_and_styles() {
        let text = "a: 1\nb:\n  - \"2\"\n  - &x 3\nc: {d: '4'}\n";
        let doc = Doc::parse(text.as_bytes()).unwrap().unwrap();
        let Kind::Map(pairs) = &doc.root.kind else {
            panic!("not a mapping");
        };
        assert_eq!(doc.line(&doc.root), 1);
        assert_eq!(doc.line(&pairs[1].0), 2);
        let Kind::Seq(items) = &pairs[1].1.kind else {
            panic!("not a sequence");
        };
        assert_eq!(doc.line(&items[0]), 3);
        assert_eq!(doc.resolved(&items[0]), Resolved::Str);
        assert_eq!(doc.line(&items[1]), 4);
        assert_eq!(doc.resolved(&items[1]), Resolved::Int(3));
        let Kind::Map(inner) = &pairs[2].1.kind else {
            panic!("not a mapping");
        };
        assert_eq!(doc.line(&inner[0].1), 5);
        assert_eq!(doc.resolved(&inner[0].1), Resolved::Str);
        assert_eq!(doc.resolved(&pairs[0].1), Resolved::Int(1));
    }

    #[test]
    fn an_empty_file_has_no_document() {
        assert!(Doc::parse(b"").unwrap().is_none());
        assert!(Doc::parse(b"# nothing here\n").unwrap().is_none());
    }
}
