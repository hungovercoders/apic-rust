use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Arc;

use apic_httpd::{Response, TestServer};
use tempfile::TempDir;

use super::{execute, join, must_write, server, tmp};

/// Serves a job that is running for two polls and done after.
fn retry_project() -> (TempDir, TestServer) {
    let polls = Arc::new(AtomicI32::new(0));
    let srv = server(move |r| {
        if r.path != "/job" {
            return Response::json(200, r#"{"ok":true}"#);
        }
        if polls.fetch_add(1, Ordering::SeqCst) + 1 >= 3 {
            return Response::json(200, r#"{"state":"done"}"#);
        }
        Response::json(200, r#"{"state":"running"}"#)
    });
    let dir = tmp();
    must_write(
        join(dir.path(), "api.http"),
        "
### submit
# @name submit
# @assert status == 200
GET {{baseUrl}}/submit

### wait
# @name wait
# @retry 5 1ms
# @assert status == 200
# @assert body.$.state == done
GET {{baseUrl}}/job

### plain
# @name plain
# @assert body.$.state == done
GET {{baseUrl}}/job
",
    );
    must_write(
        join(dir.path(), "http-client.env.json"),
        &format!(r#"{{"dev":{{"baseUrl":"{}"}}}}"#, srv.url()),
    );
    (dir, srv)
}

#[test]
fn run_prints_attempts_as_they_happen() {
    let (tmp, _srv) = retry_project();
    let dir = tmp.path().to_str().unwrap();
    let (code, out, errb) = execute(&[
        "run",
        "submit",
        "wait",
        "-C",
        dir,
        "--env",
        "dev",
        "--no-session",
        "--no-color",
    ]);
    assert_eq!(code, 0, "out={out} err={errb}");
    // submit's report, then wait's attempt lines, then wait's report with
    // the attempt count.
    let submit = out.find("/submit");
    let first = out.find(r#"attempt 1/5 · body.$.state == done: got "running""#);
    let second = out.find("attempt 2/5");
    let report = out.find("200 OK · ").unwrap_or(0);
    let report = out[report + 1..].find("200 OK · ").map(|i| i + report + 1);
    assert!(
        submit.is_some()
            && first.is_some()
            && second.is_some()
            && submit < first
            && first < second
            && second < report,
        "order wrong:\n{out}"
    );
    assert!(
        out.contains("· 3 attempts") && out.matches("attempt ").count() == 2,
        "the report should say 3 attempts after two attempt lines:\n{out}"
    );
}

#[test]
fn run_json_carries_attempts_and_no_progress() {
    let (tmp, _srv) = retry_project();
    let dir = tmp.path().to_str().unwrap();
    let (code, out, errb) = execute(&[
        "run",
        "wait",
        "-C",
        dir,
        "--env",
        "dev",
        "--no-session",
        "--json",
    ]);
    assert!(
        code == 0 && out.contains(r#""attempts":3"#) && !out.contains("attempt 1/5"),
        "code={code} out={out} err={errb}"
    );
}

#[test]
fn retry_flags() {
    let (tmp, _srv) = retry_project();
    let dir = tmp.path().to_str().unwrap();
    // --retry applies to a request without its own policy.
    let (code, out, _) = execute(&[
        "run",
        "plain",
        "-C",
        dir,
        "--env",
        "dev",
        "--no-session",
        "--json",
        "--retry",
        "4 1ms",
    ]);
    assert!(
        code == 0 && out.contains(r#""attempts":3"#),
        "--retry: code={code} out={out}"
    );
    // --no-retry sends once, and the assertion fails.
    let (tmp, _srv) = retry_project();
    let dir = tmp.path().to_str().unwrap();
    let (code, out, _) = execute(&[
        "run",
        "wait",
        "-C",
        dir,
        "--env",
        "dev",
        "--no-session",
        "--json",
        "--no-retry",
    ]);
    assert!(
        code == 1 && !out.contains(r#""attempts""#) && out.contains(r#""ok":false"#),
        "--no-retry: code={code} out={out}"
    );
    // A bad --retry is a usage error.
    let (code, _, errb) = execute(&[
        "run",
        "plain",
        "-C",
        dir,
        "--env",
        "dev",
        "--no-session",
        "--retry",
        "soon",
    ]);
    assert!(
        code == 2 && errb.contains(r#"--retry "soon""#),
        "bad --retry: code={code} err={errb}"
    );
}

#[test]
fn validate_reports_bad_retry() {
    let tmp = tmp();
    let dir = tmp.path().to_str().unwrap();
    must_write(
        join(dir, "api.http"),
        "### a\n# @name a\n# @retry 0\nGET http://x\n",
    );
    must_write(join(dir, "apic.yaml"), "retry: 3 soon\n");
    let (code, out, _) = execute(&["validate", "-C", dir, "--no-color"]);
    assert_ne!(code, 0, "validate should fail:\n{out}");
    for want in [
        r#"api.http:3:10: error: @retry "0": attempts must be a whole number of at least 1, got "0" (bad-retry)"#,
        r#"apic.yaml:0: error: retry "3 soon": interval must be a duration"#,
    ] {
        assert!(out.contains(want), "missing {want:?} in:\n{out}");
    }
}
