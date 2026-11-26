use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::{Arc, LazyLock};

use apic_gocompat::{check_json, quote};
use apic_selector::{compile_regex, leading, select_value, Kind, Number, Response, Value};
use regex::Regex;
use serde::Serialize;

use crate::schema::Resolved;

/// The supported comparison operators.
pub const OPERATORS: [&str; 13] = [
    "==",
    "!=",
    "<=",
    ">=",
    "<",
    ">",
    "contains",
    "matches",
    "startsWith",
    "endsWith",
    "matchesSchema",
    "exists",
    "not exists",
];

/// Predicates take no value: `body.$.id isInteger`, `body.$.items not
/// isEmpty`. Each also has a `not` form.
pub const PREDICATES: [&str; 8] = [
    "isString", "isNumber", "isInteger", "isBoolean", "isArray", "isObject", "isNull", "isEmpty",
];

/// The operators `length` takes: `body.$.items length == 3`.
const COMPARISONS: [&str; 6] = ["==", "!=", "<=", ">=", "<", ">"];

/// A parsed assertion.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Expr {
    pub selector: String,
    /// The operator, a predicate (`isString`, `not isEmpty`) or
    /// `matchesSchema`, whose value is the schema file.
    pub op: String,
    /// The raw right-hand side; may contain {{placeholders}}.
    pub value: String,
    /// Compares the selected value's length (characters, elements or keys)
    /// rather than the value: `body.$.items length == 3`.
    pub length: bool,
}

fn is_predicate(op: &str) -> bool {
    PREDICATES.contains(&op)
}

fn without_not(op: &str) -> &str {
    op.strip_prefix("not ").unwrap_or(op)
}

impl Expr {
    /// Reports an operator that takes no value.
    pub fn unary(&self) -> bool {
        self.op == "exists" || self.op == "not exists" || is_predicate(without_not(&self.op))
    }

    /// How an expression reads in results: the selector, the operator and
    /// the rendered value.
    pub fn display(&self, expected: &str) -> String {
        let op = if self.length { format!("length {}", self.op) } else { self.op.clone() };
        if self.unary() {
            return format!("{} {}", self.selector, op);
        }
        format!("{} {} {}", self.selector, op, expected)
    }
}

/// Returns the JSON Schema named by `matchesSchema`, read from a file
/// resolved and confined the way the caller's files are; build one with
/// [`schemas`].
pub type SchemaLoader<'a> = dyn Fn(&str) -> std::result::Result<Arc<Resolved>, String> + 'a;

/// What evaluation may need beyond the response.
#[derive(Clone, Copy, Default)]
pub struct Options<'a> {
    /// The loader `matchesSchema` reads its file with. None refuses the
    /// operator.
    pub schema: Option<&'a SchemaLoader<'a>>,
}

/// Returns a schema loader that reads each file with `read`, then parses
/// and resolves it once: every later assertion naming the same file, and
/// every retry of the request, gets the resolved schema (or the same
/// error) without reading it again. It is not for use across threads.
pub fn schemas<'a>(
    read: impl Fn(&str) -> std::result::Result<Vec<u8>, String> + 'a,
) -> impl Fn(&str) -> std::result::Result<Arc<Resolved>, String> + 'a {
    let cache: RefCell<HashMap<String, std::result::Result<Arc<Resolved>, String>>> = RefCell::new(HashMap::new());
    move |path: &str| {
        if let Some(entry) = cache.borrow().get(path) {
            return entry.clone();
        }
        let entry = load_schema(&read, path);
        cache.borrow_mut().insert(path.to_string(), entry.clone());
        entry
    }
}

fn load_schema(
    read: &dyn Fn(&str) -> std::result::Result<Vec<u8>, String>,
    path: &str,
) -> std::result::Result<Arc<Resolved>, String> {
    let data = read(path)?;
    let wrap = |e: String| format!("schema {path}: {e}");
    check_json(&data).map_err(wrap)?;
    let value: serde_json::Value = serde_json::from_slice(&data).map_err(|e| wrap(e.to_string()))?;
    Resolved::new(&value).map(Arc::new).map_err(wrap)
}

/// The outcome of evaluating one assertion.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Result {
    pub expr: String,
    pub pass: bool,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub actual: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub expected: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub error: String,
}

/// Splits an expression into selector, operator and value.
pub fn parse(expr: &str) -> std::result::Result<Expr, String> {
    let s = expr.trim();
    // The selector may hold spaces inside brackets (a filter), so it is
    // read up to the first space outside one.
    let (sel, rest) = leading(s);
    let mut fields: Vec<&str> = vec![sel];
    fields.extend(rest.split_whitespace());
    let sel = sel.to_string();
    if fields.len() < 2 {
        return Err(format!("assert {}: expected `<selector> <op> <value>`", quote(expr)));
    }
    if rest == "exists" || rest == "not exists" {
        return Ok(Expr { selector: sel, op: rest.to_string(), ..Expr::default() });
    }
    let unary = fields[1..].join(" ");
    if is_predicate(without_not(&unary)) {
        return Ok(Expr { selector: sel, op: unary, ..Expr::default() });
    }
    if is_predicate(fields[1]) || fields[1] == "not" && fields.len() > 2 && is_predicate(fields[2]) {
        return Err(format!(
            "assert {}: {} does not take a value",
            quote(expr),
            quote(&fields[1..fields.len().min(3)].join(" "))
        ));
    }
    if fields[1] == "length" {
        if fields.len() < 4 {
            return Err(format!(
                "assert {}: expected `<selector> length <op> <number>`",
                quote(expr)
            ));
        }
        let op = fields[2];
        if !COMPARISONS.contains(&op) {
            return Err(format!(
                "assert {}: length takes one of {}, got {}",
                quote(expr),
                COMPARISONS.join(", "),
                quote(op)
            ));
        }
        let after = rest.strip_prefix("length").unwrap_or(rest).trim();
        let val = after.strip_prefix(op).unwrap_or(after).trim();
        return Ok(Expr { selector: sel, op: op.to_string(), value: val.to_string(), length: true });
    }
    if rest.starts_with("exists ") {
        return Err(format!("assert {}: {} does not take a value", quote(expr), quote("exists")));
    }
    if rest.starts_with("not exists ") {
        return Err(format!(
            "assert {}: {} does not take a value",
            quote(expr),
            quote("not exists")
        ));
    }
    let op = fields[1];
    if !OPERATORS.contains(&op) {
        return Err(format!(
            "assert {}: unknown operator {} (one of {}, length <op>, or a predicate: {})",
            quote(expr),
            quote(op),
            OPERATORS.join(", "),
            PREDICATES.join(", ")
        ));
    }
    let mut val = rest[op.len()..].trim();
    if val.is_empty() {
        return Err(format!("assert {}: expected `<selector> <op> <value>`", quote(expr)));
    }
    let b = val.as_bytes();
    if b.len() >= 2 && (b[0] == b'"' && b[b.len() - 1] == b'"' || b[0] == b'\'' && b[b.len() - 1] == b'\'') {
        val = &val[1..val.len() - 1];
    }
    Ok(Expr { selector: sel, op: op.to_string(), value: val.to_string(), length: false })
}

/// Keeps an assertion's selector and operator and replaces its value with
/// `mask`. The selector is read the way [`parse`] reads it, so a filter
/// with spaces stays whole. What `parse` cannot read is masked after its
/// first two words, so a secret never shows because the expression around
/// it was malformed.
pub fn redact(expr: &str, mask: &str) -> String {
    if let Ok(e) = parse(expr) {
        if e.unary() {
            return expr.to_string();
        }
        return e.display(mask);
    }
    let fields: Vec<&str> = expr.split_whitespace().collect();
    match fields.len() {
        0 | 1 => expr.to_string(),
        2 => format!("{} {}", fields[0], mask),
        _ => format!("{} {} {}", fields[0], fields[1], mask),
    }
}

/// Evaluates a parsed expression against a response. `expected` is the
/// right-hand side after template rendering.
pub fn eval(e: &Expr, expected: &str, resp: &Response) -> Result {
    eval_with(e, expected, resp, Options::default())
}

/// [`eval`] with what `matchesSchema` needs.
pub fn eval_with(e: &Expr, expected: &str, resp: &Response, opts: Options<'_>) -> Result {
    let mut res = Result { expr: e.display(expected), ..Result::default() };
    if !e.unary() {
        res.expected = expected.to_string();
    }
    let found = match select_value(resp, &e.selector) {
        Ok(found) => found,
        Err(err) => {
            res.error = err.to_string();
            return res;
        }
    };
    let ok = found.is_some();
    let v = found.unwrap_or_default();
    res.actual = v.text.clone();
    match e.op.as_str() {
        "exists" => {
            res.pass = ok;
            return res;
        }
        "not exists" => {
            res.pass = !ok;
            return res;
        }
        _ => {}
    }
    if !ok {
        res.error = format!("no value at {}", e.selector);
        return res;
    }
    if e.length {
        match length(&v) {
            Ok(n) => {
                res.actual = n.to_string();
                (res.pass, res.error) = compare(&res.actual, &e.op, expected);
            }
            Err(err) => res.error = err,
        }
    } else if e.unary() {
        let want = !e.op.starts_with("not ");
        match predicate(without_not(&e.op), &v) {
            Ok(got) => {
                if e.op != "isEmpty" && e.op != "not isEmpty" {
                    res.actual = v.kind.to_string(); // what it is, not what it says
                }
                res.pass = got == want;
            }
            Err(err) => res.error = err,
        }
    } else if e.op == "matchesSchema" {
        match matches_schema(&e.selector, &v, expected, opts) {
            Ok(problem) => {
                res.pass = problem.is_empty();
                if !res.pass {
                    res.actual = problem;
                }
            }
            Err(err) => res.error = err,
        }
    } else {
        (res.pass, res.error) = compare(&v.text, &e.op, expected);
    }
    res
}

/// Reads JSON text, with the error a reader of a broken document is shown.
fn parse_json(text: &str) -> std::result::Result<serde_json::Value, String> {
    check_json(text.as_bytes())?;
    serde_json::from_str(text).map_err(|e| e.to_string())
}

/// A string's characters, an array's elements or an object's keys.
fn length(v: &Value) -> std::result::Result<usize, String> {
    match v.kind {
        Kind::String => Ok(v.text.chars().count()),
        Kind::Array => match parse_json(&v.text)? {
            serde_json::Value::Array(a) => Ok(a.len()),
            _ => Err("json: cannot unmarshal value into Go value of type []json.RawMessage".to_string()),
        },
        Kind::Object => match parse_json(&v.text)? {
            serde_json::Value::Object(o) => Ok(o.len()),
            _ => Err("json: cannot unmarshal value into Go value of type map[string]json.RawMessage".to_string()),
        },
        _ => Err(format!(
            "length needs a string, an array or an object, got {} {}",
            v.kind, v.text
        )),
    }
}

fn predicate(op: &str, v: &Value) -> std::result::Result<bool, String> {
    match op {
        "isString" => Ok(v.kind == Kind::String),
        "isNumber" => Ok(v.kind == Kind::Number),
        "isInteger" => Ok(v.kind == Kind::Number && parse_number(&v.text).is_some_and(|n| n.is_int())),
        "isBoolean" => Ok(v.kind == Kind::Bool),
        "isArray" => Ok(v.kind == Kind::Array),
        "isObject" => Ok(v.kind == Kind::Object),
        "isNull" => Ok(v.kind == Kind::Null),
        "isEmpty" => {
            if !matches!(v.kind, Kind::String | Kind::Array | Kind::Object) {
                return Err(format!(
                    "isEmpty needs a string, an array or an object, got {} {}",
                    v.kind, v.text
                ));
            }
            Ok(length(v)? == 0)
        }
        _ => Err(format!("unknown predicate {op}")),
    }
}

/// Validates the selected value against a JSON Schema file (draft 2020-12
/// or draft-07, `$ref` within the file). It returns what does not match,
/// or "" when it does; the error is a schema that cannot be read.
fn matches_schema(sel: &str, v: &Value, path: &str, opts: Options<'_>) -> std::result::Result<String, String> {
    let Some(loader) = opts.schema else {
        return Err("matchesSchema is not available here".to_string());
    };
    let resolved = loader(path)?;
    let instance = if v.kind == Kind::String && sel != "body" {
        serde_json::Value::String(v.text.clone()) // a JSON string value, already unquoted
    } else {
        parse_json(&v.text).map_err(|e| format!("matchesSchema needs JSON, and {sel} is not: {e}"))?
    };
    Ok(resolved.validate(&instance).err().unwrap_or_default())
}

/// The decimal syntax assertions treat as numeric: an optional sign,
/// digits with an optional fraction, an optional exponent.
static RE_NUMBER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[+-]?([0-9]+\.?[0-9]*|\.[0-9]+)(?:[eE]([+-]?[0-9]+))?$").unwrap());

// Bounds on the digits parse_number expands: response values are
// untrusted, so 1e1000000000 must not be parsed.
const MAX_NUMBER_DIGITS: usize = 4096;
const MAX_NUMBER_EXPONENT: i64 = 4096;

/// Reads a decimal number exactly, so large integers such as
/// 9007199254740993 keep their value instead of rounding through a 64-bit
/// float, and values beyond a float's range (1e1000) still compare as
/// numbers. Words, Inf, NaN, fractions like 1/2 and numbers with more than
/// 4096 digits or an exponent beyond ±4096 are not numbers here and
/// compare as text.
pub fn parse_number(s: &str) -> Option<Number> {
    let m = RE_NUMBER.captures(s)?;
    if m[1].len() > MAX_NUMBER_DIGITS {
        return None;
    }
    if let Some(exp) = m.get(2).map(|e| e.as_str()).filter(|e| !e.is_empty()) {
        let sign = exp.trim_end_matches(|c: char| c.is_ascii_digit());
        let digits = exp[sign.len()..].trim_start_matches('0'); // 1e+0004096 is 1e4096
        if digits.len() > 6 {
            // ±4096 needs four digits; anything longer is out of range anyway
            return None;
        }
        let exp: i64 = if digits.is_empty() { 0 } else { digits.parse().ok()? };
        if exp > MAX_NUMBER_EXPONENT {
            return None;
        }
    }
    Number::parse(s)
}

fn compare(actual: &str, op: &str, expected: &str) -> (bool, String) {
    let cmp = match (parse_number(actual), parse_number(expected)) {
        (Some(a), Some(e)) => Some(a.cmp(&e)),
        _ => None,
    };
    let ok = |b: bool| (b, String::new());
    match op {
        "==" => ok(cmp.map_or(actual == expected, |c| c == Ordering::Equal)),
        "!=" => ok(cmp.map_or(actual != expected, |c| c != Ordering::Equal)),
        "<" | "<=" | ">" | ">=" => {
            let Some(cmp) = cmp else {
                return (
                    false,
                    format!("{} needs numeric operands, got {} and {}", op, quote(actual), quote(expected)),
                );
            };
            ok(match op {
                "<" => cmp == Ordering::Less,
                "<=" => cmp != Ordering::Greater,
                ">" => cmp == Ordering::Greater,
                _ => cmp != Ordering::Less,
            })
        }
        "contains" => ok(actual.contains(expected)),
        "startsWith" => ok(actual.starts_with(expected)),
        "endsWith" => ok(actual.ends_with(expected)),
        "matches" => match compile_regex(expected) {
            Ok(re) => ok(re.is_match(actual)),
            Err(e) => (false, format!("bad regex: {e}")),
        },
        _ => (false, format!("unknown operator {op}")),
    }
}
