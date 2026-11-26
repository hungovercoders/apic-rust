use std::sync::LazyLock;

use apic_gocompat::quote;

use crate::Phrase;

/// One of apic test's fixed vocabulary entries. The runner binds handlers
/// by `name`; the patterns live here so validation can detect `# @step`
/// phrases that would be ambiguous with them. `shapes` spell out the texts
/// the regex accepts with every alternation expanded, so the overlap check
/// can unify a phrase against them word by word. A capture is written the
/// way the regex constrains it: `"{x}"` for a quoted value, `{#}` for a
/// number, `{#ms}` for a number followed by ms, `{x}` for any single
/// token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltinStep {
    pub name: &'static str,
    pub regex: &'static str,
    pub shapes: Vec<String>,
}

fn step(name: &'static str, regex: &'static str, shapes: &[&str]) -> BuiltinStep {
    BuiltinStep {
        name,
        regex,
        shapes: shapes.iter().map(|s| s.to_string()).collect(),
    }
}

fn expanded(
    name: &'static str,
    regex: &'static str,
    shape: &str,
    alts: &[(&str, &[&str])],
) -> BuiltinStep {
    BuiltinStep {
        name,
        regex,
        shapes: expand(shape, alts),
    }
}

const WHERE: (&str, &[&str]) = ("{where}", &["body", "header", "cookie"]);

/// The fixed step vocabulary, in registration order.
pub static BUILTIN: LazyLock<Vec<BuiltinStep>> = LazyLock::new(|| {
    vec![
        step(
            "environment",
            r#"^the environment is "([^"]*)"$"#,
            &[r#"the environment is "{x}""#],
        ),
        step(
            "variable",
            r#"^the variable "([^"]+)" is "([^"]*)"$"#,
            &[r#"the variable "{x}" is "{y}""#],
        ),
        step("variables", r"^the variables:$", &["the variables:"]),
        step("run", r#"^I run "([^"]*)"$"#, &[r#"I run "{x}""#]),
        step(
            "run-with",
            r#"^I run "([^"]*)" with:$"#,
            &[r#"I run "{x}" with:"#],
        ),
        step(
            "run-file",
            r#"^I run the file "([^"]*)"$"#,
            &[r#"I run the file "{x}""#],
        ),
        step(
            "status",
            r"^the response status is (\d+)$",
            &["the response status is {#}"],
        ),
        step(
            "status-not",
            r"^the response status is not (\d+)$",
            &["the response status is not {#}"],
        ),
        step(
            "status-class",
            r"^the response is (successful|a client error|a server error)$",
            &[
                "the response is successful",
                "the response is a client error",
                "the response is a server error",
            ],
        ),
        expanded(
            "compare",
            r#"^the response (body|header|cookie) "([^"]*)" (is not|is|equals|contains|starts with|ends with|matches) "([^"]*)"$"#,
            r#"the response {where} "{sel}" {op} "{v}""#,
            &[
                WHERE,
                (
                    "{op}",
                    &[
                        "is not",
                        "is",
                        "equals",
                        "contains",
                        "starts with",
                        "ends with",
                        "matches",
                    ],
                ),
            ],
        ),
        expanded(
            "exists",
            r#"^the response (body|header|cookie) "([^"]*)" (exists|does not exist)$"#,
            r#"the response {where} "{sel}" {op}"#,
            &[WHERE, ("{op}", &["exists", "does not exist"])],
        ),
        step(
            "length",
            r#"^the response body "([^"]*)" has length (\d+)$"#,
            &[r#"the response body "{sel}" has length {#}"#],
        ),
        expanded(
            "type",
            r#"^the response body "([^"]*)" is (a string|a number|an integer|a boolean|an array|an object|null)$"#,
            r#"the response body "{sel}" is {type}"#,
            &[(
                "{type}",
                &[
                    "a string",
                    "a number",
                    "an integer",
                    "a boolean",
                    "an array",
                    "an object",
                    "null",
                ],
            )],
        ),
        step(
            "empty",
            r#"^the response body "([^"]*)" (is empty|is not empty)$"#,
            &[
                r#"the response body "{sel}" is empty"#,
                r#"the response body "{sel}" is not empty"#,
            ],
        ),
        step(
            "schema",
            r#"^the response body(?: "([^"]*)")? matches the schema "([^"]*)"$"#,
            &[
                r#"the response body matches the schema "{file}""#,
                r#"the response body "{sel}" matches the schema "{file}""#,
            ],
        ),
        step(
            "body-equals",
            r"^the response body is:$",
            &["the response body is:"],
        ),
        step(
            "body-contains",
            r"^the response body contains:$",
            &["the response body contains:"],
        ),
        step(
            "duration",
            r"^the response time is under (\d+) ?ms$",
            &[
                "the response time is under {#} ms",
                "the response time is under {#ms}",
            ],
        ),
        expanded(
            "capture",
            r#"^I capture the response (body|header|cookie) "([^"]*)" as "([^"]+)"$"#,
            r#"I capture the response {where} "{sel}" as "{name}""#,
            &[WHERE],
        ),
    ]
});

/// The fixed step vocabulary, in registration order: [`BUILTIN`] as a
/// slice.
pub fn builtin() -> &'static [BuiltinStep] {
    &BUILTIN
}

/// Substitutes every combination of the given alternations into a shape.
fn expand(shape: &str, alts: &[(&str, &[&str])]) -> Vec<String> {
    let mut out = vec![shape.to_string()];
    for (placeholder, words) in alts {
        out = out
            .iter()
            .flat_map(|s| words.iter().map(move |w| s.replace(placeholder, w)))
            .collect();
    }
    out
}

/// The most parameters a phrase may declare.
pub const MAX_PARAMS: usize = 6;

/// Narrows what a parameter can match, mirroring the regex that backs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParamKind {
    /// `("[^"]*"|\S+)`: a quoted value or a bare word.
    Any,
    /// `"([^"]*)"`: quoted text only.
    Quoted,
    /// `(\d+)`: digits only.
    Digits,
    /// `(\d+)ms`: digits followed by ms, as one word.
    DigitsMs,
}

/// A word of a phrase: a literal, or a parameter. A parameter matches
/// exactly one token of step text: a bare word, or a quoted value, which
/// is a single token even when it contains spaces because no literal word
/// can match text that includes quotes.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Literal(String),
    Param {
        kind: ParamKind,
        /// Literal text around a phrase placeholder inside one word, as in
        /// `{id}foo`; a matching word must carry it.
        prefix: String,
        suffix: String,
    },
}

impl Token {
    fn param(kind: ParamKind) -> Token {
        Token::Param {
            kind,
            prefix: String::new(),
            suffix: String::new(),
        }
    }

    fn word(w: &str) -> Token {
        if w == "{#}" {
            Token::param(ParamKind::Digits)
        } else if w == "{#ms}" {
            Token::param(ParamKind::DigitsMs)
        } else if w.contains('{') && is_quoted(w) {
            Token::param(ParamKind::Quoted)
        } else if let Some(open) = w.find('{') {
            Token::Param {
                kind: ParamKind::Any,
                prefix: w[..open].to_string(),
                suffix: w.rfind('}').map_or("", |close| &w[close + 1..]).to_string(),
            }
        } else {
            Token::Literal(w.to_string())
        }
    }
}

/// Splits step text into tokens. A double-quoted span is one token even
/// when it contains spaces, matching how the step patterns treat it.
fn tokenize(text: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quote = false;
    for c in text.chars() {
        if (c == ' ' || c == '\t') && !in_quote {
            if !cur.is_empty() {
                out.push(Token::word(&cur));
                cur.clear();
            }
            continue;
        }
        if c == '"' {
            in_quote = !in_quote;
        }
        cur.push(c);
    }
    if !cur.is_empty() {
        out.push(Token::word(&cur));
    }
    out
}

fn is_quoted(w: &str) -> bool {
    w.len() >= 2 && w.starts_with('"') && w.ends_with('"')
}

fn is_digits(w: &str) -> bool {
    !w.is_empty() && w.bytes().all(|c| c.is_ascii_digit())
}

/// Reports whether some step text could match both token patterns: the
/// sequences must have the same length and, position by position, the
/// tokens must admit a common word. Literals must be equal; a parameter
/// admits what its kind allows, so a quoted-only capture never meets a
/// bare word and a numeric capture never meets a quoted value.
fn overlap(a: &[Token], b: &[Token]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| meet(x, y))
}

/// Reports whether one word can satisfy both tokens.
fn meet(x: &Token, y: &Token) -> bool {
    match (x, y) {
        (Token::Literal(a), Token::Literal(b)) => a == b,
        (
            Token::Param {
                kind: xk,
                prefix: xp,
                suffix: xs,
            },
            Token::Param {
                kind: yk,
                prefix: yp,
                suffix: ys,
            },
        ) => match (*xk, *yk) {
            // Both free: a common word needs compatible affixes, e.g.
            // {x}foo and {y}bar can never match the same word.
            (ParamKind::Any, ParamKind::Any) => {
                (xp.starts_with(yp.as_str()) || yp.starts_with(xp.as_str()))
                    && (xs.ends_with(ys.as_str()) || ys.ends_with(xs.as_str()))
            }
            (ParamKind::Any, kind) => affixes_fit(xp, xs, kind),
            (kind, ParamKind::Any) => affixes_fit(yp, ys, kind),
            (a, b) => a == b,
        },
        (
            Token::Param {
                kind,
                prefix,
                suffix,
            },
            Token::Literal(lit),
        )
        | (
            Token::Literal(lit),
            Token::Param {
                kind,
                prefix,
                suffix,
            },
        ) => match kind {
            ParamKind::Quoted => is_quoted(lit),
            ParamKind::Digits => is_digits(lit),
            ParamKind::DigitsMs => lit.strip_suffix("ms").is_some_and(is_digits),
            ParamKind::Any => {
                lit.starts_with(prefix.as_str())
                    && lit.ends_with(suffix.as_str())
                    && lit.len() >= prefix.len() + suffix.len()
            }
        },
    }
}

/// Reports whether a free placeholder with literal affixes can still
/// produce a word of the constrained kind.
fn affixes_fit(prefix: &str, suffix: &str, kind: ParamKind) -> bool {
    match kind {
        ParamKind::Quoted => {
            (prefix.is_empty() || prefix.starts_with('"'))
                && (suffix.is_empty() || suffix.ends_with('"'))
        }
        ParamKind::Digits => {
            (prefix.is_empty() || is_digits(prefix)) && (suffix.is_empty() || is_digits(suffix))
        }
        ParamKind::DigitsMs => {
            let ok_suffix = suffix.is_empty()
                || is_digits(suffix)
                || suffix
                    .strip_suffix("ms")
                    .is_some_and(|rest| rest.is_empty() || is_digits(rest));
            (prefix.is_empty() || is_digits(prefix)) && ok_suffix
        }
        ParamKind::Any => true,
    }
}

impl Phrase {
    /// Reports whether text matching this phrase could also match a
    /// built-in step, which the test runner treats as ambiguous. The
    /// error names the step it collides with.
    pub fn conflicts_with_builtin(&self) -> Result<(), String> {
        let mine = tokenize(&self.text);
        for b in builtin() {
            for shape in &b.shapes {
                if overlap(&mine, &tokenize(shape)) {
                    return Err(format!(
                        "@step {} could match the same text as the built-in step {} ({})",
                        quote(&self.text),
                        b.name,
                        b.regex.trim_matches(['^', '$'])
                    ));
                }
            }
        }
        Ok(())
    }

    /// Reports whether some step text could match both phrases.
    pub fn conflicts_with(&self, other: &Phrase) -> bool {
        overlap(&tokenize(&self.text), &tokenize(&other.text))
    }
}
