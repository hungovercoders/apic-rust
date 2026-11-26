//! Substitutes `{{expr}}` placeholders.
//!
//! A resolver is any `FnMut(&str) -> Result<Option<String>, E>`: it
//! returns the value for an expression such as `baseUrl`, `$uuid`,
//! `$processEnv HOME` or `login.response.body.$.id`. `Ok(None)` means the
//! expression is unknown; `Err` reports a malformed one, with whatever
//! error type the caller uses.

#![forbid(unsafe_code)]

use std::collections::HashSet;
use std::fmt;
use std::sync::LazyLock;

use regex::Regex;

static RE_PLACEHOLDER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\{\{[\t\n\f\r ]*([^{}]+?)[\t\n\f\r ]*\}\}").expect("a valid pattern")
});

/// Lists expressions that could not be resolved.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MissingError {
    /// The distinct unknown expressions, in order of appearance.
    pub exprs: Vec<String>,
    /// The text with every known placeholder substituted and the unknown
    /// ones left in place.
    pub output: String,
}

impl fmt::Display for MissingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "missing variables: {}", self.exprs.join(", "))
    }
}

impl std::error::Error for MissingError {}

/// Why [`render`] could not substitute every placeholder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenderError<E> {
    /// Some expressions are unknown; the rest were substituted.
    Missing(MissingError),
    /// The resolver refused an expression; rendering stopped there.
    Resolve {
        /// The expression, as written between the braces.
        expr: String,
        /// The resolver's error.
        source: E,
        /// The text as far as it was rendered: placeholders before the
        /// failing one substituted, the rest left in place.
        output: String,
    },
}

impl<E> RenderError<E> {
    /// The text as far as it could be rendered.
    pub fn output(&self) -> &str {
        match self {
            RenderError::Missing(m) => &m.output,
            RenderError::Resolve { output, .. } => output,
        }
    }

    /// The unknown expressions, when that is what went wrong.
    pub fn missing(&self) -> Option<&MissingError> {
        match self {
            RenderError::Missing(m) => Some(m),
            RenderError::Resolve { .. } => None,
        }
    }
}

impl<E: fmt::Display> fmt::Display for RenderError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RenderError::Missing(m) => m.fmt(f),
            RenderError::Resolve { expr, source, .. } => write!(f, "{{{{{expr}}}}}: {source}"),
        }
    }
}

impl<E: std::error::Error + 'static> std::error::Error for RenderError<E> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            RenderError::Missing(_) => None,
            RenderError::Resolve { source, .. } => Some(source),
        }
    }
}

/// Returns the distinct placeholder expressions in `s`, in order.
pub fn exprs(s: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    RE_PLACEHOLDER
        .captures_iter(s)
        .filter_map(|m| m.get(1))
        .map(|m| m.as_str())
        .filter(|e| seen.insert(*e))
        .map(str::to_string)
        .collect()
}

/// Substitutes every placeholder in `s`. Unknown expressions are left in
/// place and reported together in a [`RenderError::Missing`]; any other
/// resolver error stops rendering immediately.
pub fn render<E, F>(s: &str, mut resolve: F) -> Result<String, RenderError<E>>
where
    F: FnMut(&str) -> Result<Option<String>, E>,
{
    let mut missing: Vec<String> = Vec::new();
    let mut failed: Option<(String, E)> = None;
    let mut out = String::with_capacity(s.len());
    let mut last = 0;
    for m in RE_PLACEHOLDER.find_iter(s) {
        out.push_str(&s[last..m.start()]);
        last = m.end();
        let text = m.as_str();
        let expr = text[2..text.len() - 2].trim();
        match resolve(expr) {
            Ok(Some(value)) => out.push_str(&value),
            Ok(None) => {
                if !missing.iter().any(|e| e == expr) {
                    missing.push(expr.to_string());
                }
                out.push_str(text);
            }
            Err(err) => {
                failed = Some((expr.to_string(), err));
                last = m.start();
                break;
            }
        }
    }
    out.push_str(&s[last..]);
    if let Some((expr, source)) = failed {
        return Err(RenderError::Resolve {
            expr,
            source,
            output: out,
        });
    }
    if !missing.is_empty() {
        return Err(RenderError::Missing(MissingError {
            exprs: missing,
            output: out,
        }));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_cases() {
        let r = |e: &str| match e {
            "boom" => Err("bad".to_string()),
            "a" => Ok(Some("1".to_string())),
            "b" => Ok(Some("2".to_string())),
            _ => Ok(None),
        };
        assert_eq!(render("x{{a}}y{{ b }}z", r).unwrap(), "x1y2z");
        let err = render("{{a}}{{c}}{{d}}{{c}}", r).unwrap_err();
        let me = err.missing().expect("a missing error");
        assert_eq!(me.exprs, ["c", "d"]);
        assert_eq!(me.output, "1{{c}}{{d}}{{c}}");
        assert_eq!(err.output(), "1{{c}}{{d}}{{c}}");
        assert_eq!(err.to_string(), "missing variables: c, d");
        let err = render("{{a}}-{{boom}}-{{b}}", r).unwrap_err();
        assert!(err.missing().is_none());
        assert_eq!(err.to_string(), "{{boom}}: bad");
        assert_eq!(err.output(), "1-{{boom}}-{{b}}");
        assert_eq!(exprs("{{a}} {{b}} {{a}}"), ["a", "b"]);
        assert_eq!(
            exprs("{{ $processEnv HOME }} {x} {{}}"),
            ["$processEnv HOME"]
        );
    }
}
