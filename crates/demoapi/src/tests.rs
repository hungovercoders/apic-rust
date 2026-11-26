use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use apic_httpc::{CancelToken, Client, Request};
use apic_httpd::TestServer;
use serde_json::Value;

use crate::project::PROJECT;
use crate::{new, write_project};

fn server() -> TestServer {
    TestServer::new(new())
}

#[test]
fn status_out_of_range_falls_back_to_200() {
    let srv = server();
    let res = Client::new()
        .send(&Request::get(&format!("{}/status/9999", srv.url())))
        .unwrap();
    assert_eq!(res.status_code, 200);
}

/// What `call` returns: the status, the headers and the JSON body.
struct Reply {
    status: u16,
    headers: apic_httpc::Headers,
    body: Value,
}

impl Reply {
    fn header(&self, name: &str) -> &str {
        self.headers.get(name).unwrap_or("")
    }

    fn len(&self) -> usize {
        self.body.as_array().map_or(0, Vec::len)
    }
}

/// Sends one request with a bearer token and decodes the JSON body.
fn call(srv: &TestServer, method: &str, path: &str, body: &str, headers: &[(&str, &str)]) -> Reply {
    let mut req = Request::new(method, &format!("{}{path}", srv.url()))
        .header("Authorization", "Bearer mock-token")
        .body(body);
    if !body.is_empty() {
        req = req.header("Content-Type", "application/json");
    }
    for (k, v) in headers {
        req = req.header(k, v);
    }
    let res = Client::new().send(&req).unwrap();
    Reply {
        status: res.status_code,
        body: serde_json::from_slice(&res.body).unwrap_or(Value::Null),
        headers: res.headers,
    }
}

#[test]
fn todos_filter_and_paginate() {
    let srv = server();
    let res = call(&srv, "GET", "/todos", "", &[]);
    assert!(
        res.status == 200 && res.len() == 2 && res.header("X-Total-Count") == "2" && res.header("Link").is_empty(),
        "unfiltered: status={} body={}",
        res.status,
        res.body
    );
    let res = call(&srv, "GET", "/todos?done=true", "", &[]);
    assert!(res.len() == 1 && res.header("X-Total-Count") == "1", "done=true: {}", res.body);
    let res = call(&srv, "GET", "/todos?limit=1", "", &[]);
    let link = res.header("Link");
    assert!(
        res.len() == 1 && res.header("X-Total-Count") == "2" && link.contains("page=2") && link.contains(r#"rel="next""#),
        "limit=1: len={} link={link:?}",
        res.len()
    );
    assert_eq!(link, r#"</todos?limit=1&page=2>; rel="next""#);
    let res = call(&srv, "GET", "/todos?limit=1&page=2", "", &[]);
    assert!(
        res.len() == 1 && res.header("Link").is_empty(),
        "last page should carry no next link: {}",
        res.body
    );
    let res = call(&srv, "GET", "/todos?done=maybe", "", &[]);
    assert!(res.status == 400 && !res.body["error"].is_null(), "bad done value: {} {}", res.status, res.body);
    // Absurd page and limit values must not overflow: past the end is an
    // empty page, and limit is capped.
    for q in [
        "page=9223372036854775807&limit=9223372036854775807",
        "page=9223372036854775807",
        "page=3&limit=1",
    ] {
        let res = call(&srv, "GET", &format!("/todos?{q}"), "", &[]);
        assert!(
            res.status == 200 && res.body.is_array() && res.len() == 0,
            "{q}: status={} body={}, want an empty page",
            res.status,
            res.body
        );
    }
    for q in ["limit=1000", "limit=9223372036854775807"] {
        let res = call(&srv, "GET", &format!("/todos?{q}"), "", &[]);
        assert!(
            res.status == 200 && res.len() == 2,
            "{q}: a limit above the cap still lists everything that exists: status={} body={}",
            res.status,
            res.body
        );
    }
}

#[test]
fn create_todo_validates() {
    let srv = server();
    let res = call(&srv, "POST", "/todos", r#"{"title": "  "}"#, &[]);
    assert_eq!(res.status, 422, "{}", res.body);
    assert!(
        res.body["error"] == "validation failed" && res.body["fields"]["title"] == "must not be empty",
        "body = {}",
        res.body
    );
    let res = call(&srv, "POST", "/todos", r#"{"title": "ok"}"#, &[]);
    assert!(res.status == 201 && res.body["id"] == "3", "valid create: {} {}", res.status, res.body);
}

#[test]
fn jobs_advance_per_poll() {
    let srv = server();
    let res = call(&srv, "POST", "/jobs", "", &[]);
    assert!(
        res.status == 202 && res.body["state"] == "queued" && res.header("Location") == "/jobs/1",
        "create: {} {}",
        res.status,
        res.body
    );
    for (i, want) in ["running", "done", "done"].iter().enumerate() {
        let res = call(&srv, "GET", "/jobs/1", "", &[]);
        assert_eq!(res.body["state"], *want, "poll {}", i + 1);
    }
    assert_eq!(call(&srv, "GET", "/jobs/99", "", &[]).status, 404, "unknown job");
}

#[test]
fn upload_echoes_parts() {
    let srv = server();
    let body = "--XX\r\nContent-Disposition: form-data; name=\"title\"\r\n\r\nQuarterly report\r\n--XX\r\nContent-Disposition: form-data; name=\"file\"; filename=\"report.txt\"\r\nContent-Type: application/octet-stream\r\n\r\nhello\r\n--XX--\r\n";
    let req = Request::post(&format!("{}/upload", srv.url()), body)
        .header("Content-Type", "multipart/form-data; boundary=XX");
    let res = Client::new().send(&req).unwrap();
    assert_eq!(
        String::from_utf8_lossy(&res.body),
        "{\"fields\":{\"title\":\"Quarterly report\"},\"files\":[{\"field\":\"file\",\"filename\":\"report.txt\",\"size\":5,\"content_type\":\"application/octet-stream\"}]}\n"
    );
    assert_eq!(res.status_code, 201);

    let res = call(&srv, "POST", "/upload", r#"{"not": "multipart"}"#, &[]);
    assert!(res.status == 400 && !res.body["error"].is_null(), "non-multipart: {} {}", res.status, res.body);
}

#[test]
fn graphql() {
    let srv = server();
    let res = call(
        &srv,
        "POST",
        "/graphql",
        r#"{"query": "{ todos(done: $done) { id } }", "variables": {"done": true}}"#,
        &[],
    );
    assert!(
        res.status == 200 && res.body["data"]["todos"].as_array().map_or(0, Vec::len) == 1,
        "todos query: {} {}",
        res.status,
        res.body
    );
    let res = call(&srv, "POST", "/graphql", r#"{"query": "{ nope }"}"#, &[]);
    assert!(!res.body["errors"].is_null(), "unknown field should report errors: {}", res.body);
    let res = call(&srv, "POST", "/graphql", "not json", &[]);
    assert!(res.status == 400 && !res.body["errors"].is_null(), "bad body: {} {}", res.status, res.body);
}

#[test]
fn api_key_csv_slow_and_health() {
    let srv = server();
    assert_eq!(call(&srv, "GET", "/keyed", "", &[]).status, 401, "no key");
    let res = call(&srv, "GET", "/keyed", "", &[("X-Api-Key", "demo-key")]);
    assert!(res.status == 200 && res.body["ok"] == true, "with key: {} {}", res.status, res.body);

    let csv = Client::new()
        .send(&Request::get(&format!("{}/reports/daily.csv", srv.url())))
        .unwrap();
    let body = String::from_utf8_lossy(&csv.body);
    assert!(
        csv.headers.get("Content-Type").unwrap_or("").starts_with("text/csv")
            && body.starts_with("date,total,done,open\n")
            && body.ends_with(",2,1,1\n"),
        "csv: body={body:?}"
    );

    let start = Instant::now();
    let res = call(&srv, "GET", "/slow?ms=50", "", &[]);
    assert!(
        res.status == 200 && res.body["slept_ms"] == 50 && start.elapsed() >= Duration::from_millis(50),
        "slow: {} {} after {:?}",
        res.status,
        res.body,
        start.elapsed()
    );

    let res = call(&srv, "GET", "/health", "", &[]);
    assert!(
        res.status == 200
            && res.body["status"] == "ok"
            && !res.body["uptime_seconds"].is_null()
            && res.body["version"] == "dev",
        "health: {} {}",
        res.status,
        res.body
    );
}

/// The responses read as they always have, byte for byte.
#[test]
fn response_bodies() {
    let srv = server();
    let text = |method: &str, path: &str, body: &str| {
        let req = Request::new(method, &format!("{}{path}", srv.url()))
            .header("Authorization", "Bearer t")
            .body(body);
        let res = Client::new().send(&req).unwrap();
        format!("{} {}", res.status_code, String::from_utf8_lossy(&res.body))
    };
    assert_eq!(
        text("POST", "/auth/login", r#"{"USER":"alice","password":"s3cret"} trailing"#),
        "200 {\"access_token\":\"mock-token\"}\n"
    );
    assert_eq!(text("POST", "/auth/login", "{}"), "401 {\"error\":\"bad credentials\"}\n");
    assert_eq!(text("GET", "/me", ""), "200 {\"email\":\"alice@example.com\"}\n");
    assert_eq!(
        text("GET", "/todos", ""),
        "200 [{\"id\":\"1\",\"title\":\"Buy milk\",\"done\":false},{\"id\":\"2\",\"title\":\"Read the apic docs\",\"done\":true}]\n"
    );
    assert_eq!(
        text("PUT", "/todos/1", r#"{"done":true,"title":"<b> & co"}"#),
        "200 {\"id\":\"1\",\"title\":\"\\u003cb\\u003e \\u0026 co\",\"done\":true}\n"
    );
    assert_eq!(text("DELETE", "/todos/1", ""), "204 ");
    assert_eq!(text("GET", "/todos/1", ""), "404 {\"error\":\"not found\"}\n");
    assert_eq!(text("GET", "/todos/02", ""), "404 {\"error\":\"not found\"}\n");
    assert_eq!(text("GET", "/todos?done=true&page=2", ""), "200 []\n");
    assert_eq!(text("GET", "/status/418", ""), "418 {\"status\":418}\n");
    assert_eq!(text("GET", "/keyed", ""), "401 {\"error\":\"missing or bad X-Api-Key\"}\n");
    assert_eq!(
        text("POST", "/graphql", r#"{"query":"{ health todos }","variables":{"done":false}}"#),
        "200 {\"data\":{\"health\":\"ok\",\"todos\":[]}}\n"
    );
    assert_eq!(
        text("POST", "/graphql", r#"{"query":"{ health }","variables":[]}"#),
        "400 {\"errors\":[{\"message\":\"expected a JSON body with a query\"}]}\n"
    );
    assert_eq!(
        text("POST", "/oauth/token", ""),
        "401 {\"error\":\"invalid_client\"}\n"
    );
    let res = Client::new()
        .send(&Request::get(&format!("{}/redirect", srv.url())))
        .unwrap();
    assert_eq!((res.status_code, res.headers.get("Location")), (302, Some("/status/200")));
}

/// Runs every request in `project/` as one flow against a live instance
/// of the demo API, so the bundled example stays correct without needing
/// network access.
#[test]
fn examples_suite() {
    let srv = server();
    let p = apic_project::load(concat!(env!("CARGO_MANIFEST_DIR"), "/project")).unwrap();
    let r = apic_runner::Runner::new(
        p.clone(),
        apic_runner::Options {
            vars: BTreeMap::from([("baseUrl".to_string(), srv.url())]),
            no_session: true,
            keep_going: true,
            ..apic_runner::Options::default()
        },
    )
    .unwrap();
    let (results, err) = Arc::new(r).run_all(&CancelToken::new(), &p.requests());
    assert!(err.is_none(), "{err:?}");
    for res in &results {
        let name = res.request.name.as_str();
        let want_fail = name == "not-found";
        assert!(
            res.ok != want_fail,
            "{name}: ok={}, errors={:?}, asserts={:?}",
            res.ok,
            res.errors,
            res.asserts
        );
    }
}

/// Runs the bundled features/ against the demo API, so `apic test -C
/// apic-demo` keeps working offline.
#[test]
fn example_features() {
    let srv = server();
    let p = apic_project::load(concat!(env!("CARGO_MANIFEST_DIR"), "/project")).unwrap();
    let cfg = apic_bdd::Config {
        vars: BTreeMap::from([("baseUrl".to_string(), srv.url())]),
        ..apic_bdd::Config::new(p)
    };
    let (sum, _, code, err) = apic_bdd::run_summary(&CancelToken::new(), &apic_bdd::Options::new(cfg));
    assert!(err.is_none(), "{err:?}");
    let sum = sum.expect("a summary");
    assert!(
        code == 0 && sum.ok && sum.scenarios == 8 && sum.passed == 8,
        "code={code} summary={sum:?}"
    );
}

/// The demo project is what a lesson, a screenshot and an agent's first
/// session see, and `apic fmt` is what they run after an edit, so it must
/// already be in canonical form or the formatter touches what they did
/// not.
#[test]
fn project_is_formatted() {
    for (path, data) in PROJECT {
        if !path.ends_with(".http") {
            continue;
        }
        let src = String::from_utf8_lossy(data).replace("\r\n", "\n");
        assert_eq!(
            apic_httpfile::format(&src),
            src,
            "{path} is not formatted; run `apic fmt -C crates/demoapi/project`"
        );
    }
}

#[test]
fn write_project_scaffolds() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().to_string_lossy().into_owned();

    let (written, skipped) = write_project(&dir, 9999, false).unwrap();
    assert!(skipped.is_empty(), "first write should skip nothing, got {skipped:?}");
    let want: Vec<String> = [
        "apic.yaml",
        "auth.http",
        "explore.http",
        "features/todos.feature",
        "http-client.private.env.json",
        "jobs.http",
        "todos.http",
        "http-client.env.json",
    ]
    .iter()
    .map(|f| tmp.path().join(f).to_string_lossy().into_owned())
    .collect();
    assert_eq!(written, want);

    let env = std::fs::read_to_string(tmp.path().join("http-client.env.json")).unwrap();
    assert_eq!(env, "{\n  \"local\": {\n    \"baseUrl\": \"http://localhost:9999\"\n  }\n}\n");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(tmp.path().join("http-client.private.env.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "http-client.private.env.json mode");
    }

    let p = apic_project::load(&dir).unwrap();
    let diags = p.validate();
    assert!(diags.is_empty(), "scaffolded project has diagnostics: {diags:?}");

    // Re-running without force leaves existing files alone.
    let (written, skipped) = write_project(&dir, 9999, false).unwrap();
    assert!(
        written.is_empty() && skipped.len() == want.len(),
        "without force: written={written:?} skipped={skipped:?}"
    );

    // With force, every file is rewritten.
    let (written, skipped) = write_project(&dir, 9999, true).unwrap();
    assert!(
        skipped.is_empty() && written.len() == want.len(),
        "with force: written={written:?} skipped={skipped:?}"
    );
}
