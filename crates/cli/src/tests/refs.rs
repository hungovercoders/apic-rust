use apic_httpd::{Mux, Response, TestServer};
use serde_json::Value;
use tempfile::TempDir;

use super::{execute, join, must_write, tmp};

/// Serves a login and a /me that needs its token, and writes a project
/// where whoami declares `# @ref login`.
fn ref_project() -> (TempDir, TestServer) {
    let mut mux = Mux::new();
    mux.handle_fn("POST /login", |_| Response::json(200, r#"{"token":"t-1"}"#))
        .unwrap();
    mux.handle_fn("GET /me", |r| {
        if r.header("Authorization") != Some("Bearer t-1") {
            return Response::error(401, r#"{"error":"unauthorised"}"#);
        }
        Response::json(200, r#"{"name":"alice"}"#)
    })
    .unwrap();
    mux.handle_fn("GET /broken", |_| {
        Response::error(503, r#"{"error":"down"}"#)
    })
    .unwrap();
    let srv = TestServer::new(mux.into_handler());
    let dir = tmp();
    must_write(
        join(dir.path(), "api.http"),
        "
### login
# @name login
# @assert status == 200
# @capture token = body.$.token
POST {{baseUrl}}/login

### whoami
# @name whoami
# @ref login
# @assert status == 200
GET {{baseUrl}}/me
Authorization: Bearer {{token}}

### broken
# @name broken
# @assert status == 200
# @capture other = body.$.token
GET {{baseUrl}}/broken

### needs-broken
# @name needs-broken
# @ref broken
GET {{baseUrl}}/me
Authorization: Bearer {{other}}
",
    );
    must_write(
        join(dir.path(), "http-client.env.json"),
        &format!(r#"{{"dev":{{"baseUrl":"{}"}}}}"#, srv.url()),
    );
    (dir, srv)
}

const FEATURE: &str =
    "Feature: Login\n  Scenario: It works\n    When I run \"login\"\n    Then the response status is 200\n";

#[test]
fn run_prints_what_ref_ran_first() {
    let (tmp, _srv) = ref_project();
    let dir = tmp.path().to_str().unwrap();
    let (code, out, errb) = execute(&[
        "run",
        "whoami",
        "-C",
        dir,
        "--env",
        "dev",
        "--no-session",
        "--no-color",
    ]);
    assert_eq!(code, 0, "out={out} err={errb}");
    let login = out.find("POST ");
    let note = out.find("↳ ran login first (# @ref)");
    let me = out.find("GET ");
    assert!(
        login.is_some() && note.is_some() && me.is_some() && login < note && note < me,
        "login's report, the note, then whoami's, got:\n{out}"
    );
    assert_eq!(
        out.matches("200 OK").count(),
        2,
        "both requests should report 200:\n{out}"
    );
}

#[test]
fn run_json_flattens_what_ref_ran_first() {
    let (tmp, _srv) = ref_project();
    let dir = tmp.path().to_str().unwrap();
    let (code, out, errb) = execute(&[
        "run",
        "whoami",
        "-C",
        dir,
        "--env",
        "dev",
        "--no-session",
        "--json",
    ]);
    assert_eq!(code, 0, "out={out} err={errb}");
    let lines: Vec<&str> = out.trim().split('\n').collect();
    assert_eq!(lines.len(), 2, "want one object per request sent:\n{out}");
    let mut names = Vec::new();
    for l in lines {
        let obj: Value = serde_json::from_str(l).unwrap_or_else(|e| panic!("{e} in {l}"));
        assert!(
            obj["ok"] == true && obj.get("ran_first").is_none(),
            "each line is its own flat result: {l}"
        );
        names.push(obj["request"]["name"].as_str().unwrap_or("").to_string());
    }
    assert_eq!(names.join(","), "login,whoami");
}

#[test]
fn run_fails_when_a_ref_fails() {
    let (tmp, _srv) = ref_project();
    let dir = tmp.path().to_str().unwrap();
    let (code, out, _) = execute(&[
        "run",
        "needs-broken",
        "-C",
        dir,
        "--env",
        "dev",
        "--no-session",
        "--no-color",
    ]);
    assert_eq!(
        code, 1,
        "a failed dependency is a failed run (exit 1):\n{out}"
    );
    for want in ["503", "↳ ran broken first (# @ref)", "✗ @ref broken failed"] {
        assert!(out.contains(want), "missing {want:?} in:\n{out}");
    }
    assert!(
        !out.contains("200 OK"),
        "needs-broken should not have been sent:\n{out}"
    );
}

#[test]
fn list_and_describe_report_refs() {
    let (tmp, _srv) = ref_project();
    let dir = tmp.path().to_str().unwrap();
    let (_, out, _) = execute(&["list", "-C", dir, "--env", "dev", "--json"]);
    assert!(
        out.contains(r#""refs": ["#) && out.contains(r#""login""#),
        "list --json should carry refs:\n{out}"
    );
    let (_, out, _) = execute(&[
        "describe",
        "whoami",
        "-C",
        dir,
        "--env",
        "dev",
        "--no-session",
        "--json",
    ]);
    assert!(
        out.contains(r#""refs": ["#)
            && out.contains(r#""ref_runs": true"#)
            && out.contains(r#""ready": true"#),
        "describe --json should say login runs first and the request is ready:\n{out}"
    );
    let (_, out, _) = execute(&[
        "describe",
        "whoami",
        "-C",
        dir,
        "--env",
        "dev",
        "--no-session",
        "--no-color",
    ]);
    assert!(
        out.contains("captured by login, which # @ref runs first"),
        "describe should say login runs first:\n{out}"
    );
}

#[test]
fn validate_reports_bad_refs() {
    let tmp = tmp();
    let dir = tmp.path().to_str().unwrap();
    must_write(
        join(dir, "api.http"),
        "### a\n# @name a\n# @ref nope\n# @ref b\nGET http://x\n\n### b\n# @name b\n# @ref a\nGET http://x\n",
    );
    let (code, out, _) = execute(&["validate", "-C", dir, "--no-color"]);
    assert_ne!(code, 0, "validate should fail:\n{out}");
    for want in [
        "api.http:3:8: error: @ref nope: no request named \"nope\"",
        "(bad-ref)",
        "api.http:4:8: error: @ref b is a cycle: a -> b -> a",
        "(ref-cycle)",
        "api.http:9:8: error: @ref a is a cycle: b -> a -> b",
    ] {
        assert!(out.contains(want), "missing {want:?} in:\n{out}");
    }
}

#[test]
fn run_verbose_shows_timings() {
    let (tmp, _srv) = ref_project();
    let dir = tmp.path().to_str().unwrap();
    let (code, out, errb) = execute(&[
        "run",
        "login",
        "-C",
        dir,
        "--env",
        "dev",
        "--no-session",
        "--no-color",
        "-v",
    ]);
    assert_eq!(code, 0, "out={out} err={errb}");
    assert!(
        out.contains("dns 0 ms · connect ") && out.contains(" · new connection\n"),
        "no timings line in verbose output:\n{out}"
    );
    let (_, out, _) = execute(&["run", "login", "-C", dir, "--env", "dev", "--no-session"]);
    assert!(!out.contains("dns 0 ms"), "timings should need -v:\n{out}");
    let (_, out, _) = execute(&[
        "run",
        "login",
        "-C",
        dir,
        "--env",
        "dev",
        "--no-session",
        "--json",
    ]);
    assert!(
        out.contains(r#""timings":{"#) && out.contains(r#""reused":false"#),
        "json should carry timings:\n{out}"
    );
}

#[test]
fn html_reports() {
    let (tmp, _srv) = ref_project();
    let dir = tmp.path().to_str().unwrap();
    must_write(join(dir, "features/login.feature"), FEATURE);
    // apic test --format html writes the report to --output and keeps
    // the exit code.
    let elsewhere = super::tmp();
    let out = join(elsewhere.path(), "features.html");
    let (code, stdout, stderr) = execute(&[
        "test", "-C", dir, "--env", "dev", "--format", "html", "--output", &out,
    ]);
    assert!(
        code == 0 && stdout.is_empty(),
        "test html: code={code} out={stdout:?} err={stderr}"
    );
    let html = std::fs::read_to_string(&out).unwrap();
    for want in [
        "<!doctype html>",
        "all passed",
        "It works",
        "I run &#34;login&#34;",
        "environment <b>dev</b>",
    ] {
        assert!(html.contains(want), "features report lacks {want:?}");
    }
    // Without --output the report goes to stdout.
    let (code, stdout, _) = execute(&["test", "-C", dir, "--env", "dev", "--format", "html"]);
    assert!(
        code == 0 && stdout.starts_with("<!doctype html>"),
        "test html to stdout: code={code}"
    );
    // apic run --report writes the report beside the usual output.
    let report = join(elsewhere.path(), "run.html");
    let (code, stdout, stderr) = execute(&[
        "run",
        "whoami",
        "-C",
        dir,
        "--env",
        "dev",
        "--no-session",
        "--json",
        "--report",
        &report,
    ]);
    assert!(
        code == 0 && stdout.contains(r#""ok":true"#),
        "run --report: code={code} out={stdout} err={stderr}"
    );
    let html = std::fs::read_to_string(&report).unwrap();
    for want in [
        "<!doctype html>",
        "ran first (# @ref)",
        "whoami",
        "all passed",
    ] {
        assert!(html.contains(want), "run report lacks {want:?}");
    }
    // A project file is never a report path.
    let (code, _, stderr) = execute(&[
        "run",
        "login",
        "-C",
        dir,
        "--env",
        "dev",
        "--no-session",
        "--report",
        &join(dir, "api.http"),
    ]);
    assert!(
        code == 2 && stderr.contains("would overwrite"),
        "--report onto a request file: code={code} err={stderr}"
    );
    // A failed request still gets its report, and the exit code says so.
    let (code, _, _) = execute(&[
        "run",
        "broken",
        "-C",
        dir,
        "--env",
        "dev",
        "--no-session",
        "--report",
        &report,
    ]);
    let html = std::fs::read_to_string(&report).unwrap();
    assert!(
        code == 1 && html.contains("1 failed"),
        "failed run report: code={code}"
    );
}

#[test]
fn test_command_honours_proxy_flags() {
    let (tmp, _srv) = ref_project();
    let dir = tmp.path().to_str().unwrap();
    must_write(join(dir, "features/login.feature"), FEATURE);
    // An unreachable proxy for a host the environment would not skip.
    let (code, _, stderr) = execute(&[
        "test",
        "-C",
        dir,
        "--env",
        "dev",
        "--proxy",
        "http://127.0.0.1:1",
    ]);
    assert_eq!(code, 3, "test through a dead proxy: err={stderr}");
    let (code, _, stderr) = execute(&[
        "test",
        "-C",
        dir,
        "--env",
        "dev",
        "--proxy",
        "http://127.0.0.1:1",
        "--no-proxy",
    ]);
    assert_eq!(code, 0, "test --no-proxy: err={stderr}");
}
