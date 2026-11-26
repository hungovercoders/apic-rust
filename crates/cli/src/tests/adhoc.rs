use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Arc;

use apic_httpd::{Response, TestServer};
use serde_json::Value;
use tempfile::TempDir;

use super::{execute, join, must_read, must_write, server, tmp};

/// Serves one JSON object and counts the requests it gets.
fn ad_hoc_project() -> (TempDir, Arc<AtomicI32>, TestServer) {
    let hits = Arc::new(AtomicI32::new(0));
    let count = hits.clone();
    let srv = server(move |_| {
        count.fetch_add(1, Ordering::SeqCst);
        Response::json(
            200,
            r#"{"id": 7, "items": [{"done": true}, {"done": false}]}"#,
        )
    });
    let dir = tmp();
    must_write(
        join(dir.path(), "api.http"),
        "
### thing
# @name thing
# @assert status == 200
GET {{baseUrl}}/thing
X-Token: {{token}}

### other
# @name other
POST {{baseUrl}}/other
Content-Type: application/json

{\"id\": {{id}}}

### login
# @name login
# @capture session = body.$.id
GET {{baseUrl}}/login

### secured
# @name secured
# @ref login
DELETE {{baseUrl}}/secured
X-Auth: Bearer {{session}}

### cleanup
# @name cleanup
# @disabled
# @assert status == 200
DELETE {{baseUrl}}/cleanup
",
    );
    must_write(
        join(dir.path(), "http-client.env.json"),
        &format!(r#"{{"dev":{{"baseUrl":"{}","token":"t"}}}}"#, srv.url()),
    );
    (dir, hits, srv)
}

fn decode_run(out: &str) -> Value {
    serde_json::from_str(out.trim()).unwrap_or_else(|e| panic!("{e}: {out}"))
}

fn asserts(obj: &Value) -> &[Value] {
    obj["asserts"].as_array().map_or(&[], Vec::as_slice)
}

/// --assert and --capture add to the request for this run, so a selector
/// can be tried before it is written into the file.
#[test]
fn run_assert_and_capture_flags() {
    let (tmp, _, _srv) = ad_hoc_project();
    let dir = tmp.path().to_str().unwrap();
    let base = [
        "-C",
        dir,
        "--env",
        "dev",
        "--no-session",
        "--json",
        "run",
        "thing",
    ];
    let run = |extra: &[&str]| execute(&[&base[..], extra].concat());
    let (code, out, errb) = run(&[
        "--assert",
        "body.$.items[?(@.done != true)].# == 1",
        "--assert",
        "body.$.id isInteger",
        "--capture",
        "count = body.$.items.#",
    ]);
    assert_eq!(code, 0, "err={errb} out={out}");
    let obj = decode_run(&out);
    let a = asserts(&obj);
    assert!(
        a.len() == 3 && a[1]["pass"] == true && a[1]["actual"] == "1" && a[2]["pass"] == true,
        "asserts: {a:?}"
    );
    assert_eq!(
        obj["captures"]["count"], "2",
        "captures: {}",
        obj["captures"]
    );
    // A failing flag assertion fails the run like a directive would.
    let (code, out, _) = run(&["--assert", "body.$.id == 8"]);
    let obj = decode_run(&out);
    let a = asserts(&obj);
    assert!(
        code == 1 && obj["ok"] == false && a[1]["pass"] == false && a[1]["actual"] == "7",
        "failing: code={code} {a:?}"
    );
    // A bad expression is a flag error that names the flag, before
    // anything is sent.
    for bad in [
        ["--assert", "body.$.id ==="],
        ["--capture", "nosel"],
        ["--capture", "=body"],
    ] {
        let (code, _, errb) = run(&bad);
        assert!(
            code == 2 && errb.contains(&format!("{} ", bad[0])),
            "{bad:?}: code={code} err={errb}"
        );
    }
    // The project's own request is untouched: a plain run has one assert.
    let (code, out, _) = run(&[]);
    assert!(
        code == 0 && asserts(&decode_run(&out)).len() == 1,
        "plain run after ad hoc: code={code} {out}"
    );
}

/// A dry run prints what would be sent and sends nothing.
#[test]
fn run_dry_run() {
    let (tmp, hits, _srv) = ad_hoc_project();
    let dir = tmp.path().to_str().unwrap();
    let (code, out, errb) = execute(&[
        "-C",
        dir,
        "--env",
        "dev",
        "--no-session",
        "--json",
        "run",
        "other",
        "--var",
        "id=3",
        "--dry-run",
    ]);
    assert_eq!(code, 0, "err={errb}");
    let obj = decode_run(&out);
    assert!(
        obj["dry_run"] == true
            && obj["ok"] == true
            && obj.get("response").is_none_or(Value::is_null)
            && obj["request"]["method"] == "POST"
            && obj["request"]["url"]
                .as_str()
                .unwrap_or("")
                .ends_with("/other"),
        "dry run object: {out}"
    );
    assert_eq!(hits.load(Ordering::SeqCst), 0, "a dry run sent a request");
    // The text form shows the resolved body and says it was not sent.
    let (code, out, _) = execute(&[
        "-C",
        dir,
        "--env",
        "dev",
        "--no-session",
        "--no-color",
        "run",
        "other",
        "thing",
        "--var",
        "id=3",
        "--dry-run",
    ]);
    assert!(
        code == 0
            && out.contains(r#"{"id": 3}"#)
            && out.matches("dry run · not sent").count() == 2
            && out.contains("X-Token: t"),
        "text: code={code}\n{out}"
    );
    // A missing variable is the same error a real run gives, and nothing
    // runs to supply it.
    let (code, _, errb) = execute(&[
        "-C",
        dir,
        "--env",
        "dev",
        "--no-session",
        "run",
        "other",
        "--dry-run",
    ]);
    assert!(
        code == 2 && errb.contains("{{id}}") && hits.load(Ordering::SeqCst) == 0,
        "missing: code={code} err={errb}"
    );
    let (code, _, errb) = execute(&[
        "-C",
        dir,
        "--env",
        "dev",
        "run",
        "thing",
        "--dry-run",
        "--data",
        "-",
    ]);
    assert!(
        code == 2 && errb.contains("--dry-run"),
        "with --data: code={code} err={errb}"
    );
    // --body-only prints the request body, there being no response.
    let (code, out, _) = execute(&[
        "-C",
        dir,
        "--env",
        "dev",
        "--no-session",
        "run",
        "other",
        "--var",
        "id=3",
        "--dry-run",
        "--body-only",
    ]);
    assert!(
        code == 0 && out == "{\"id\": 3}\n",
        "body-only: code={code} out={out:?}"
    );
    assert_eq!(hits.load(Ordering::SeqCst), 0, "a dry run sent a request");
}

/// A variable a `# @ref` dependency would capture does not fail a dry
/// run: the request is shown with the placeholder and a warning naming
/// what a run would send first, and still nothing is sent.
#[test]
fn run_dry_run_leaves_ref_supplied_placeholders() {
    let (tmp, hits, _srv) = ad_hoc_project();
    let dir = tmp.path().to_str().unwrap();
    let (code, out, errb) = execute(&[
        "-C",
        dir,
        "--env",
        "dev",
        "--no-session",
        "--json",
        "run",
        "secured",
        "--dry-run",
    ]);
    assert_eq!(code, 0, "err={errb}");
    let obj = decode_run(&out);
    let warnings = obj["warnings"].as_array().cloned().unwrap_or_default();
    assert!(
        obj["dry_run"] == true
            && obj["ok"] == true
            && obj["request"]["headers"]["X-Auth"] == "Bearer {{session}}"
            && warnings.len() == 1
            && warnings[0]
                .as_str()
                .unwrap_or("")
                .contains("send login first"),
        "object: {out}"
    );
    assert_eq!(hits.load(Ordering::SeqCst), 0, "the dependency was sent");
    let (code, out, _) = execute(&[
        "-C",
        dir,
        "--env",
        "dev",
        "--no-session",
        "--no-color",
        "run",
        "secured",
        "--dry-run",
    ]);
    assert!(
        code == 0
            && out.contains("! {{session}} is not set; a run would send login first")
            && out.contains("dry run · not sent"),
        "text: code={code}\n{out}"
    );
}

/// The ad hoc checks ride on the runner's options rather than on copies
/// of the requests, so a `# @disabled` request named on the command line
/// is still the one the runner knows it was asked for, and is sent.
#[test]
fn run_ad_hoc_flags_keep_disabled_targets() {
    let (tmp, hits, _srv) = ad_hoc_project();
    let dir = tmp.path().to_str().unwrap();
    let (code, out, errb) = execute(&[
        "-C",
        dir,
        "--env",
        "dev",
        "--no-session",
        "--json",
        "run",
        "cleanup",
        "--assert",
        "body.$.id == 7",
    ]);
    assert_eq!(code, 0, "err={errb} out={out}");
    let obj = decode_run(&out);
    let a = asserts(&obj);
    assert!(
        hits.load(Ordering::SeqCst) == 1
            && obj["response"].is_object()
            && a.len() == 2
            && a[1]["pass"] == true,
        "hits={} {out}",
        hits.load(Ordering::SeqCst)
    );
    // And they do not reach a dependency: login, pulled in by secured's
    // `# @ref`, keeps its own checks.
    let (code, out, _) = execute(&[
        "-C",
        dir,
        "--env",
        "dev",
        "--no-session",
        "--json",
        "run",
        "secured",
        "--assert",
        "status == 200",
        "--capture",
        "n=body.$.id",
    ]);
    let lines: Vec<&str> = out.trim().split('\n').collect();
    assert!(code == 0 && lines.len() == 2, "code={code} out={out}");
    let (login, secured) = (decode_run(lines[0]), decode_run(lines[1]));
    assert!(
        asserts(&login).is_empty()
            && login["captures"].get("n").is_none()
            && asserts(&secured).len() == 1
            && secured["captures"]["n"] == "7",
        "login {login} / secured {secured}"
    );
}

/// fmt file.http#name formats the one request and keeps the rest byte
/// for byte, so an agent's change stays its own.
#[test]
fn fmt_one_request() {
    let tmp = tmp();
    let dir = tmp.path().to_str().unwrap();
    let src = "###   first\n# @assert status == 200\n# @name first\nGET   https://x/a\n\n\n### second\n# @capture id = body.$.id\n# @name second\nPOST https://x/b\ncontent-type: application/json\n\n{\"a\":1}\n";
    must_write(join(dir, "api.http"), src);
    let (code, out, errb) = execute(&["-C", dir, "fmt", "api.http#second", "--no-color"]);
    assert!(
        code == 0 && out.contains("formatted api.http"),
        "code={code} out={out} err={errb}"
    );
    let got = must_read(join(dir, "api.http"));
    let want = "###   first\n# @assert status == 200\n# @name first\nGET   https://x/a\n\n\n### second\n# @name second\n# @capture id = body.$.id\nPOST https://x/b\nContent-Type: application/json\n\n{\n  \"a\": 1\n}\n";
    assert_eq!(got, want);
    // The number form, --check reporting, and an unknown name.
    let (code, out, _) = execute(&["-C", dir, "fmt", "api.http#1", "--check"]);
    assert!(
        code == 1 && out.trim() == "api.http",
        "check: code={code} out={out:?}"
    );
    let (code, out, _) = execute(&["-C", dir, "fmt", "api.http#2", "--check"]);
    assert!(
        code == 0 && out.contains("already formatted"),
        "second is formatted: code={code} out={out:?}"
    );
    let (code, _, errb) = execute(&["-C", dir, "fmt", "api.http#third"]);
    assert!(
        code == 2 && errb.contains(r#"no request named "third""#),
        "unknown: code={code} err={errb}"
    );
}

/// Requests are counted as `apic run file.http#N` counts them: the text
/// before the first `###` is one when it holds a request line, a `###`
/// block without one is not; and a request named there can be formatted.
#[test]
fn fmt_one_request_counts_like_run() {
    let tmp = tmp();
    // A # in the project directory is not a selector.
    let dir_path = tmp.path().join("proj#2");
    let dir = dir_path.to_str().unwrap();
    let src = "# @name   first\nGET   https://x/a\n\n### Users\n# a heading block, no request\n\n### second\n# @name   second\nGET   https://x/b\n";
    must_write(join(dir, "api.http"), src);
    let (code, _, errb) = execute(&["-C", dir, "fmt", "api.http#first"]);
    assert_eq!(code, 0, "by name in the implicit block: err={errb}");
    assert_eq!(
        must_read(join(dir, "api.http")),
        "# @name first\nGET https://x/a\n\n### Users\n# a heading block, no request\n\n### second\n# @name   second\nGET   https://x/b\n",
        "after #first"
    );
    let (code, _, errb) = execute(&["-C", dir, "fmt", "api.http#2"]);
    assert_eq!(code, 0, "by number: err={errb}");
    let got = must_read(join(dir, "api.http"));
    assert!(
        got.ends_with("### second\n# @name second\nGET https://x/b\n"),
        "after #2 (the second request, not the second ### block):\n{got}"
    );
    let (code, _, errb) = execute(&["-C", dir, "fmt", "api.http#3"]);
    assert!(
        code == 2 && errb.contains(r#"no request named "3""#),
        "#3 of two requests: code={code} err={errb}"
    );
    // Two requests of one file under --diff --json: one diff each, the
    // file listed once.
    must_write(join(dir, "api.http"), src);
    let (code, out, _) = execute(&[
        "-C",
        dir,
        "--json",
        "fmt",
        "api.http#first",
        "api.http#second",
        "--diff",
    ]);
    let got: Value =
        serde_json::from_str(&out).unwrap_or_else(|e| panic!("code={code} {e}: {out}"));
    assert_eq!(code, 1);
    assert!(
        got["changed"].as_array().is_some_and(|c| c.len() == 1)
            && got["diff"].as_object().is_some_and(|d| d.len() == 2)
            && got["diff"]["api.http#first"]
                .as_str()
                .unwrap_or("")
                .contains("+# @name first")
            && got["diff"]["api.http#second"]
                .as_str()
                .unwrap_or("")
                .contains("+# @name second"),
        "json: {got}"
    );
}

/// A selector or a name a directive would be refused for is refused on
/// the flag too, before the request is sent: an agent trying `--capture`
/// on a DELETE must not learn of its typo from the response.
#[test]
fn run_ad_hoc_flags_are_checked_before_sending() {
    let (tmp, hits, _srv) = ad_hoc_project();
    let dir = tmp.path().to_str().unwrap();
    for bad in [
        ["--assert", "nope == 1"],
        ["--assert", "header == 1"],
        ["--capture", "x=not-a-selector"],
        ["--capture", "1bad=body.$.id"],
        ["--capture", "two words=body.$.id"],
    ] {
        let (code, _, errb) = execute(&[
            "-C",
            dir,
            "--env",
            "dev",
            "--no-session",
            "run",
            "other",
            "--var",
            "id=1",
            bad[0],
            bad[1],
        ]);
        assert!(
            code == 2 && errb.contains(&format!("{} ", bad[0])),
            "{bad:?}: code={code} err={errb}"
        );
    }
    assert_eq!(
        hits.load(Ordering::SeqCst),
        0,
        "requests were sent before the flag check"
    );
}
