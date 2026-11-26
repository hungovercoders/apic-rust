//! apic list: every request in the project, filtered by a pattern.

use std::collections::BTreeSet;
use std::sync::LazyLock;

use apic_gocompat::quote;
use apic_httpfile::Request;
use regex::Regex;
use serde::Serialize;

use crate::io::{w, wln};
use crate::root::{App, CliError, CliResult, Invocation};
use crate::shared::{plural, theme};
use crate::spec::{ArgRule, CommandSpec};

#[derive(Serialize, Default)]
struct ListEntry {
    id: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    name: String,
    method: String,
    url: String,
    file: String,
    line: usize,
    #[serde(skip_serializing_if = "String::is_empty")]
    description: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    captures: Vec<String>,
    #[serde(skip_serializing_if = "is_zero")]
    asserts: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    steps: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    refs: Vec<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    disabled: bool,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

impl ListEntry {
    fn matches(&self, pattern: &str) -> bool {
        let p = pattern.to_lowercase();
        self.id.to_lowercase().contains(&p)
            || self.url.to_lowercase().contains(&p)
            || self.description.to_lowercase().contains(&p)
            || self.file.to_lowercase().contains(&p)
    }
}

pub(crate) fn spec() -> CommandSpec {
    CommandSpec::new("list [pattern]", "List every request in the project")
        .long(
            "List the requests apic found, grouped by file. A pattern narrows the list
to requests whose id, URL, file or description contains it.",
        )
        .example(
            "  apic list
  apic list todo
  apic list --json | jq '.requests[].id'",
        )
        .args(ArgRule::Max(1))
        .runs(run)
}

fn run(a: &mut App, inv: &Invocation) -> CliResult {
    let args = &inv.args;
    let p = a.load_project()?;
    let mut entries = Vec::new();
    for r in p.requests() {
        let e = ListEntry {
            id: r.id(),
            name: r.name.clone(),
            method: r.method.clone(),
            url: r.url.clone(),
            file: r.file.path.clone(),
            line: r.line,
            description: r.description.clone(),
            captures: r.captures.iter().map(|c| c.name.clone()).collect(),
            asserts: r.asserts.len(),
            steps: r.steps(),
            refs: ref_ids(&r),
            disabled: r.disabled(),
        };
        if args.len() == 1 && !e.matches(&args[0]) {
            continue;
        }
        entries.push(e);
    }
    if a.g.json {
        #[derive(Serialize)]
        struct Listing<'a> {
            root: &'a str,
            requests: &'a [ListEntry],
            #[serde(skip_serializing_if = "str::is_empty")]
            hint: &'a str,
        }
        let hint = if p.files.is_empty() {
            NEW_HERE_HINT
        } else {
            ""
        };
        return a.write_json(&Listing {
            root: &p.root,
            requests: &entries,
            hint,
        });
    }
    if entries.is_empty() {
        if args.len() == 1 {
            wln!(
                a.stdout,
                "no requests match {} (run `apic list` to see them all)",
                quote(&args[0])
            );
            return Ok(());
        }
        wln!(a.stdout, "no .http files found under {}", p.root);
        if p.files.is_empty() {
            wln!(a.stdout, "{NEW_HERE_HINT}");
        }
        return Ok(());
    }
    let t = theme();
    let files: BTreeSet<&str> = entries.iter().map(|e| e.file.as_str()).collect();
    let has_steps = entries.iter().any(|e| !e.steps.is_empty());
    let mut cols = vec!["ID", "METHOD", "URL", "LINE", "DESCRIPTION"];
    if files.len() == 1 {
        cols[3] = "FILE";
    }
    if has_steps {
        cols.push("PHRASES");
    }
    let mut rows: Vec<Vec<String>> = vec![cols.iter().map(|c| t.bold.render(c)).collect()];
    let mut last_file = "";
    for e in &entries {
        if files.len() > 1 && e.file != last_file {
            rows.push(vec![t.accent.render(&e.file)]);
            last_file = &e.file;
        }
        let place = if files.len() > 1 {
            format!(":{}", e.line)
        } else {
            format!("{}:{}", e.file, e.line)
        };
        let (id, desc) = if e.disabled {
            (
                t.dim.render(&e.id),
                format!("{} {}", t.dim.render("(disabled)"), e.description)
                    .trim()
                    .to_string(),
            )
        } else {
            (t.bold.render(&e.id), e.description.clone())
        };
        let mut row = vec![
            id,
            t.method(&e.method),
            t.url.render(&e.url),
            t.dim.render(&place),
            desc,
        ];
        if has_steps {
            row.push(t.dim.render(&e.steps.join(" | ")));
        }
        rows.push(row);
    }
    a.stdout
        .write_text(&table(&rows))
        .map_err(CliError::other)?;
    w!(
        a.stdout,
        "\n{}\n",
        t.dim.render(&format!(
            "{} in {} · apic describe <id> · apic run <id> · apic ui",
            plural(entries.len(), "request"),
            plural(files.len(), "file")
        ))
    );
    Ok(())
}

/// Lists the targets of a request's `# @ref` and `# @forceRef` directives.
fn ref_ids(r: &Request) -> Vec<String> {
    r.refs().into_iter().map(|r| r.id).collect()
}

/// What a directory with no request files gets told: the two commands
/// that give it some. The project's not-found error says the same.
pub(crate) const NEW_HERE_HINT: &str =
    "new here? apic init scaffolds a project, and apic ui --demo shows a working one";

/// A colour or style sequence the theme writes.
static ANSI_SEQ: LazyLock<Regex> =
    LazyLock::new(|| Regex::new("\x1b\\[[0-9;]*m").expect("valid pattern"));

/// Lays rows out in columns two spaces apart, sized by what shows on the
/// terminal rather than by the bytes, so a bold header lines up with the
/// coloured rows beneath it. A short row (a file heading) takes part in
/// the sizing without padding, and no line carries trailing spaces, which
/// would wrap in a terminal only just wide enough.
pub(crate) fn table(rows: &[Vec<String>]) -> String {
    let mut widths: Vec<usize> = Vec::new();
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            if i >= widths.len() {
                widths.push(0);
            }
            widths[i] = widths[i].max(shown(cell));
        }
    }
    let mut b = String::new();
    for row in rows {
        // A trailing cell that shows nothing (an empty phrase, styled)
        // would keep the padding before it on the line.
        let mut last = row.len().saturating_sub(1);
        while last > 0 && shown(&row[last]) == 0 {
            last -= 1;
        }
        for (i, cell) in row.iter().take(last + 1).enumerate() {
            b.push_str(cell);
            if i < last {
                b.push_str(&" ".repeat(widths[i] - shown(cell) + 2));
            }
        }
        b.push('\n');
    }
    b
}

/// How many cells of the terminal a styled string takes.
fn shown(s: &str) -> usize {
    ANSI_SEQ.replace_all(s, "").chars().count()
}
