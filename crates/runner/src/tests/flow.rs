use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use apic_httpd::{Request as HttpRequest, Response as HttpResponse, TestServer};

use super::*;
use crate::SleepError;

const FLOW_HTTP: &str = r#"
### One
# @name one
GET {{baseUrl}}/one

### Kept out of the flow
# @name off
# @disabled
GET {{baseUrl}}/off

### Rate limited
# @name slow
# @sleep 1500ms
GET {{baseUrl}}/slow

### Bad sleep
# @name bad-sleep
# @sleep soon
GET {{baseUrl}}/slow
"#;

struct Flow {
    r: Runner,
    hits: Arc<HashMap<&'static str, AtomicUsize>>,
    sleeper: Arc<FakeSleeper>,
    _srv: TestServer,
    _dir: tempfile::TempDir,
}

impl Flow {
    fn hits(&self, path: &str) -> usize {
        self.hits[path].load(Ordering::SeqCst)
    }
}

fn flow_runner() -> Flow {
    let hits: Arc<HashMap<&'static str, AtomicUsize>> = Arc::new(
        ["/one", "/off", "/slow"]
            .into_iter()
            .map(|p| (p, AtomicUsize::new(0)))
            .collect(),
    );
    let seen = hits.clone();
    let srv = TestServer::new(move |r: &HttpRequest| {
        if let Some(n) = seen.get(r.path.as_str()) {
            n.fetch_add(1, Ordering::SeqCst);
        }
        HttpResponse::new(200)
    });
    let dir = write_project(&[
        ("api.http", FLOW_HTTP),
        (
            "http-client.env.json",
            &format!(r#"{{"dev": {{"baseUrl": "{}"}}}}"#, srv.url()),
        ),
    ]);
    let mut r = new_runner(
        dir.path(),
        Options {
            env: "dev".into(),
            no_session: true,
            keep_going: true,
            ..Options::default()
        },
    );
    let sleeper = FakeSleeper::install(&mut r);
    Flow {
        r,
        hits,
        sleeper,
        _srv: srv,
        _dir: dir,
    }
}

/// A file run as a flow skips a `# @disabled` request and says so; naming
/// it sends it.
#[test]
fn disabled_is_skipped_by_flows() {
    let mut f = flow_runner();
    let reqs = f.r.target("api.http").unwrap();
    let seen: Arc<Mutex<Vec<String>>> = Arc::default();
    let sink = seen.clone();
    f.r.on_result = Some(Box::new(move |res, _| {
        sink.lock()
            .unwrap()
            .push(format!("{}:{}", res.request.name, res.skipped));
    }));
    let (results, err) = f.r.run_all(&ctx(), &reqs);
    let err = err.expect("the bad @sleep is a usage error");
    assert!(err.to_string().contains("@sleep"), "{err}");
    assert!(
        results.len() == 4
            && results[1].skipped == "disabled"
            && results[1].ok
            && results[1].response.is_none(),
        "results: {results:?}"
    );
    assert!(f.hits("/off") == 0 && f.hits("/one") == 1);
    assert_eq!(
        seen.lock().unwrap().join(","),
        "one:,off:disabled,slow:,bad-sleep:"
    );

    // By name, or file#name, it goes out.
    for target in ["off", "api.http#off", "api.http#2"] {
        let reqs = f.r.target(target).unwrap();
        let (results, err) = f.r.run_all(&ctx(), &reqs);
        assert!(
            err.is_none()
                && results.len() == 1
                && results[0].skipped.is_empty()
                && results[0].response.is_some(),
            "{target}: {results:?} {err:?}"
        );
    }
    assert_eq!(f.hits("/off"), 3);
}

/// `# @sleep` waits before sending, through the runner's sleeper, and a
/// cancelled wait sends nothing.
#[test]
fn sleep_before_sending() {
    let f = flow_runner();
    let r = &f.r;
    let req = lookup(r, "slow");
    assert!(run_ok(r, "slow").ok);
    assert_eq!(f.sleeper.waits(), [Duration::from_millis(1500)]);
    assert_eq!(f.hits("/slow"), 1);
    let d = r.describe(&req);
    assert!(d.sleep == "1500ms" && !d.disabled, "{d:?}");
    *f.sleeper.err.lock().unwrap() = Some(SleepError::Cancelled);
    let err = run_err(r, "slow");
    assert!(
        err.transport().is_some() && err.is_cancelled() && f.hits("/slow") == 1,
        "a cancelled sleep: {err}"
    );
    assert_eq!(crate::code_of(&err), crate::Code::Cancelled);
    assert_eq!(err.to_string(), "request failed: context canceled");
    assert!(r.describe(&lookup(r, "off")).disabled);
    let codes: Vec<String> = r
        .project
        .validate()
        .iter()
        .map(|d| format!("{}@{}:{}", d.code, d.line, d.column))
        .collect();
    assert_eq!(codes.join(","), "bad-sleep@18:10");
}

/// Ctrl-C ends a flow where it is, keep_going or not: every request left
/// would fail the same way.
#[test]
fn a_cancelled_flow_stops_at_once() {
    let f = flow_runner();
    let cancel = ctx();
    cancel.cancel();
    let reqs = vec![lookup(&f.r, "one"), lookup(&f.r, "slow")];
    let (results, err) = f.r.run_all(&cancel, &reqs);
    let err = err.expect("a cancelled run is an error");
    assert!(err.is_cancelled(), "{err}");
    assert_eq!(crate::code_of(&err), crate::Code::Cancelled);
    assert_eq!(exit(Some(&err)), crate::EXIT_TRANSPORT);
    assert_eq!(results.len(), 1, "the flow went on after the cancel");
    assert_eq!(
        results[0].error.as_ref().map(|e| e.code),
        Some(crate::Code::Cancelled)
    );
    assert_eq!(f.hits("/one"), 0);
}
