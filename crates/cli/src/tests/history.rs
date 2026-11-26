use std::path::Path;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Arc;

use apic_httpd::{Response, TestServer};
use serde_json::Value;
use tempfile::TempDir;

use super::{execute, join, must_read, must_write, server, tmp};

/// Serves a counter whose body changes on every call and writes a
/// project that keeps `keep` responses per request.
pub(crate) fn history_project(keep: usize) -> (TempDir, TestServer) {
    let n = Arc::new(AtomicI32::new(0));
    let srv = server(move |r| {
        let c = n.fetch_add(1, Ordering::SeqCst) + 1;
        let status = if r.path == "/flaky" && c % 2 == 0 {
            503
        } else {
            200
        };
        Response::json(
            status,
            format!(r#"{{"count":{c},"items":["a"],"stable":true}}"#),
        )
    });
    let dir = tmp();
    let url = srv.url();
    must_write(
        join(dir.path(), "api.http"),
        &format!(
            "### Count
# @name count
GET {url}/count
Authorization: Bearer secret-token

### Flaky
# @name flaky
GET {url}/flaky

### Unnamed
GET {url}/unnamed
"
        ),
    );
    if keep > 0 {
        must_write(join(dir.path(), "apic.yaml"), &format!("history: {keep}\n"));
    }
    (dir, srv)
}

/// Runs a command line and returns stdout, stderr and the exit code.
fn apic(args: &[&str]) -> (String, String, i32) {
    let (code, out, err) = execute(args);
    (out, err, code)
}

#[test]
fn history_records_lists_shows_and_diffs() {
    let (tmp, _srv) = history_project(2);
    let dir = tmp.path().to_str().unwrap();
    for _ in 0..3 {
        let (_, err_out, code) = apic(&["-C", dir, "run", "count"]);
        assert_eq!(code, 0, "run: {err_out}");
    }
    let (out, _, code) = apic(&["-C", dir, "history", "count"]);
    assert_eq!(code, 0);
    assert!(
        out.contains("count · default · 2 entries")
            && out.contains("#1")
            && out.contains("#2")
            && !out.contains("#3"),
        "history keeps the last 2:\n{out}"
    );

    let (out, _, _) = apic(&["-C", dir, "--json", "history", "count"]);
    let list: Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("{e}\n{out}"));
    let entries = list["entries"].as_array().unwrap();
    assert!(
        list["env"] == "default"
            && list["request"] == "count"
            && list["keep"] == 2
            && entries.len() == 2
            && entries[0]["index"] == 1
            && entries[0]["status"] == 200,
        "history --json = {list}"
    );

    let (out, _, _) = apic(&["-C", dir, "history", "count", "--show", "1"]);
    assert!(
        out.contains(r#""count": 3"#) && out.contains("200 OK"),
        "--show 1 is the newest (count 3):\n{out}"
    );
    let stored = must_read(Path::new(dir).join(entries[0]["file"].as_str().unwrap()));
    assert!(
        !stored.contains("secret-token"),
        "the Authorization header was stored unmasked"
    );

    let (out, _, _) = apic(&["-C", dir, "history", "diff", "count"]);
    assert!(
        out.contains("~ $.count: 2 → 3") && out.contains("1 change") && !out.contains("stable"),
        "diff of the last two:\n{out}"
    );
    let (out, _, _) = apic(&["-C", dir, "--json", "history", "diff", "count", "1", "2"]);
    let diff: Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("{e}\n{out}"));
    let changes = diff["changes"].as_array().unwrap();
    assert!(
        diff["from"]["index"] == 1
            && diff["to"]["index"] == 2
            && changes.len() == 1
            && changes[0]["path"] == "$.count"
            && changes[0]["from"] == 3
            && changes[0]["to"] == 2,
        "diff --json 1 2 = {diff}"
    );

    let (out, _, _) = apic(&["-C", dir, "history"]);
    assert!(
        out.contains("count 2 entries"),
        "history with no request lists the requests:\n{out}"
    );
}

#[test]
fn history_diff_shows_a_status_change() {
    let (tmp, _srv) = history_project(5);
    let dir = tmp.path().to_str().unwrap();
    apic(&["-C", dir, "run", "flaky"]);
    apic(&["-C", dir, "run", "flaky"]);
    let (out, _, _) = apic(&["-C", dir, "history", "diff", "flaky"]);
    assert!(out.contains("~ status: 200 → 503"), "diff:\n{out}");
}

#[test]
fn history_is_off_unless_configured() {
    let (tmp, _srv) = history_project(0);
    let dir = tmp.path().to_str().unwrap();
    apic(&["-C", dir, "run", "count"]);
    assert!(
        !Path::new(&join(dir, ".apic/history")).exists(),
        "history was recorded without history: in apic.yaml"
    );
    let (out, _, code) = apic(&["-C", dir, "history", "count"]);
    assert!(
        code == 0 && out.contains("no history for count") && out.contains("history is off"),
        "history while off: {code}\n{out}"
    );
}

#[test]
fn history_is_not_recorded_for_unnamed_no_session_or_data() {
    let (tmp, _srv) = history_project(5);
    let dir = tmp.path().to_str().unwrap();
    apic(&["-C", dir, "run", "api.http#3"]);
    apic(&["-C", dir, "--no-session", "run", "count"]);
    let rows = join(dir, "rows.csv");
    must_write(&rows, "x\n1\n2\n");
    let (_, err_out, code) = apic(&["-C", dir, "run", "count", "--data", &rows]);
    assert_eq!(code, 0, "data run: {err_out}");
    assert!(
        !Path::new(&join(dir, ".apic/history")).exists(),
        "history recorded an unnamed request, a --no-session run or a data run"
    );
    let (_, err_out, code) = apic(&["-C", dir, "history", "api.http#3"]);
    assert!(
        code == 2 && err_out.contains("no # @name"),
        "history of an unnamed request: {code} {err_out}"
    );
}

#[test]
fn history_redacted_runs_store_the_redacted_form() {
    let (tmp, _srv) = history_project(5);
    let dir = tmp.path().to_str().unwrap();
    apic(&["-C", dir, "--redact", "run", "count"]);
    let (out, _, _) = apic(&["-C", dir, "--json", "history", "count", "--show", "1"]);
    assert!(
        !out.contains(r#""count": 1"#) && out.contains(r#""body": "***""#),
        "a --redact run stored its body:\n{out}"
    );
}

#[test]
fn history_errors_and_clear() {
    let (tmp, _srv) = history_project(5);
    let dir = tmp.path().to_str().unwrap();
    apic(&["-C", dir, "run", "count"]);
    let cases: [(&[&str], &str); 7] = [
        (
            &["history", "diff", "count"],
            "count has 1 entry in default; a diff needs two",
        ),
        (
            &["history", "count", "--show", "4"],
            "count has 1 history entry, no #4",
        ),
        (
            &["history", "diff", "count", "0"],
            r#""0" is not an entry number"#,
        ),
        (&["history", "nope"], r#"no request named "nope""#),
        (&["history", "--show", "1"], "--show needs a request"),
        (
            &["history", "clear", "count", "--all"],
            "give it no request",
        ),
        (&["history", "clear"], "pass --all"),
    ];
    for (args, want) in cases {
        let (_, err_out, code) = apic(&[&["-C", dir], args].concat());
        assert!(
            code == 2 && err_out.contains(want),
            "{args:?}: {code} {err_out}, want {want:?}"
        );
    }

    apic(&["-C", dir, "run", "flaky"]);
    let (out, _, _) = apic(&["-C", dir, "--json", "history", "clear", "count"]);
    assert!(
        out.contains(r#""request": "count""#) && out.contains(r#""entries": 1"#),
        "clear count:\n{out}"
    );
    let (out, _, _) = apic(&["-C", dir, "history"]);
    assert!(
        !out.contains("count") && out.contains("flaky"),
        "clearing count should leave flaky:\n{out}"
    );
    // A request that is gone from the files keeps its history until it
    // is cleared.
    must_write(
        join(dir, "api.http"),
        "### Other\n# @name other\nGET http://example.com\n",
    );
    let (out, _, code) = apic(&["-C", dir, "history", "flaky"]);
    assert!(
        code == 0 && out.contains("flaky · default · 1 entry"),
        "history of a removed request: {code}\n{out}"
    );
    let (_, err_out, code) = apic(&["-C", dir, "history", "clear"]);
    assert!(
        code == 2 && err_out.contains("--all for every request"),
        "a bare clear should refuse: {code} {err_out}"
    );
    let (out, _, _) = apic(&["-C", dir, "history", "clear", "--all"]);
    assert!(
        out.contains("cleared 1 entry"),
        "clear the environment:\n{out}"
    );
}

#[test]
fn history_clear_scopes() {
    let (tmp, _srv) = history_project(5);
    let dir = tmp.path().to_str().unwrap();
    must_write(
        join(dir, "http-client.env.json"),
        r#"{"dev": {}, "staging": {}}"#,
    );
    apic(&["-C", dir, "--env", "dev", "run", "count"]);
    apic(&["-C", dir, "--env", "staging", "run", "count"]);
    let (out, _, _) = apic(&["-C", dir, "--env", "dev", "history", "clear", "--all"]);
    assert!(
        out.contains("cleared 1 entry"),
        "--all clears the environment only:\n{out}"
    );
    let (out, _, _) = apic(&["-C", dir, "--env", "staging", "history"]);
    assert!(
        out.contains("count 1 entry"),
        "staging lost its history:\n{out}"
    );
    let (out, _, _) = apic(&["-C", dir, "--json", "history", "clear", "--every-env"]);
    assert!(
        out.contains(r#""cleared": "*""#) && out.contains(r#""entries": 1"#),
        "--every-env:\n{out}"
    );
}

#[test]
fn history_clear_refuses_for_a_request_named_clear() {
    let (tmp, _srv) = history_project(5);
    let dir = tmp.path().to_str().unwrap();
    must_write(
        join(dir, "cart.http"),
        "### Clear the cart\n# @name clear\nDELETE http://example.com/cart\n",
    );
    let (_, err_out, code) = apic(&["-C", dir, "history", "clear"]);
    assert!(
        code == 2 && err_out.contains("apic history cart.http#clear"),
        "bare clear with a request named clear: {code} {err_out}"
    );
}

#[test]
fn history_keeps_requests_that_share_a_name_apart() {
    let (tmp, _srv) = history_project(5);
    let dir = tmp.path().to_str().unwrap();
    must_write(
        join(dir, "other.http"),
        &must_read(join(dir, "api.http")).replacen("/count", "/other-count", 1),
    );
    let (_, err_out, code) = apic(&["-C", dir, "run", "api.http#count"]);
    assert_eq!(code, 0, "run: {err_out}");
    apic(&["-C", dir, "run", "other.http#count"]);
    apic(&["-C", dir, "run", "other.http#count"]);
    let (out, _, _) = apic(&["-C", dir, "history", "api.http#count"]);
    assert!(
        out.contains("api.http#count · default · 1 entry"),
        "api.http#count:\n{out}"
    );
    let (out, _, _) = apic(&["-C", dir, "history", "other.http#count"]);
    assert!(out.contains("2 entries"), "other.http#count:\n{out}");
    let (_, err_out, code) = apic(&["-C", dir, "history", "count"]);
    assert!(
        code == 2 && err_out.contains("more than once"),
        "a shared name is ambiguous: {code} {err_out}"
    );
}

#[test]
fn history_holds_the_final_result() {
    let (tmp, _srv) = history_project(5);
    let dir = tmp.path().to_str().unwrap();
    let elsewhere = super::tmp();
    let out = join(elsewhere.path(), "count.json");
    let (_, err_out, code) = apic(&["-C", dir, "run", "count", "--output", &out]);
    assert_eq!(code, 0, "run --output: {err_out}");
    let (shown, _, _) = apic(&["-C", dir, "--json", "history", "count", "--show", "1"]);
    assert!(
        shown.contains(r#""saved_to""#),
        "history lacks the --output path:\n{shown}"
    );
}

#[test]
fn a_history_that_cannot_be_written_is_a_warning() {
    let (tmp, _srv) = history_project(5);
    let dir = tmp.path().to_str().unwrap();
    // A file where the history directory should be: every write fails.
    must_write(join(dir, ".apic/history"), "not a directory");
    let (out, err_out, code) = apic(&["-C", dir, "--json", "run", "count"]);
    assert_eq!(code, 0, "a failed history write failed the run: {err_out}");
    let res: Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("{e}\n{out}"));
    let warnings = res["warnings"].as_array().cloned().unwrap_or_default();
    assert!(
        res["ok"] == true
            && res.get("errors").is_none()
            && warnings.len() == 1
            && warnings[0].as_str().unwrap_or("").starts_with("history: "),
        "result = {res}"
    );
    let (text, _, code) = apic(&["-C", dir, "run", "count"]);
    assert!(
        code == 0 && text.contains("! history: "),
        "text output: {code}\n{text}"
    );
}

#[test]
fn history_commands_do_not_need_the_session() {
    let (tmp, _srv) = history_project(5);
    let dir = tmp.path().to_str().unwrap();
    apic(&["-C", dir, "run", "count"]);
    must_write(join(dir, ".apic/session.json"), "{not json");
    let cases: [&[&str]; 2] = [&["history", "count"], &["history", "clear", "--every-env"]];
    for args in cases {
        let (_, err_out, code) = apic(&[&["-C", dir], args].concat());
        assert_eq!(code, 0, "{args:?} with a broken session: {err_out}");
    }
}
