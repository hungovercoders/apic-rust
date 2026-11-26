use std::collections::BTreeMap;

use apic_gocompat::{format_time, Time};
use apic_history::{Change, Entry};
use apic_httpc::CancelToken;
use apic_httpd::{handler, Response, TestServer};
use apic_httpfile::Header;
use apic_runner::{
    parse_result, shared_session, Description, Options as RunOptions, Progress, Result, Runner,
    VarInfo,
};
use serde_json::Value;

use crate::style::{with_color_profile, Profile};
use crate::*;

/// Runs `f` without colour, whatever the terminal the tests run in.
fn plain<R>(f: impl FnOnce() -> R) -> R {
    with_color_profile(Profile::Ascii, f)
}

fn vars(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn run_all(src: &str, base_url: &str, redact: bool) -> Vec<Result> {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("api.http"), src).unwrap();
    let p = apic_project::load(dir.path()).unwrap();
    let r = Runner::new(
        p,
        RunOptions {
            vars: vars(&[("baseUrl", base_url)]),
            session: Some(shared_session(apic_session::new_memory())),
            keep_going: true,
            redact,
            ..RunOptions::default()
        },
    )
    .unwrap();
    let (out, err) = r.run_all(&CancelToken::new(), &r.project.requests());
    assert!(err.is_none(), "{err:?}");
    out
}

fn results() -> Vec<Result> {
    let srv = TestServer::new(handler(|r| {
        if r.path == "/users/42" {
            return Response::json(
                200,
                r#"{"id": 42, "name": "alice", "tags": ["a","b"], "active": true, "age": null}"#,
            );
        }
        Response::new(404).with_body(r#"{"error": "nope"}"#)
    }));
    let src = "### Fetch\n# @name get-user\n# @assert status == 200\n# @assert body.$.name == alice\n# @capture uid = body.$.id\nGET {{baseUrl}}/users/42\nAccept: application/json\n\n### Missing\n# @name missing\n# @assert status == 200\nGET {{baseUrl}}/nope\n";
    run_all(src, &srv.url(), false)
}

fn written(f: impl FnOnce(&mut Vec<u8>)) -> String {
    let mut buf = Vec::new();
    f(&mut buf);
    String::from_utf8_lossy(&buf).into_owned()
}

#[test]
fn human_renders_status_body_checks_and_captures() {
    let res = results();
    plain(|| {
        let got = written(|b| human(b, &res[0], false));
        for want in [
            "GET http://",
            "200 OK",
            " ms",
            "\"name\": \"alice\"",
            "✓ status == 200",
            "✓ body.$.name == alice",
            "↳ uid = 42",
        ] {
            assert!(got.contains(want), "missing {want:?} in:\n{got}");
        }
        assert!(
            !got.contains("\x1b["),
            "ascii profile should produce no escapes:\n{got:?}"
        );
        let verbose = written(|b| human(b, &res[0], true));
        assert!(
            verbose.contains("Accept: application/json")
                && verbose.contains("content-type: application/json"),
            "verbose output should include request and response headers:\n{verbose}"
        );
        assert!(
            verbose.contains("HTTP/1.1 200 OK") && !got.contains("HTTP/1.1 200"),
            "only verbose output names the protocol:\n{verbose}"
        );
    });
}

#[test]
fn failure_shows_actual_and_summary_table() {
    let res = results();
    plain(|| {
        let got = written(|b| human(b, &res[1], false));
        assert!(
            got.contains("✗ status == 200 (actual: 404)"),
            "failed assertion should show the actual value:\n{got}"
        );
        let checks = checks(&Theme::default(), &res[1], 80, true);
        assert!(
            checks.contains("expected: 200"),
            "checks with expected should show the expected value:\n{checks}"
        );
        let got = written(|b| summary(b, &res));
        for want in [
            "✓ get-user",
            "✗ missing",
            "404",
            "status == 200 (actual: 404)",
            "1 failed, 1 passed",
            "2 requests",
        ] {
            assert!(got.contains(want), "missing {want:?} in:\n{got}");
        }
    });
}

#[test]
fn highlight_json_styles_each_kind_of_token() {
    let input = "{\n  \"key\": \"a:b\",\n  \"n\": -1.5e3,\n  \"ok\": true,\n  \"nil\": null\n}";
    let got = plain(|| highlight_json(&Theme::default(), input));
    assert_eq!(got, input, "ascii profile must not change the text");
    with_color_profile(Profile::Ansi, || {
        let th = Theme::default();
        let got = highlight_json(&th, input);
        assert!(
            got.contains(&th.key.render("\"key\"")),
            "key not styled as key: {got:?}"
        );
        assert!(
            got.contains(&th.str.render("\"a:b\"")),
            "string value with a colon must be styled as a string: {got:?}"
        );
        assert!(
            got.contains(&th.num.render("-1.5e3"))
                && got.contains(&th.lit.render("true"))
                && got.contains(&th.lit.render("null")),
            "numbers and literals not styled: {got:?}"
        );
        assert!(
            got.contains("\x1b["),
            "the ANSI profile writes escapes: {got:?}"
        );
    });
}

#[test]
fn truncate_is_rune_safe() {
    assert_eq!(truncate("héllo wörld", 5), "héllo…");
    assert_eq!(truncate("a\nb", 10), "a b");
}

#[test]
fn render_body_leaves_non_json_alone() {
    assert_eq!(
        render_body(&Theme::default(), b"<html>hi</html>"),
        "<html>hi</html>"
    );
}

/// Covers the three renderers that are separate code paths from --json:
/// the human report, --body-only and the flow summary. Before this, a
/// redacted run masked the captured token while printing the response
/// body it came from.
#[test]
fn redact_masks_response_everywhere() {
    let srv = TestServer::new(handler(|_| {
        Response::json(200, r#"{"access_token":"body-secret","name":"nope"}"#)
            .with_header("Set-Cookie", "sid=cookie-secret")
    }));
    let src = "### Login\n# @name login\n# @assert body.$.name == alice\n# @capture token = body.$.access_token\nGET {{baseUrl}}/login\nX-Api-Key: header-secret\n";
    let res = run_all(src, &srv.url(), true);
    plain(|| {
        let renders: [(&str, String); 4] = [
            // verbose: prints response headers too
            ("human", written(|b| human(b, &res[0], true))),
            ("body", written(|b| body(b, &res[0]))),
            ("summary_table", summary_table(&Theme::default(), &res)),
            ("checks", checks(&Theme::default(), &res[0], 80, true)),
        ];
        for (name, out) in &renders {
            for bad in ["body-secret", "cookie-secret", "header-secret"] {
                assert!(
                    !out.contains(bad),
                    "{name} leaked {bad:?} under --redact:\n{out}"
                );
            }
        }
    });
}

#[test]
fn json_writes_one_line_per_request_sent() {
    let srv = TestServer::new(handler(|_| Response::json(200, r#"{"t": "<a&b>"}"#)));
    let src = "### One\n# @name one\n# @capture t = body.$.t\nGET {{baseUrl}}/one\n\n### Two\n# @name two\n# @ref one\nGET {{baseUrl}}/two?x={{t}}\n";
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("api.http"), src).unwrap();
    let p = apic_project::load(dir.path()).unwrap();
    let r = Runner::new(
        p,
        RunOptions {
            vars: vars(&[("baseUrl", &srv.url())]),
            no_session: true,
            ..RunOptions::default()
        },
    )
    .unwrap();
    let req = r.project.lookup("two").unwrap();
    let (res, err) = r.run(&CancelToken::new(), &req);
    assert!(err.is_none(), "{err:?}");
    let res = res.unwrap();
    assert_eq!(res.deps.len(), 1);

    let flat = written(|b| json(b, &res).unwrap());
    let lines: Vec<&str> = flat.lines().collect();
    assert_eq!(lines.len(), 2, "{flat}");
    assert!(flat.ends_with('\n'));
    let first: Value = serde_json::from_str(lines[0]).unwrap();
    let second: Value = serde_json::from_str(lines[1]).unwrap();
    assert_eq!(first["request"]["name"], "one");
    assert_eq!(second["request"]["name"], "two");
    assert!(second.get("ran_first").is_none(), "{flat}");
    assert_eq!(first["captures"]["t"], "<a&b>");
    assert_eq!(lines[1], {
        let mut alone = res.clone();
        alone.deps.clear();
        alone.to_json()
    });

    let nested = written(|b| json_nested(b, &res).unwrap());
    assert_eq!(nested.lines().count(), 1);
    let v: Value = serde_json::from_str(&nested).unwrap();
    assert_eq!(v["ran_first"][0]["request"]["name"], "one");
}

// ---------------------------------------------------------------------
// The recorded renders: `testdata/render_golden.json` holds results as
// `apic run --json` printed them, with what each renderer made of them
// under every colour profile.

const GOLDEN: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/render_golden.json");

fn golden() -> Value {
    serde_json::from_str(&std::fs::read_to_string(GOLDEN).expect("the golden file")).expect("JSON")
}

/// The theme's colours are all of the 256, so 24-bit colour would show
/// nothing the 256-colour profile does not.
const PROFILES: [(&str, Profile); 3] = [
    ("ansi256", Profile::Ansi256),
    ("ansi", Profile::Ansi),
    ("ascii", Profile::Ascii),
];

/// The parts of a result's JSON that a stored result does not keep but
/// the renderers show.
#[derive(serde::Deserialize, Default)]
#[serde(default)]
struct Extras {
    warnings: Vec<String>,
    dry_run: bool,
    skipped: String,
    ran_first: Vec<Box<serde_json::value::RawValue>>,
    error: Option<apic_runner::ErrorInfo>,
}

/// Rebuilds a result from its JSON: what `parse_result` reads, and around
/// it the fields of [`Extras`].
fn load(json: &str) -> Result {
    let mut res = parse_result(json).unwrap();
    let extras: Extras = serde_json::from_str(json).unwrap();
    res.warnings = extras.warnings;
    res.dry_run = extras.dry_run;
    res.skipped = extras.skipped;
    res.error = extras.error;
    res.deps = extras.ran_first.iter().map(|d| load(d.get())).collect();
    res
}

fn set_redact(res: &mut Result, redact: bool) {
    res.redact = redact;
    for dep in &mut res.deps {
        set_redact(dep, redact);
    }
}

fn load_case(case: &Value) -> Result {
    let mut res = load(case["json"].as_str().unwrap());
    set_redact(&mut res, case["redact"].as_bool().unwrap());
    res.recorded = case["recorded"].as_bool().unwrap();
    res.record_key = case["record_key"].as_str().unwrap().to_string();
    res.unrecorded = case["unrecorded"].as_str().unwrap().to_string();
    res.body_limit = case["body_limit"].as_u64().unwrap() as usize;
    res
}

/// Collects what differs from the record. With UPDATE_GOLDEN set it
/// collects the new renders instead and writes them into the record, for
/// a deliberate change to a renderer.
#[derive(Default)]
struct Diffs {
    differing: Vec<String>,
    /// How many renders were held against the record.
    compared: usize,
    /// Where each new render goes in the file: a JSON pointer, the text.
    updates: Vec<(String, String)>,
}

/// One writer at a time, when several tests update the record.
static RECORD: std::sync::Mutex<()> = std::sync::Mutex::new(());

impl Diffs {
    /// Compares `got` with the render recorded at `pointer` (as
    /// `/results/3/out/ansi/checks`); a render the record does not hold is
    /// not compared.
    fn check(&mut self, golden: &Value, pointer: &str, what: &str, got: &str) {
        let Some(want) = golden.pointer(pointer).and_then(Value::as_str) else {
            return;
        };
        self.compared += 1;
        if got == want {
            return;
        }
        if std::env::var_os("UPDATE_GOLDEN").is_some() {
            self.updates.push((pointer.to_string(), got.to_string()));
            return;
        }
        // Show where the two part ways, not all of both.
        let at = got
            .char_indices()
            .zip(want.chars())
            .find(|((_, a), b)| a != b)
            .map_or(got.len().min(want.len()), |((i, _), _)| i);
        let from = (0..=at.saturating_sub(40))
            .rev()
            .find(|i| got.is_char_boundary(*i) && want.is_char_boundary(*i))
            .unwrap_or(0);
        let tail = |s: &str| s[from..].chars().take(120).collect::<String>();
        self.differing.push(format!(
            "{what} differs at byte {at}\n  got  {:?}\n  want {:?}",
            tail(got),
            tail(want)
        ));
    }

    /// Fails on any difference, and when fewer than `at_least` renders
    /// were compared: a record that lost its renders proves nothing.
    fn finish(self, at_least: usize) {
        assert!(
            self.compared >= at_least,
            "only {} renders were compared",
            self.compared
        );
        if !self.updates.is_empty() {
            let _guard = RECORD.lock().unwrap();
            let mut golden = golden();
            for (pointer, text) in self.updates {
                *golden.pointer_mut(&pointer).unwrap() = Value::from(text);
            }
            let mut text = serde_json::to_string_pretty(&golden).unwrap();
            text.push('\n');
            std::fs::write(GOLDEN, text).unwrap();
        }
        assert!(
            self.differing.is_empty(),
            "{} renders differ (UPDATE_GOLDEN=1 accepts them):\n{}",
            self.differing.len(),
            self.differing
                .iter()
                .take(20)
                .cloned()
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
}

#[test]
fn results_render_as_recorded() {
    let golden = golden();
    let mut diffs = Diffs::default();
    let cases = golden["results"].as_array().unwrap();
    assert!(cases.len() > 20);
    for (i, case) in cases.iter().enumerate() {
        let name = case["name"].as_str().unwrap();
        let res = load_case(case);
        for (pname, profile) in PROFILES {
            with_color_profile(profile, || {
                let t = Theme::default();
                let mut check = |key: &str, got: String| {
                    let pointer = format!("/results/{i}/out/{pname}/{key}");
                    diffs.check(&golden, &pointer, &format!("{name}: {key} [{pname}]"), &got)
                };
                check("request_line", request_line(&t, &res));
                check("request_detail", request_detail(&t, &res));
                check("status_line", status_line(&t, &res));
                check("timings", timings(&t, &res));
                check("checks", checks(&t, &res, 0, false));
                check("checks_narrow", checks(&t, &res, 12, true));
                check("result", result(&t, &res, Options::default()));
                let verbose = Options {
                    verbose: true,
                    width: 30,
                };
                check("result_verbose", result(&t, &res, verbose));
                check("human", written(|b| human(b, &res, false)));
                check("human_verbose", written(|b| human(b, &res, true)));
                check("deps", written(|b| deps(b, &res, false)));
                check("body", written(|b| body(b, &res)));
                check("json", written(|b| json(b, &res).unwrap()));
                check("json_nested", written(|b| json_nested(b, &res).unwrap()));
                check("response_headers", response_headers(&t, &res));
                let raw = res.display_raw_body();
                check("body_or_summary", body_or_summary(&t, &res, &raw));
                check("render_body", render_body(&t, &raw));
            });
        }
    }
    diffs.finish(1000);
}

#[test]
fn flows_render_as_recorded() {
    let golden = golden();
    let mut diffs = Diffs::default();
    let by_name: BTreeMap<&str, Result> = golden["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| (c["name"].as_str().unwrap(), load_case(c)))
        .collect();
    for (i, flow) in golden["flows"].as_array().unwrap().iter().enumerate() {
        let name = flow["name"].as_str().unwrap();
        let results: Vec<Result> = flow["results"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| by_name[n.as_str().unwrap()].clone())
            .collect();
        for (pname, profile) in PROFILES {
            with_color_profile(profile, || {
                let t = Theme::default();
                let mut check = |key: &str, got: String| {
                    let pointer = format!("/flows/{i}/out/{pname}/{key}");
                    diffs.check(&golden, &pointer, &format!("{name}: {key} [{pname}]"), &got)
                };
                check("summary_table", summary_table(&t, &results));
                check("summary_line", summary_line(&t, &results));
                check("summary", written(|b| summary(b, &results)));
            });
        }
    }
    diffs.finish(18);
}

#[test]
fn descriptions_render_as_recorded() {
    let golden = golden();
    let mut diffs = Diffs::default();
    let cases = golden["describes"].as_array().unwrap();
    assert_eq!(cases.len(), 4);
    for (i, case) in cases.iter().enumerate() {
        let name = case["name"].as_str().unwrap();
        let d: Description = serde_json::from_value(case["description"].clone()).unwrap();
        let headers: Vec<Header> = case["headers"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|h| Header {
                        name: h[0].as_str().unwrap().to_string(),
                        value: h[1].as_str().unwrap().to_string(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        for (pname, profile) in PROFILES {
            with_color_profile(profile, || {
                let t = Theme::default();
                let mut check = |key: &str, got: String| {
                    let pointer = format!("/describes/{i}/out/{pname}/{key}");
                    diffs.check(&golden, &pointer, &format!("{name}: {key} [{pname}]"), &got)
                };
                check("describe", describe(&t, &d, &headers));
                check("readiness", readiness(&t, &d));
                check("variables", variables(&t, &d.variables, true));
                check("variables_bare", variables(&t, &d.variables, false));
            });
        }
    }
    diffs.finish(48);
}

#[test]
fn the_small_renderers_give_what_was_recorded() {
    let golden = golden();
    let misc = &golden["misc"];
    let mut diffs = Diffs::default();
    let entries: Vec<Entry> = misc["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| Entry {
            index: e["index"].as_u64().unwrap() as usize,
            time: Time::parse_rfc3339(e["time"].as_str().unwrap()).unwrap(),
            ok: e["ok"].as_bool().unwrap(),
            status: e["status"].as_i64().unwrap(),
            status_text: e
                .get("status_text")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            duration_ms: e["duration_ms"].as_i64().unwrap(),
            size: e["size"].as_i64().unwrap(),
            file: e["file"].as_str().unwrap().to_string(),
            result: None,
        })
        .collect();
    let raw = |v: Option<&Value>| v.map(Value::to_string);
    let diff: Vec<Change> = misc["changes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| Change {
            path: c["path"].as_str().unwrap().to_string(),
            op: match c["op"].as_str().unwrap() {
                "added" => apic_history::ADDED,
                "removed" => apic_history::REMOVED,
                _ => apic_history::CHANGED,
            },
            from: raw(c.get("from")),
            to: raw(c.get("to")),
        })
        .collect();
    let vars: Vec<VarInfo> = serde_json::from_value(misc["vars"].clone()).unwrap();
    let progress = Progress {
        req: Default::default(),
        attempt: 2,
        max: 5,
        failure: "status == 200 (actual: 503)".to_string(),
    };
    // The record shows times in UTC; here they come out in the machine's
    // zone, so each is put back before comparing.
    let in_utc = |mut s: String| -> String {
        for e in &entries {
            let utc = format_time(e.time.utc(), "", "2006-01-02 15:04:05");
            s = s.replace(&history_time(e.time), &utc);
        }
        s
    };
    let highlight = "{\n  \"key\": \"a:b\",\n  \"n\": -1.5e3,\n  \"ok\": true,\n  \"nil\": null,\n  \"esc\": \"q\\\"uote\" ,\n  \"é\": [1, 2.5, false],\n  \"open\": \"unterminated\n}\nnot json at all: the fox\n";
    for (pname, profile) in PROFILES {
        with_color_profile(profile, || {
            let t = Theme::default();
            let mut check = |key: &str, got: String| {
                let pointer = format!("/misc/out/{pname}/{key}");
                diffs.check(&golden, &pointer, &format!("{key} [{pname}]"), &got)
            };
            check("history_entries", in_utc(history_entries(&t, &entries)));
            check("history_entry", in_utc(history_entry(&entries[1])));
            check("changes", changes(&t, &diff, 0));
            check("changes_narrow", changes(&t, &diff, 10));
            check("variables", variables(&t, &vars, true));
            check("variables_bare", variables(&t, &vars, false));
            check("attempt", attempt(&t, &progress));
            check("section", section(&t, "headers"));
            let methods = [
                "GET", "HEAD", "OPTIONS", "POST", "PUT", "PATCH", "DELETE", "TRACE",
            ];
            check("methods", methods.iter().map(|m| t.method(m)).collect());
            check(
                "statuses",
                t.status(200, "OK")
                    + &t.status(301, "Moved")
                    + &t.status(500, "")
                    + &t.status_code(204)
                    + &t.status_code(399)
                    + &t.status_code(400),
            );
            check(
                "latencies",
                [0, 199, 200, 999, 1000]
                    .iter()
                    .map(|ms| t.latency(*ms))
                    .collect(),
            );
            check("marks", t.mark(true) + &t.mark(false));
            check("highlight", highlight_json(&t, highlight));
        });
    }
    diffs.finish(39);

    let plain = &golden["plain"];
    let list = |k: &str| -> Vec<String> {
        plain[k]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s.as_str().unwrap().to_string())
            .collect()
    };
    let sizes = [
        0,
        1023,
        1024,
        1536,
        1024 * 1024 - 1,
        1024 * 1024,
        5 * 1024 * 1024 + 300_000,
    ];
    assert_eq!(sizes.map(size).to_vec(), list("size"));
    assert_eq!(
        vec![indent("a\nb\n\n"), indent(""), indent("one")],
        list("indent")
    );
    assert_eq!(
        vec![
            truncate("héllo wörld", 5),
            truncate("a\nb", 10),
            truncate("abc", 3),
            truncate("abcd", 0)
        ],
        list("truncate")
    );
    assert_eq!(
        vec![mask(&vars[0]), mask(&vars[1]), mask(&vars[5])],
        list("mask")
    );
}
