use std::path::Path;
use std::sync::LazyLock;

use apic_gocompat::Time;
use apic_httpc::CancelToken;
use apic_httpd::{Mux, Response, TestServer};
use apic_runner::{Options, Result as RunResult, Runner};
use regex::Regex;

use crate::{features, run, Meta};

/// Compares `got` with testdata/`name`, rewriting it when UPDATE_GOLDEN is
/// set. Both sides are read with CRLF folded to LF, so the comparison is
/// the same whatever line endings a checkout or a fixture carries. On a
/// mismatch the output is left beside the golden as `name`.got; a stale
/// .got from an earlier failure is removed when the test passes.
fn golden(name: &str, got: &str) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("testdata")
        .join(name);
    let got = got.replace("\r\n", "\n");
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::write(&path, got).unwrap();
        return;
    }
    let want = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{e} (run with UPDATE_GOLDEN=1 to create it)"))
        .replace("\r\n", "\n");
    let got_path = path.with_file_name(format!("{name}.got"));
    if want == got {
        let _ = std::fs::remove_file(&got_path);
        return;
    }
    let place = match std::fs::write(&got_path, &got) {
        Ok(()) => format!("the output is in {}", got_path.display()),
        Err(e) => format!("the output could not be written beside it: {e}"),
    };
    panic!("{name} differs from the golden file; {place} (run with UPDATE_GOLDEN=1 to accept it)");
}

fn api_server() -> TestServer {
    let mut mux = Mux::new();
    mux.handle_fn("POST /login", |_| {
        Response::json(200, r#"{"token":"t-1","user":{"name":"alice"}}"#)
            .with_header("Set-Cookie", "sid=abc")
    })
    .unwrap();
    mux.handle_fn("GET /me", |r| {
        if r.header("Authorization") != Some("Bearer t-1") {
            return Response::error(401, r#"{"error":"unauthorised"}"#);
        }
        Response::json(200, r#"{"name":"alice","roles":["admin"]}"#)
    })
    .unwrap();
    mux.handle_fn("GET /missing", |_| Response::error(404, "nothing here"))
        .unwrap();
    TestServer::new(mux.into_handler())
}

fn results(redact: bool) -> (Vec<RunResult>, String) {
    let srv = api_server();
    let url = srv.url();
    let dir = tempfile::tempdir().unwrap();
    let files = [
        (
            "http-client.env.json",
            format!(r#"{{"dev": {{"baseUrl": "{url}"}}}}"#),
        ),
        (
            "http-client.private.env.json",
            r#"{"dev": {"password": "s3cret"}}"#.to_string(),
        ),
        (
            "api.http",
            r#"
### Log in
# @name login
# @assert status == 200
# @capture token = body.$.token
POST {{baseUrl}}/login
Content-Type: application/json

{"user": "alice", "password": "{{password}}"}

### Who am I
# @name me
# @ref login
# @assert status == 200
# @assert body.$.name == alice
GET {{baseUrl}}/me
Authorization: Bearer {{token}}

### Not there
# @name missing
# @assert status == 200
# @assert header.content-type contains json
GET {{baseUrl}}/missing
"#
            .to_string(),
        ),
    ];
    for (name, content) in &files {
        std::fs::write(dir.path().join(name), content).unwrap();
    }
    let p = apic_project::load(dir.path()).unwrap();
    let r = Runner::new(
        p,
        Options {
            env: "dev".into(),
            no_session: true,
            redact,
            keep_going: true,
            ..Options::default()
        },
    )
    .unwrap();
    let mut out = Vec::new();
    for name in ["me", "missing"] {
        let req = r.project.lookup(name).unwrap();
        let (res, err) = r.run(&CancelToken::new(), &req);
        assert!(err.is_none(), "{err:?}");
        out.push(res.unwrap());
    }
    (out, url)
}

static MS_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b\d+ ms\b").unwrap());
static DATE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<th>date</th><td>[^<]*</td>").unwrap());

/// Strips what varies between runs: the server port and the measured
/// times.
fn normalise(html: &str, server_url: &str) -> String {
    let s = html.replace(server_url, "http://api.test");
    let lines: Vec<String> = s
        .split('\n')
        .map(|line| {
            let line = if line.contains(r#"class="timings""#) {
                r#"<div class="timings">(timings)</div>"#
            } else {
                line
            };
            DATE_RE
                .replace_all(line, "<th>date</th><td>(date)</td>")
                .into_owned()
        })
        .collect();
    // "<n> ms" durations are measured; pin them.
    MS_RE.replace_all(&lines.join("\n"), "0 ms").into_owned()
}

fn at(year: i64, month: u32, day: u32, hour: u32) -> Time {
    Time::from_civil(year, month, day, hour, 0, 0, 0, 0)
}

#[test]
fn run_report_golden() {
    let (res, url) = results(false);
    let mut buf = Vec::new();
    let meta = Meta {
        version: "test".into(),
        env: "dev".into(),
        time: Some(at(2026, 9, 21, 12)),
        project: "/tmp/project".into(),
        ..Meta::default()
    };
    run(&mut buf, &meta, &res).unwrap();
    let html = String::from_utf8(buf).unwrap();
    for want in [
        "<!doctype html>",
        "1 failed",
        "ran first (# @ref)",
        "status == 200",
        "header.content-type contains json",
        "token = t-1",
        "&#34;name&#34;: &#34;alice&#34;",
        "Authorization",
        "***",
        "nothing here",
        "environment <b>dev</b>",
    ] {
        assert!(html.contains(want), "report lacks {want:?}");
    }
    // Bodies are shown in full without --redact, as run -v shows them;
    // the response's Set-Cookie is masked either way.
    assert!(
        !html.contains("sid=abc"),
        "set-cookie is shown although sensitive"
    );
    golden("run.golden.html", &normalise(&html, &url));
}

#[test]
fn run_report_redacted() {
    let (res, url) = results(true);
    let mut buf = Vec::new();
    let meta = Meta {
        version: "test".into(),
        env: "dev".into(),
        redacted: true,
        time: Some(Time::from_unix(0, 0)),
        ..Meta::default()
    };
    run(&mut buf, &meta, &res).unwrap();
    let html = String::from_utf8(buf).unwrap();
    for secret in ["t-1", "alice", "s3cret", "admin", "nothing here"] {
        assert!(
            !html.contains(secret),
            "redacted report contains {secret:?}"
        );
    }
    for want in [
        r#"class="badge redacted">redacted"#,
        "1 failed",
        "status == ***",
        "token = ***",
    ] {
        assert!(html.contains(want), "redacted report lacks {want:?}");
    }
    golden("run-redacted.golden.html", &normalise(&html, &url));
}

const CUCUMBER_FIXTURE: &str = r#"[{"uri":"features/users.feature","keyword":"Feature","name":"Users","elements":[
 {"keyword":"Background","name":"","type":"background","steps":[{"keyword":"Given ","name":"I am logged in","result":{"status":"passed","duration":1200000}}]},
 {"keyword":"Scenario","name":"Fetch a user","type":"scenario","steps":[
   {"keyword":"When ","name":"I fetch user 42","result":{"status":"passed","duration":800000}},
   {"keyword":"Then ","name":"the response status is 200","result":{"status":"passed","duration":10000}},
   {"keyword":"And ","name":"the response body matches:","doc_string":{"value":"{\"id\": 42}"},"result":{"status":"passed","duration":10000}}]},
 {"keyword":"Background","name":"","type":"background","steps":[{"keyword":"Given ","name":"I am logged in","result":{"status":"passed","duration":1000000}}]},
 {"keyword":"Scenario","name":"A missing user","type":"scenario","steps":[
   {"keyword":"When ","name":"I fetch user 999","result":{"status":"passed","duration":700000}},
   {"keyword":"Then ","name":"the response status is 200","result":{"status":"failed","duration":5000,"error_message":"expected status == 200, got \"404\"\n  GET http://api.test/users/999"}},
   {"keyword":"And ","name":"the variables:","rows":[{"cells":["name","value"]},{"cells":["a","1"]}],"result":{"status":"skipped"}}]},
 {"keyword":"Scenario","name":"Not written yet","type":"scenario","steps":[
   {"keyword":"When ","name":"I do something new","result":{"status":"undefined"}}]}
]}]"#;

#[test]
fn features_report_golden() {
    let mut buf = Vec::new();
    let meta = Meta {
        version: "test".into(),
        env: "dev".into(),
        time: Some(at(2026, 9, 21, 12)),
        ..Meta::default()
    };
    features(&mut buf, &meta, CUCUMBER_FIXTURE.as_bytes()).unwrap();
    let html = String::from_utf8(buf).unwrap();
    for want in [
        "2 failed",
        "3 scenarios · 9 steps",
        "Fetch a user",
        "expected status == 200",
        "undefined steps",
        "<td>name</td>",
        "&#34;id&#34;: 42",
        "I am logged in",
    ] {
        assert!(html.contains(want), "report lacks {want:?}");
    }
    golden("features.golden.html", &html);
    // An empty run is a report too.
    let mut buf = Vec::new();
    features(&mut buf, &meta, b"").unwrap();
    assert!(String::from_utf8(buf).unwrap().contains("all passed"));
    let mut buf = Vec::new();
    let err = features(&mut buf, &meta, b"nonsense").expect_err("bad JSON should be an error");
    assert!(
        err.to_string().starts_with("parse cucumber report: "),
        "{err}"
    );
}

#[test]
fn a_report_without_a_time_is_dated_now() {
    let mut buf = Vec::new();
    features(&mut buf, &Meta::default(), b" \n").unwrap();
    let html = String::from_utf8(buf).unwrap();
    let year = Time::now().civil().year.to_string();
    assert!(
        html.contains("<title>apic test · all passed</title>"),
        "{html}"
    );
    assert!(html.contains(&format!("<span>{year}-")), "{html}");
}

/// The parts of a result's JSON that a stored result does not keep but
/// the report shows.
#[derive(serde::Deserialize, Default)]
#[serde(default)]
struct Extras {
    skipped: String,
    ran_first: Vec<Box<serde_json::value::RawValue>>,
}

fn load(json: &str, redact: bool) -> RunResult {
    let mut res = apic_runner::parse_result(json).unwrap();
    let extras: Extras = serde_json::from_str(json).unwrap();
    res.skipped = extras.skipped;
    res.redact = redact;
    res.deps = extras
        .ran_first
        .iter()
        .map(|d| load(d.get(), redact))
        .collect();
    res
}

#[derive(serde::Deserialize, serde::Serialize)]
struct Wide {
    redact: bool,
    /// Each result as `apic run --json` printed it.
    results: Vec<String>,
    html: String,
}

/// A flow with every kind of result in it (retried, skipped, binary,
/// never answered, with dependencies), as recorded with the page it must
/// give. UPDATE_GOLDEN rewrites the page in the record.
#[test]
fn a_flow_of_every_kind_of_result_renders_as_recorded() {
    for name in ["run-wide.golden.json", "run-wide-redacted.golden.json"] {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("testdata")
            .join(name);
        let mut wide: Wide =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let results: Vec<RunResult> = wide.results.iter().map(|r| load(r, wide.redact)).collect();
        let meta = Meta {
            version: "1.2.3".into(),
            env: "dev".into(),
            redacted: wide.redact,
            time: Some(Time::from_civil(2026, 9, 21, 12, 0, 0, 5, 0)),
            project: "/tmp/<project>".into(),
            ..Meta::default()
        };
        let mut buf = Vec::new();
        run(&mut buf, &meta, &results).unwrap();
        let html = String::from_utf8(buf).unwrap();
        if html == wide.html {
            continue;
        }
        if std::env::var_os("UPDATE_GOLDEN").is_some() {
            wide.html = html;
            let mut text = serde_json::to_string_pretty(&wide).unwrap();
            text.push('\n');
            std::fs::write(&path, text).unwrap();
            continue;
        }
        let at = html
            .bytes()
            .zip(wide.html.bytes())
            .position(|(a, b)| a != b)
            .unwrap_or(html.len().min(wide.html.len()));
        let from = (0..=at.saturating_sub(200))
            .rev()
            .find(|i| html.is_char_boundary(*i) && wide.html.is_char_boundary(*i))
            .unwrap_or(0);
        let show = |s: &str| s[from..].chars().take(400).collect::<String>();
        panic!(
            "{name} differs at byte {at} (UPDATE_GOLDEN=1 accepts it)\n got: {}\nwant: {}",
            show(&html),
            show(&wide.html)
        );
    }
}
