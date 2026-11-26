//! The `.env` format: `KEY=value` lines, as the dotenv tools read them.
//!
//! ```text
//! # a comment
//! KEY=value                 unquoted: to the end of the line, trimmed
//! KEY=value # comment       a # after white space starts a comment
//! export KEY=value          a leading export is ignored
//! KEY: value                a colon works as the separator too
//! KEY="two\nlines $OTHER"   double quotes: \n and \r are line breaks, a
//!                           backslash before any other character is
//!                           dropped, and $OTHER, ${OTHER} expand to
//!                           variables set earlier in the file (\$ keeps
//!                           the dollar)
//! KEY='as $written'         single quotes: nothing is expanded
//! ```
//!
//! A quoted value may span lines. Unquoted values expand variables too.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use apic_gocompat::quote;
use regex::{Captures, Regex};

/// Parses the text of a `.env` file into its variables.
pub fn parse(src: &[u8]) -> Result<BTreeMap<String, String>, String> {
    let text = String::from_utf8_lossy(src).replace("\r\n", "\n");
    let mut out = BTreeMap::new();
    let mut cutset = text.as_str();
    while let Some(start) = statement_start(cutset) {
        let (key, left) = locate_key_name(start)?;
        let (value, left) = extract_var_value(left, &out)?;
        out.insert(key, value);
        cutset = left;
    }
    Ok(out)
}

/// Skips white space and comment lines to where the next statement begins;
/// None at the end of the file.
fn statement_start(mut src: &str) -> Option<&str> {
    loop {
        let pos = src.find(|c: char| !is_unicode_space(c))?;
        src = &src[pos..];
        if !src.starts_with('#') {
            return Some(src);
        }
        // Skip the comment.
        let pos = src.find('\n')?;
        src = &src[pos..];
    }
}

/// The white space of the Latin-1 range plus the Unicode spaces.
fn is_unicode_space(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{0b}' | '\u{0c}' | '\r' | ' ' | '\u{85}' | '\u{a0}'
    ) || (c > '\u{ff}' && c.is_whitespace())
}

/// A space character that is not a line break.
fn is_space(c: char) -> bool {
    matches!(
        c,
        '\t' | '\u{0b}' | '\u{0c}' | '\r' | ' ' | '\u{85}' | '\u{a0}'
    )
}

/// Locates and reads the key name and returns it with the rest of the
/// text, which starts at the value.
fn locate_key_name(src: &str) -> Result<(String, &str), String> {
    // Trim "export" and the space after it.
    let mut src = src.trim_start_matches(is_space);
    if let Some(trimmed) = src.strip_prefix("export") {
        if trimmed.starts_with(is_space) {
            src = trimmed.trim_start_matches(is_space);
        }
    }
    // Find where the key ends, checking it byte by byte on the way.
    let mut key = "";
    let mut offset = 0;
    for (i, &b) in src.as_bytes().iter().enumerate() {
        let c = char::from(b);
        if is_space(c) {
            continue;
        }
        match b {
            b'=' | b':' => {
                // The yaml-style `KEY: value` is read too.
                key = &src[..i];
                offset = i + 1;
                break;
            }
            b'_' => {}
            _ => {
                // A variable name is letters, digits, _ and .
                if c.is_alphabetic() || c.is_numeric() || c == '.' {
                    continue;
                }
                return Err(format!(
                    "unexpected character {} in variable name near {}",
                    quote(&c.to_string()),
                    quote(src)
                ));
            }
        }
    }
    if src.is_empty() {
        return Err("zero length string".to_string());
    }
    let key = key.trim_end_matches(is_unicode_space);
    let cutset = src[offset..].trim_start_matches(is_space);
    Ok((key.to_string(), cutset))
}

/// Reads a variable's value and returns it with the rest of the text.
fn extract_var_value<'a>(
    src: &'a str,
    vars: &BTreeMap<String, String>,
) -> Result<(String, &'a str), String> {
    let b = src.as_bytes();
    let quote_char = match b.first() {
        Some(&q @ (b'"' | b'\'')) => q,
        _ => {
            // Unquoted: read to the end of the line.
            let end_of_line = src.find(['\n', '\r']).unwrap_or(src.len());
            if end_of_line == 0 {
                return Ok((String::new(), src));
            }
            let line: Vec<char> = src[..end_of_line].chars().collect();
            // Work backwards to see whether the line ends in white space
            // then a comment (as in `value # some comment`).
            let mut end_of_var = line.len();
            for i in (1..line.len()).rev() {
                if line[i] == '#' && is_space(line[i - 1]) {
                    end_of_var = i;
                    break;
                }
            }
            let value: String = line[..end_of_var].iter().collect();
            let trimmed = value.trim_matches(is_space);
            return Ok((expand_variables(trimmed, vars), &src[end_of_line..]));
        }
    };
    // Look for the quote that ends the value.
    for i in 1..b.len() {
        if b[i] != quote_char {
            continue;
        }
        // Skip an escaped quote (\" or \', whichever the quote is).
        if b[i - 1] == b'\\' {
            continue;
        }
        let q = char::from(quote_char);
        let mut value = src[..i]
            .trim_end_matches(q)
            .trim_start_matches(q)
            .to_string();
        if quote_char == b'"' {
            // Double quotes unescape line breaks and expand variables.
            value = expand_variables(&expand_escapes(&value), vars);
        }
        return Ok((value, &src[i + 1..]));
    }
    // The quoted value is not terminated.
    let end = src.find('\n').unwrap_or(src.len());
    Err(format!("unterminated quoted value {}", &src[..end]))
}

static ESCAPE_REGEX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\\.").unwrap());
static EXPAND_VAR_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(\\)?(\$)(\()?\{?([A-Z0-9_]+)?\}?").unwrap());
static UNESCAPE_CHARS_REGEX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\\([^$])").unwrap());

fn expand_escapes(s: &str) -> String {
    let out = ESCAPE_REGEX.replace_all(s, |m: &Captures<'_>| match &m[0] {
        "\\n" => "\n".to_string(),
        "\\r" => "\r".to_string(),
        other => other.to_string(),
    });
    UNESCAPE_CHARS_REGEX.replace_all(&out, "$1").into_owned()
}

fn expand_variables(v: &str, vars: &BTreeMap<String, String>) -> String {
    EXPAND_VAR_REGEX
        .replace_all(v, |m: &Captures<'_>| {
            let whole = &m[0];
            if m.get(1).is_some() {
                // An escaped dollar stays, without its backslash.
                return whole[1..].to_string();
            }
            match m.get(4) {
                Some(name) => vars.get(name.as_str()).cloned().unwrap_or_default(),
                None => whole.to_string(),
            }
        })
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(src: &str) -> Vec<(String, String)> {
        parse(src.as_bytes()).unwrap().into_iter().collect()
    }

    fn pairs(want: &[(&str, &str)]) -> Vec<(String, String)> {
        want.iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn reads_the_common_forms() {
        let src = "# comment\nA=1\n  B = two words  # trailing\nexport C=c\nD: yaml\nE=\"x\\ny $A ${B} \\$A \\q\"\nF='$A \\n'\nG=\n\nH=a#b # c\r\nI=\"multi\nline\"\nJ=$A-${C}-$NOPE.\n";
        assert_eq!(
            vars(src),
            pairs(&[
                ("A", "1"),
                ("B", "two words"),
                ("C", "c"),
                ("D", "yaml"),
                ("E", "x\ny 1 two words $A q"),
                ("F", "$A \\n"),
                ("G", ""),
                ("H", "a#b"),
                ("I", "multi\nline"),
                ("J", "1-c-."),
            ])
        );
    }

    #[test]
    fn says_what_it_cannot_read() {
        assert_eq!(
            parse(b"A=1\nB='x\n").unwrap_err(),
            "unterminated quoted value 'x"
        );
        assert_eq!(
            parse(b"A B\nC=1").unwrap_err(),
            "unexpected character \"\\n\" in variable name near \"A B\\nC=1\""
        );
        assert_eq!(
            parse(b"A-B=1").unwrap_err(),
            "unexpected character \"-\" in variable name near \"A-B=1\""
        );
    }
}

#[cfg(test)]
mod reference_tests {
    use super::parse;

    /// Inputs with the variables, or the error, recorded for each.
    #[test]
    fn reference_files() {
        let data = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/testdata/dotenv_reference.json"
        ))
        .unwrap();
        let rows: serde_json::Value = serde_json::from_str(&data).unwrap();
        let mut failures = Vec::new();
        for row in rows.as_array().unwrap() {
            let input = row["In"].as_str().unwrap();
            let got = parse(input.as_bytes());
            let want_err = row["Err"].as_str().unwrap();
            let same = match &got {
                Err(e) => e == want_err,
                Ok(vars) => {
                    want_err.is_empty()
                        && row["Out"].as_object().is_some_and(|want| {
                            want.len() == vars.len()
                                && want
                                    .iter()
                                    .all(|(k, v)| vars.get(k).map(String::as_str) == v.as_str())
                        })
                }
            };
            if !same {
                failures.push(format!(
                    "{input:?}:\n  got  {got:?}\n  want {} {want_err:?}",
                    row["Out"]
                ));
            }
        }
        assert!(
            failures.is_empty(),
            "{} differ:\n{}",
            failures.len(),
            failures.join("\n")
        );
    }
}
