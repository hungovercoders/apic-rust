//! apic validate: problems in the project, as text, JSON, GitHub
//! annotations or SARIF.

use std::collections::BTreeSet;

use apic_gocompat::quote;
use apic_httpfile::Diagnostic;
use apic_project::Project;
use apic_runner::Code;
use serde::Serialize;
use serde_json::{json, Value};

use crate::io::wln;
use crate::root::{fail, App, CliError, CliResult, Invocation};
use crate::shared::{plural, theme};
use crate::spec::{CommandSpec, FlagSpec};

pub(crate) fn spec() -> CommandSpec {
    CommandSpec::new("validate", "Parse every .http file and report problems (for CI)")
        .long(
            r#"validate parses every .http file, checks selectors, assertions, auth specs,
step phrases, body files and request names, and reports what it finds.
Errors exit 2, so it works as a pull-request gate.

--format picks how diagnostics are printed:
  text    path:line:col: severity: message (code), with the source line
          and a caret under the span when stdout is a terminal
  json    {"ok", "files", "requests", "diagnostics": [...]} (also --json)
  github  GitHub Actions workflow commands, so a failing check annotates
          the pull request at the right line
  sarif   SARIF 2.1.0, for code scanning uploads"#,
        )
        .example(
            r#"  apic validate
  apic validate --format github     # in a GitHub Actions step
  apic validate --format sarif > apic.sarif
  apic validate --json | jq '.diagnostics[] | select(.severity == "error")'"#,
        )
        .flag(FlagSpec::string("format", "text", "output format: text, json, github or sarif").short('f'))
        .runs(run)
}

fn run(a: &mut App, inv: &Invocation) -> CliResult {
    let mut format = inv.flags.string("format");
    if a.g.json {
        format = "json".to_string();
    }
    let p = a.load_project()?;
    let diags = p.validate();
    let errs = diags.iter().filter(|d| d.severity == "error").count();
    match format.as_str() {
        "json" => {
            #[derive(Serialize)]
            struct Out<'a> {
                ok: bool,
                files: usize,
                requests: usize,
                diagnostics: &'a [Diagnostic],
            }
            a.write_json(&Out {
                ok: errs == 0,
                files: p.files.len(),
                requests: p.requests().len(),
                diagnostics: &diags,
            })?;
        }
        "github" => {
            for d in &diags {
                wln!(a.stdout, "{}", github_annotation(d));
            }
            a.validate_summary(&p, &diags, errs);
        }
        "sarif" => a.write_json(&sarif_report(&diags))?,
        "text" => {
            let t = theme();
            let caret = a.stdout.is_terminal();
            for d in &diags {
                let sev = if d.severity == "error" {
                    t.fail.render(&d.severity)
                } else {
                    t.warn.render(&d.severity)
                };
                let mut place = format!("{}:{}", d.path, d.line);
                if d.column > 0 {
                    place.push_str(&format!(":{}", d.column));
                }
                let code = if d.code.is_empty() {
                    String::new()
                } else {
                    format!(" {}", t.dim.render(&format!("({})", d.code)))
                };
                wln!(a.stdout, "{place}: {sev}: {}{code}", d.message);
                if caret {
                    a.stdout.print(&caret_line(&p.root, d));
                }
            }
            a.validate_summary(&p, &diags, errs);
        }
        _ => {
            return Err(fail(
                Code::Flag,
                format!("--format must be text, json, github or sarif, got {}", quote(&format)),
            ))
        }
    }
    if errs > 0 {
        return Err(CliError::exit(apic_runner::EXIT_USAGE));
    }
    Ok(())
}

impl App {
    fn validate_summary(&self, p: &Project, diags: &[Diagnostic], errs: usize) {
        let t = theme();
        let counts = format!(
            "{}, {}",
            plural(p.files.len(), "file"),
            plural(p.requests().len(), "request")
        );
        if diags.is_empty() {
            wln!(self.stdout, "{} {counts}, no problems", t.ok.render("✓"));
        } else if errs == 0 {
            wln!(self.stdout, "{} {counts}, {}", t.warn.render("!"), plural(diags.len(), "warning"));
        } else {
            wln!(
                self.stdout,
                "{} {counts}, {}, {}",
                t.fail.render("✗"),
                plural(errs, "error"),
                plural(diags.len() - errs, "warning")
            );
        }
    }
}

/// Renders the source line a diagnostic points at with a caret under its
/// span, for terminals. It returns "" when the file cannot be read or the
/// diagnostic has no span.
pub(crate) fn caret_line(root: &str, d: &Diagnostic) -> String {
    if d.column == 0 || d.line == 0 {
        return String::new();
    }
    let Ok(data) = crate::shared::read_text(apic_project::path::join(root, &d.path)) else {
        return String::new();
    };
    let data = data.replace("\r\n", "\n");
    let lines: Vec<&str> = data.split('\n').collect();
    if d.line > lines.len() {
        return String::new();
    }
    let src = lines[d.line - 1];
    let mut width = d.end_column.saturating_sub(d.column);
    if d.end_line != d.line || width < 1 || d.column - 1 > src.len() {
        width = 1;
    }
    let t = theme();
    let pad = " ".repeat(d.column - 1);
    format!("  {}\n  {pad}{}\n", t.dim.render(src), t.accent.render(&"^".repeat(width)))
}

/// Renders a diagnostic as a GitHub Actions workflow command, which the
/// runner turns into an annotation on the pull request.
pub(crate) fn github_annotation(d: &Diagnostic) -> String {
    let kind = if d.severity == "error" { "error" } else { "warning" };
    let mut props = vec![format!("file={}", gh_escape_prop(&d.path))];
    if d.line > 0 {
        props.push(format!("line={}", d.line));
        if d.column > 0 {
            props.push(format!("col={}", d.column));
            if d.end_line >= d.line && d.end_column > 0 {
                props.push(format!("endLine={}", d.end_line));
                props.push(format!("endColumn={}", d.end_column));
            }
        }
    }
    if !d.code.is_empty() {
        props.push(format!("title={}", gh_escape_prop(&format!("apic: {}", d.code))));
    }
    format!("::{kind} {}::{}", props.join(","), gh_escape_data(&d.message))
}

// The escaping rules for workflow commands: data escapes %, \r and \n;
// properties additionally escape : and ,.
fn gh_escape_data(s: &str) -> String {
    s.replace('%', "%25").replace('\r', "%0D").replace('\n', "%0A")
}

fn gh_escape_prop(s: &str) -> String {
    gh_escape_data(s).replace(':', "%3A").replace(',', "%2C")
}

/// Renders diagnostics as a SARIF 2.1.0 log with one rule per code, the
/// shape GitHub code scanning and most IDEs accept.
pub(crate) fn sarif_report(diags: &[Diagnostic]) -> Value {
    let codes: BTreeSet<&str> = diags.iter().map(|d| d.code.as_str()).filter(|c| !c.is_empty()).collect();
    let rules: Vec<Value> = codes
        .iter()
        .map(|c| {
            json!({
                "id": c,
                "shortDescription": {"text": apic_httpfile::code_description(c).unwrap_or("")},
                "helpUri": "https://apic.sh/cli/#apic-validate",
            })
        })
        .collect();
    let results: Vec<Value> = diags
        .iter()
        .map(|d| {
            let level = if d.severity == "error" { "error" } else { "warning" };
            let mut region = serde_json::Map::new();
            if d.line > 0 {
                region.insert("startLine".into(), json!(d.line));
                if d.column > 0 {
                    region.insert("startColumn".into(), json!(d.column));
                    if d.end_line >= d.line && d.end_column > 0 {
                        region.insert("endLine".into(), json!(d.end_line));
                        region.insert("endColumn".into(), json!(d.end_column));
                    }
                }
            } else {
                region.insert("startLine".into(), json!(1));
            }
            let mut res = json!({
                "level": level,
                "message": {"text": d.message},
                "locations": [{
                    "physicalLocation": {
                        "artifactLocation": {"uri": d.path, "uriBaseId": "%SRCROOT%"},
                        "region": region,
                    },
                }],
            });
            if !d.code.is_empty() {
                res["ruleId"] = json!(d.code);
            }
            res
        })
        .collect();
    json!({
        "$schema": "https://json.schemastore.org/sarif-2.1.0.json",
        "version": "2.1.0",
        "runs": [{
            "tool": {"driver": {
                "name": "apic",
                "version": crate::version(),
                "informationUri": "https://apic.sh/",
                "rules": rules,
            }},
            "results": results,
        }],
    })
}
