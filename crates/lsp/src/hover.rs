//! Hover: what a `{{placeholder}}` stands for.

use std::sync::LazyLock;

use apic_runner::{VarInfo, MASKED};
use regex::Regex;

use crate::complete::BUILTINS;
use crate::protocol::{
    lines, uri_to_path, Hover, MarkupContent, Position, Range, TextDocumentPositionParams,
};
use crate::server::{file_of, request_at, Handled, Reply, Server};

static PH_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\{\{[\t\n\f\r ]*([^{}]*?)[\t\n\f\r ]*\}\}").expect("a valid pattern")
});

impl Server {
    /// Describes the `{{placeholder}}` under the cursor: its value
    /// (masked when secret) and source, or for a missing one the request
    /// that captures it, as `apic describe` reports them for the request
    /// the cursor is in; outside a request, as `apic env` does.
    pub(crate) fn hover(&self, p: &TextDocumentPositionParams) -> Handled {
        let Some(path) = uri_to_path(&p.uri) else {
            return Ok(Reply::Null);
        };
        let mut st = self.lock();
        let text = st.text(&path);
        let ls = lines(&text);
        if p.position.line < 0 || p.position.line >= ls.len() as i64 {
            return Ok(Reply::Null);
        }
        let line = ls[p.position.line as usize];
        let col = st.units.to_byte(line, p.position.character);
        let found = PH_RE.captures_iter(line).find_map(|m| {
            let (whole, inner) = (m.get(0)?, m.get(1)?);
            (col >= whole.start() && col <= whole.end())
                .then(|| (inner.as_str(), whole.start(), whole.end()))
        });
        let Some((inner, start, end)) = found else {
            return Ok(Reply::Null);
        };
        let ps = st.project_for(&path);
        let mut vars: Vec<VarInfo> = Vec::new();
        let mut in_request = false;
        if let Ok(r) = st.runner_for(&ps) {
            if let Some(req) = request_at(file_of(&ps, &path), &text, p.position.line) {
                vars = r.describe(&req).variables;
                in_request = true;
            } else {
                vars = r.env_vars();
            }
        }
        let md = hover_text(inner, &vars, in_request);
        if md.is_empty() {
            return Ok(Reply::Null);
        }
        Ok(Reply::of(&Hover {
            contents: MarkupContent {
                kind: "markdown".to_string(),
                value: md,
            },
            range: Some(Range {
                start: Position {
                    line: p.position.line,
                    character: st.units.to_client(line, start),
                },
                end: Position {
                    line: p.position.line,
                    character: st.units.to_client(line, end),
                },
            }),
        }))
    }
}

static REF_HOVER_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^([0-9A-Za-z_-]+)\.response\.(body|headers)(.*)$").expect("a valid pattern")
});

/// The Markdown for a placeholder's inner text, or "" when there is
/// nothing to say. `vars` are the request's variables (`in_request`) or
/// the environment's.
pub(crate) fn hover_text(inner: &str, vars: &[VarInfo], in_request: bool) -> String {
    let Some(name) = inner.split_whitespace().next() else {
        return String::new();
    };
    if name.starts_with('$') {
        let base = name.split('(').next().unwrap_or(name);
        for b in BUILTINS {
            if b.name == base || (b.name == "$env" && name.starts_with("$env.")) {
                return format!("**{name}** · built-in\n\n{}", b.doc);
            }
        }
        return format!("**{name}** · unknown built-in");
    }
    if let Some(m) = REF_HOVER_RE.captures(name) {
        let what = if &m[2] == "headers" { "header" } else { "body" };
        return format!(
            "**{name}** · response reference\n\nThe {what} of `{}` from earlier in the same run.",
            &m[1]
        );
    }
    const UNSET: &str = "No source defines it. Add it to an env file, `.env` or `--var`.";
    for v in vars {
        if v.name != name {
            continue;
        }
        if v.missing {
            if !v.captured_by.is_empty() && v.ref_runs {
                return format!(
                    "**{name}** · not set\n\nCaptured by `{}`, which `# @ref` runs first.",
                    v.captured_by
                );
            }
            if !v.captured_by.is_empty() {
                return format!(
                    "**{name}** · not set\n\nCaptured by `{0}`: run it first, or add `# @ref {0}`.",
                    v.captured_by
                );
            }
            return format!("**{name}** · not set\n\n{UNSET}");
        }
        let (value, note) = if v.secret {
            (MASKED, " (secret, masked)")
        } else {
            (v.value.as_str(), "")
        };
        return format!("**{name}** = `{value}`\n\nfrom {}{note}", v.source);
    }
    if in_request {
        return format!("**{name}** · not set\n\n{UNSET}");
    }
    String::new()
}
