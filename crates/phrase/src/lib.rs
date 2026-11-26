//! Turns `# @step` phrases such as `a user named {name} exists` into
//! anchored regular expressions whose capture groups map to variables.
//!
//! The patterns ([`Phrase::regex`], [`BuiltinStep::regex`]) are kept as
//! text, since they are shown in messages; compile one with [`compile`],
//! which reads `\d`, `\w` and `\s` as their ASCII classes so a pattern
//! matches the same step text everywhere.

#![forbid(unsafe_code)]

mod builtin;

use std::collections::BTreeMap;
use std::sync::LazyLock;

use apic_gocompat::quote;
use regex::Regex;

pub use builtin::{builtin, BuiltinStep, BUILTIN, MAX_PARAMS};

/// Parameter names follow the variable grammar (letters, digits, `_ . -`).
static RE_PARAM: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\{([A-Za-z_][0-9A-Za-z_.-]*)\}").expect("a valid pattern"));

/// A compiled step phrase.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Phrase {
    /// As written.
    pub text: String,
    /// Variable names in order of appearance.
    pub params: Vec<String>,
    /// Anchored pattern; compile it with [`Phrase::compile`].
    pub regex: String,
}

/// Escapes the characters that mean something in a pattern, so `text`
/// matches itself.
fn quote_meta(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if r"\.+*?()|[]{}^$".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Compiles a step pattern. The classes `\d`, `\w`, `\s` and their
/// negations are the ASCII ones: `\d` is `0` to `9` and `\S` is anything
/// but a plain blank.
pub fn compile(pattern: &str) -> Result<Regex, regex::Error> {
    let mut out = String::with_capacity(pattern.len());
    let mut chars = pattern.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('d') => out.push_str("[0-9]"),
            Some('D') => out.push_str("[^0-9]"),
            Some('w') => out.push_str("[0-9A-Za-z_]"),
            Some('W') => out.push_str("[^0-9A-Za-z_]"),
            Some('s') => out.push_str(r"[\t\n\f\r ]"),
            Some('S') => out.push_str(r"[^\t\n\f\r ]"),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    Regex::new(&out)
}

/// Compiles a phrase: see [`Phrase::parse`].
pub fn parse(text: &str) -> Result<Phrase, String> {
    Phrase::parse(text)
}

impl Phrase {
    /// Compiles a phrase. Each `{name}` matches a double-quoted string or
    /// a bare word, so `a user named {name} exists` matches both
    /// `a user named "alice" exists` and `a user named alice exists`. A
    /// placeholder written as `"{name}"` matches quoted text only.
    pub fn parse(text: &str) -> Result<Phrase, String> {
        let text = text.trim();
        if text.is_empty() {
            return Err("@step needs a phrase".to_string());
        }
        if RE_PARAM.replace_all(text, "").contains(['{', '}']) {
            return Err(format!(
                "@step {}: braces must wrap a parameter name like {{userId}}",
                quote(text)
            ));
        }
        let mut params: Vec<String> = Vec::new();
        let mut regex = String::from("^");
        let mut last = 0;
        let bytes = text.as_bytes();
        for m in RE_PARAM.captures_iter(text) {
            let (whole, name) = (m.get(0).expect("the match"), &m[1]);
            regex.push_str(&quote_meta(&text[last..whole.start()]));
            if params.iter().any(|p| p == name) {
                return Err(format!(
                    "@step {}: parameter {{{name}}} appears twice",
                    quote(text)
                ));
            }
            if name.contains(".response.") {
                return Err(format!(
                    "@step {}: parameter {{{name}}} uses \".response.\", which is reserved for response references",
                    quote(text)
                ));
            }
            params.push(name.to_string());
            let quoted = whole.start() > 0
                && bytes[whole.start() - 1] == b'"'
                && bytes.get(whole.end()) == Some(&b'"');
            if quoted {
                // `"{name}"`: the quotes are literal, the value is their
                // content.
                regex.push_str(r#"([^"]*)"#);
            } else {
                regex.push_str(r#"("[^"]*"|\S+)"#);
            }
            last = whole.end();
        }
        if params.len() > MAX_PARAMS {
            return Err(format!(
                "@step {}: more than {MAX_PARAMS} parameters",
                quote(text)
            ));
        }
        regex.push_str(&quote_meta(&text[last..]));
        regex.push('$');
        if let Err(err) = compile(&regex) {
            return Err(format!("@step {}: {err}", quote(text)));
        }
        Ok(Phrase {
            text: text.to_string(),
            params,
            regex,
        })
    }

    /// The phrase's pattern, ready to match step text.
    pub fn compile(&self) -> Result<Regex, regex::Error> {
        compile(&self.regex)
    }

    /// Pairs captured arguments (one per parameter, in order) with
    /// parameter names, stripping surrounding double quotes.
    pub fn values<S: AsRef<str>>(&self, args: &[S]) -> BTreeMap<String, String> {
        self.params
            .iter()
            .zip(args)
            .map(|(name, arg)| (name.clone(), unquote(arg.as_ref()).to_string()))
            .collect()
    }
}

/// Strips one pair of surrounding double quotes.
pub fn unquote(s: &str) -> &str {
    if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

#[cfg(test)]
mod tests;
