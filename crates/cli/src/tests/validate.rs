use apic_httpc::CancelToken;
use apic_httpfile::Diagnostic;
use serde_json::Value;

use super::{execute, execute_with, join, must_read, must_write, s, tmp};
use crate::validate::{caret_line, github_annotation};

/// Writes a project with one warning and one error and runs `apic
/// validate` on it with the given extra flags.
fn validate_project(flags: &[&str]) -> (i32, String, String) {
    let dir = tmp();
    must_write(
        join(dir.path(), "api.http"),
        "### a\n# @name a\n# @frobnicate yes\n# @assert bogus == 1\nGET https://example.com\n",
    );
    let path = s(&dir);
    execute(&[&["validate", "-C", path.as_str()], flags].concat())
}

#[test]
fn validate_text_shows_columns_and_codes() {
    let (code, out, _) = validate_project(&[]);
    assert_eq!(code, 2, "{out}");
    for want in [
        "api.http:3:3: warning: unknown directive @frobnicate (ignored) (unknown-directive)",
        "api.http:4:11: error: assert \"bogus == 1\": unknown selector \"bogus\" (unknown-selector)",
        "1 file, 1 request, 1 error, 1 warning",
    ] {
        assert!(out.contains(want), "missing {want:?} in:\n{out}");
    }
    // Stdout is a buffer, not a terminal, so no caret lines.
    assert!(
        !out.contains('^'),
        "caret line printed without a terminal:\n{out}"
    );
}

#[test]
fn validate_json_carries_spans() {
    let (code, out, _) = validate_project(&["--json"]);
    assert_eq!(code, 2, "{out}");
    let got: Value = serde_json::from_str(&out).unwrap();
    let diags = got["diagnostics"].as_array().unwrap();
    assert!(got["ok"] == false && diags.len() == 2, "got {got}");
    let d = &diags[1];
    assert!(
        d["path"] == "api.http"
            && d["line"] == 4
            && d["column"] == 11
            && d["end_line"] == 4
            && d["end_column"] == 16
            && d["code"] == "unknown-selector"
            && d["severity"] == "error",
        "diagnostic = {d}"
    );
}

#[test]
fn validate_github_format() {
    let (code, out, _) = validate_project(&["--format", "github"]);
    assert_eq!(code, 2, "{out}");
    for want in [
        "::warning file=api.http,line=3,col=3,endLine=3,endColumn=14,title=apic%3A unknown-directive::unknown directive @frobnicate (ignored)",
        "::error file=api.http,line=4,col=11,endLine=4,endColumn=16,title=apic%3A unknown-selector::assert \"bogus == 1\": unknown selector \"bogus\"",
    ] {
        assert!(out.contains(want), "missing {want:?} in:\n{out}");
    }
}

#[test]
fn validate_sarif_format() {
    let (code, out, _) = validate_project(&["--format", "sarif"]);
    assert_eq!(code, 2, "{out}");
    let got: Value = serde_json::from_str(&out).unwrap();
    let runs = got["runs"].as_array().unwrap();
    assert!(
        got["version"] == "2.1.0" && runs.len() == 1 && runs[0]["tool"]["driver"]["name"] == "apic",
        "header: {got}"
    );
    let run = &runs[0];
    let rules = run["tool"]["driver"]["rules"].as_array().unwrap();
    assert!(
        rules.len() == 2
            && rules[0]["id"] == "unknown-directive"
            && rules[1]["id"] == "unknown-selector",
        "rules: {rules:?}"
    );
    let results = run["results"].as_array().unwrap();
    assert_eq!(results.len(), 2, "results: {results:?}");
    let r = &results[1];
    let loc = &r["locations"][0]["physicalLocation"];
    assert!(
        r["ruleId"] == "unknown-selector"
            && r["level"] == "error"
            && loc["artifactLocation"]["uri"] == "api.http"
            && loc["region"]["startLine"] == 4
            && loc["region"]["startColumn"] == 11
            && loc["region"]["endLine"] == 4
            && loc["region"]["endColumn"] == 16,
        "result: {r}"
    );
}

#[test]
fn validate_rejects_unknown_format() {
    let (code, _, stderr) = validate_project(&["--format", "xml"]);
    assert!(
        code == 2 && stderr.contains("--format must be text, json, github or sarif"),
        "code={code} stderr={stderr}"
    );
}

fn diag_for(path: &str, msg: &str) -> Diagnostic {
    Diagnostic {
        path: path.to_string(),
        line: 1,
        severity: "error".to_string(),
        code: "t".to_string(),
        message: msg.to_string(),
        ..Diagnostic::default()
    }
}

#[test]
fn github_annotation_escapes() {
    let got = github_annotation(&diag_for("a,b:c.http", "50% done\nnext"));
    assert_eq!(
        got,
        "::error file=a%2Cb%3Ac.http,line=1,title=apic%3A t::50%25 done%0Anext"
    );
}

#[test]
fn caret_line_underlines_the_span() {
    let tmp = tmp();
    let dir = s(&tmp);
    must_write(
        join(&dir, "api.http"),
        "# @assert bogus == 1\nGET https://x\n",
    );
    let mut d = diag_for("api.http", "m");
    (d.column, d.end_line, d.end_column) = (11, 1, 16);
    let got = apic_output::style::with_color_profile(apic_output::style::Profile::Ascii, || {
        caret_line(&dir, &d)
    });
    assert!(
        got.contains("# @assert bogus == 1")
            && got.ends_with(&format!("{}^^^^^\n", " ".repeat(10))),
        "got {got:?}"
    );
    assert_eq!(
        caret_line(&dir, &diag_for("api.http", "m")),
        "",
        "a diagnostic without a span has no caret"
    );
}

#[test]
fn fmt_command() {
    let tmp = tmp();
    let dir = s(&tmp);
    let messy = "### a\n# @assert status == 200\n# @name a\nGET http://x  \ncontent-type: application/json\n\n\n### b\n# @name b\nPOST http://x\n\n{\"k\":1}\n";
    let canonical = "### a\n# @name a\n# @assert status == 200\nGET http://x\nContent-Type: application/json\n\n### b\n# @name b\nPOST http://x\n\n{\n  \"k\": 1\n}\n";
    must_write(join(&dir, "api.http"), messy);
    must_write(join(&dir, "sub/other.http"), "GET http://y\n");
    let exec = |stdin: &str, args: &[&str]| {
        let all = [&["-C", dir.as_str()], args].concat();
        let (code, out, err) = execute_with(&CancelToken::new(), Some(stdin), &all);
        (out + &err, code)
    };
    // --check lists what would change and exits 1 without writing.
    let (out, code) = exec("", &["fmt", "--check"]);
    assert!(
        code == 1 && out == "api.http\n",
        "check: code={code} out={out:?}"
    );
    assert_eq!(
        must_read(join(&dir, "api.http")),
        messy,
        "--check must not write"
    );
    // --diff shows the change as a unified diff.
    let (out, code) = exec("", &["fmt", "--diff"]);
    assert!(
        code == 1
            && out.starts_with("--- api.http\n+++ api.http\n@@ -1,12 +1,13 @@\n ### a\n")
            && out.contains("-GET http://x  \n")
            && out.contains("+GET http://x\n")
            && out.contains("-{\"k\":1}\n+{\n+  \"k\": 1\n+}\n"),
        "diff: code={code} out={out}"
    );
    // stdin to stdout.
    let (out, code) = exec(messy, &["fmt", "-"]);
    assert!(
        code == 0 && out == canonical,
        "stdin: code={code} out={out:?}"
    );
    // A missing final newline is a change the diff shows, with diff -u's
    // marker.
    must_write(join(&dir, "sub/other.http"), "GET http://y");
    let (out, code) = exec("", &["fmt", "--diff", "sub"]);
    assert!(
        code == 1
            && out.contains(
                "@@ -1,1 +1,1 @@\n-GET http://y\n\\ No newline at end of file\n+GET http://y\n"
            ),
        "no-newline diff: code={code} out={out}"
    );
    must_write(join(&dir, "sub/other.http"), "GET http://y\n");
    // In place, with the JSON summary as the only thing on stdout.
    let (out, code) = exec("", &["--json", "fmt"]);
    let summary: Value =
        serde_json::from_str(&out).unwrap_or_else(|e| panic!("json: not one object: {e}\n{out}"));
    let changed = summary["changed"].as_array().unwrap();
    assert!(
        code == 0
            && summary["files"] == 2
            && changed.len() == 1
            && changed[0] == "api.http"
            && summary["formatted"] == true,
        "json: code={code} out={out}"
    );
    assert_eq!(must_read(join(&dir, "api.http")), canonical, "written");
    let (out, code) = exec("", &["fmt"]);
    assert!(
        code == 0 && out == "2 files already formatted\n",
        "second run: code={code} out={out:?}"
    );
    // Named paths, files and directories.
    must_write(join(&dir, "sub/other.http"), "GET http://y  \n");
    let (out, code) = exec("", &["fmt", "sub", "api.http"]);
    assert!(
        code == 0 && out == "formatted sub/other.http\n",
        "paths: code={code} out={out:?}"
    );
    let (_, code) = exec("", &["fmt", "nope.http"]);
    assert_eq!(code, 2, "missing path");
}
