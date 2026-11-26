use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use apic_httpd::{Mux, Request as HttpRequest, Response as HttpResponse, TestServer};

use super::*;
use crate::{sleep_cancel, Progress, SleepError};

/// Answers "running" for the first two polls and "done" after, and counts
/// the polls.
fn job_server() -> (TestServer, Arc<AtomicUsize>) {
    let polls = Arc::new(AtomicUsize::new(0));
    let count = polls.clone();
    let mut mux = Mux::new();
    mux.handle_fn("GET /job", move |_| {
        let n = count.fetch_add(1, Ordering::SeqCst) + 1;
        let state = if n >= 3 { "done" } else { "running" };
        HttpResponse::json(200, format!(r#"{{"state":"{state}","poll":{n}}}"#))
    })
    .unwrap();
    (TestServer::new(mux.into_handler()), polls)
}

const RETRY_HTTP: &str = r#"
### Wait for the job
# @name wait
# @retry 5 250ms
# @assert status == 200
# @assert body.$.state == done
# @capture poll = body.$.poll
GET {{baseUrl}}/job

### Not enough attempts
# @name short
# @retry 2 10ms
# @assert body.$.state == done
GET {{baseUrl}}/job

### No policy of its own
# @name plain
# @assert body.$.state == done
GET {{baseUrl}}/job

### Bad policy
# @name bad
# @retry soon
GET {{baseUrl}}/job
"#;

struct Retry {
    r: Runner,
    polls: Arc<AtomicUsize>,
    sleeper: Arc<FakeSleeper>,
    _srv: TestServer,
    _dir: tempfile::TempDir,
}

impl Retry {
    fn polls(&self) -> usize {
        self.polls.load(Ordering::SeqCst)
    }
}

fn retry_runner(mut opts: Options, files: &[(&str, &str)]) -> Retry {
    let (srv, polls) = job_server();
    let env = format!(r#"{{"dev": {{"baseUrl": "{}"}}}}"#, srv.url());
    let mut all = vec![
        ("api.http", RETRY_HTTP),
        ("http-client.env.json", env.as_str()),
    ];
    all.extend_from_slice(files);
    let dir = write_project(&all);
    opts.env = "dev".into();
    if opts.session.is_none() {
        opts.session = Some(memory_session());
    }
    let mut r = new_runner(dir.path(), opts);
    let sleeper = FakeSleeper::install(&mut r);
    Retry {
        r,
        polls,
        sleeper,
        _srv: srv,
        _dir: dir,
    }
}

#[test]
fn retry_resends_until_the_assertions_pass() {
    let mut t = retry_runner(Options::default(), &[]);
    let progress: Arc<Mutex<Vec<Progress>>> = Arc::default();
    let sink = progress.clone();
    t.r.progress = Some(Box::new(move |p| sink.lock().unwrap().push(p.clone())));
    let res = run_ok(&t.r, "wait");
    assert!(res.ok, "{res:?}");
    assert!(res.attempts == 3 && t.polls() == 3);
    let waits = t.sleeper.waits();
    assert!(
        waits.len() == 2 && waits[0] == Duration::from_millis(250),
        "{waits:?}"
    );
    let progress = progress.lock().unwrap();
    assert!(
        progress.len() == 2
            && progress[0].attempt == 1
            && progress[0].max == 5
            && progress[1].attempt == 2
            && progress[0].req.name == "wait",
        "progress = {progress:?}"
    );
    assert_eq!(
        progress[0].failure,
        r#"body.$.state == done: got "running""#
    );
    // Captures come from the attempt that passed, and only that one is
    // committed to the session.
    assert_eq!(res.captures["poll"], "3");
    let session = t.r.session.as_ref().unwrap().lock().unwrap();
    assert_eq!(session.get("dev", "poll"), Some("3"));
    // The passing response is what the result shows.
    assert!(String::from_utf8_lossy(&res.display_raw_body()).contains(r#""done""#));
}

#[test]
fn retry_exhausted_reports_the_last_attempt() {
    let t = retry_runner(Options::default(), &[]);
    let (res, err) = run(&t.r, "short");
    assert!(
        err.is_none(),
        "exhausting the attempts is an assertion failure, not an error: {err:?}"
    );
    let res = res.unwrap();
    assert!(!res.ok && res.attempts == 2 && t.polls() == 2);
    assert!(
        res.asserts.len() == 1 && !res.asserts[0].pass && res.asserts[0].actual == "running",
        "asserts = {:?}",
        res.asserts
    );
}

#[test]
fn retry_policy_precedence() {
    // No directive, no flag, no config: one attempt, no attempts count.
    let t = retry_runner(Options::default(), &[]);
    let res = run_ok(&t.r, "plain");
    assert!(!res.ok && res.attempts == 0 && t.polls() == 1);
    // apic.yaml supplies a default.
    let t = retry_runner(Options::default(), &[("apic.yaml", "retry: 4 100ms\n")]);
    let res = run_ok(&t.r, "plain");
    assert!(
        res.ok
            && res.attempts == 3
            && t.polls() == 3
            && t.sleeper.waits()[0] == Duration::from_millis(100),
        "config: {res:?}"
    );
    // --retry beats apic.yaml; the directive beats both.
    let retry = |n: &str| Options {
        retry: n.into(),
        ..Options::default()
    };
    let t = retry_runner(retry("3"), &[("apic.yaml", "retry: 1\n")]);
    let res = run_ok(&t.r, "plain");
    assert!(
        res.ok && res.attempts == 3 && t.sleeper.waits()[0] == Duration::from_secs(1),
        "flag: {res:?}"
    );
    let t = retry_runner(retry("9"), &[]);
    let res = run_ok(&t.r, "short");
    assert!(
        !res.ok && res.attempts == 2,
        "directive beats flag: {res:?}"
    );
    // --no-retry switches every policy off.
    let t = retry_runner(
        Options {
            no_retry: true,
            ..retry("9")
        },
        &[("apic.yaml", "retry: 9\n")],
    );
    let res = run_ok(&t.r, "wait");
    assert!(
        !res.ok && res.attempts == 0 && t.polls() == 1,
        "no-retry: {res:?}"
    );
    // Bad policies are usage errors that name their source.
    let t = retry_runner(retry("lots"), &[]);
    for (name, want, code) in [
        ("bad", r#"bad @retry "soon""#, crate::Code::Directive),
        ("plain", r#"--retry "lots""#, crate::Code::Flag),
    ] {
        let err = run_err(&t.r, name);
        assert!(
            err.usage().is_some() && err.to_string().contains(want),
            "{name}: want usage error with {want:?}, got {err}"
        );
        assert_eq!(crate::code_of(&err), code);
    }
    let t = retry_runner(Options::default(), &[("apic.yaml", "retry: never\n")]);
    let err = run_err(&t.r, "plain");
    assert!(
        err.to_string().contains(r#"apic.yaml: bad retry "never""#),
        "config: got {err}"
    );
}

#[test]
fn retry_covers_transport_errors_and_stops_on_cancel() {
    // A server that is not listening: every attempt is a transport error.
    let addr = {
        let ln = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        ln.local_addr().unwrap().to_string()
    };
    let dir = write_project(&[
        (
            "api.http",
            &format!("### down\n# @name down\n# @retry 3 10ms\nGET http://{addr}/\n"),
        ),
        ("http-client.env.json", r#"{"dev": {}}"#),
    ]);
    let mut r = new_runner(
        dir.path(),
        Options {
            env: "dev".into(),
            session: Some(memory_session()),
            ..Options::default()
        },
    );
    let sleeper = FakeSleeper::install(&mut r);
    let attempts = Arc::new(AtomicUsize::new(0));
    let count = attempts.clone();
    r.progress = Some(Box::new(move |_| {
        count.fetch_add(1, Ordering::SeqCst);
    }));
    let err = run_err(&r, "down");
    assert!(
        err.transport().is_some()
            && attempts.load(Ordering::SeqCst) == 2
            && sleeper.waits().len() == 2,
        "want a transport error after 3 attempts, got {err} (waits {:?})",
        sleeper.waits()
    );
    // Cancelled while waiting between attempts: a transport error, at
    // once.
    *sleeper.err.lock().unwrap() = Some(SleepError::Cancelled);
    sleeper.waits.lock().unwrap().clear();
    let err = run_err(&r, "down");
    assert!(
        err.transport().is_some() && err.is_cancelled() && sleeper.waits().len() == 1,
        "want cancellation surfaced as a transport error after one wait, got {err} (waits {:?})",
        sleeper.waits()
    );
}

#[test]
fn sleep_cancel_honours_cancellation() {
    let cancelled = ctx();
    cancelled.cancel();
    assert_eq!(
        sleep_cancel(&cancelled, Duration::from_secs(60)),
        Err(SleepError::Cancelled)
    );
    assert_eq!(
        sleep_cancel(&cancelled, Duration::ZERO),
        Err(SleepError::Cancelled)
    );
    assert_eq!(sleep_cancel(&ctx(), Duration::ZERO), Ok(()));
    let start = Instant::now();
    assert_eq!(sleep_cancel(&ctx(), Duration::from_millis(5)), Ok(()));
    assert!(start.elapsed() >= Duration::from_millis(5));
}

#[test]
fn retry_runs_each_attempt_with_its_own_timeout() {
    // The first attempt hangs past the timeout; the second answers at
    // once.
    let n = AtomicUsize::new(0);
    let srv = TestServer::new(move |_: &HttpRequest| {
        if n.fetch_add(1, Ordering::SeqCst) == 0 {
            std::thread::sleep(Duration::from_millis(600));
            return HttpResponse::new(200);
        }
        HttpResponse::new(200).with_body("ok")
    });
    let dir = write_project(&[
        (
            "api.http",
            &format!(
                "### slow\n# @name slow\n# @retry 2 0s\n# @timeout 100ms\n# @assert status == 200\nGET {}/\n",
                srv.url()
            ),
        ),
        ("http-client.env.json", r#"{"dev": {}}"#),
    ]);
    let r = new_runner(
        dir.path(),
        Options {
            env: "dev".into(),
            session: Some(memory_session()),
            ..Options::default()
        },
    );
    let res = run_ok(&r, "slow");
    assert!(res.ok && res.attempts == 2, "{res:?}");
    // On its own the first attempt is a timeout.
    let slow = TestServer::new(|_: &HttpRequest| {
        std::thread::sleep(Duration::from_millis(400));
        HttpResponse::new(200)
    });
    std::fs::write(
        dir.path().join("api.http"),
        format!(
            "### slow\n# @name slow\n# @timeout 50ms\nGET {}/\n",
            slow.url()
        ),
    )
    .unwrap();
    let r = new_runner(
        dir.path(),
        Options {
            env: "dev".into(),
            no_session: true,
            ..Options::default()
        },
    );
    let err = run_err(&r, "slow");
    assert_eq!(crate::code_of(&err), crate::Code::Timeout, "{err}");
}
