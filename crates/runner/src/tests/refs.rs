use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use apic_httpd::{Mux, Response as HttpResponse, TestServer};

use super::*;
use crate::Code;

/// Counts logins so the tests can tell when `# @ref` ran one.
fn ref_server() -> (TestServer, Arc<AtomicUsize>) {
    let logins = Arc::new(AtomicUsize::new(0));
    let count = logins.clone();
    let mut mux = Mux::new();
    mux.handle_fn("POST /auth/login", move |_| {
        let n = count.fetch_add(1, Ordering::SeqCst) + 1;
        HttpResponse::json(200, format!(r#"{{"access_token":"tok-{n}"}}"#))
    })
    .unwrap();
    mux.handle_fn("GET /me", |r| {
        let authz = r.header("Authorization").unwrap_or("");
        let Some(token) = authz
            .strip_prefix("Bearer ")
            .filter(|t| t.starts_with("tok-"))
        else {
            return HttpResponse::error(401, r#"{"error":"unauthorised"}"#);
        };
        HttpResponse::json(
            200,
            format!(r#"{{"email":"alice@example.com","token":"{token}"}}"#),
        )
    })
    .unwrap();
    mux.handle_fn("GET /region", |_| {
        HttpResponse::json(200, r#"{"region":"eu"}"#)
    })
    .unwrap();
    mux.handle_fn("GET /fail", |_| {
        HttpResponse::error(503, r#"{"error":"nope"}"#)
    })
    .unwrap();
    (TestServer::new(mux.into_handler()), logins)
}

const REF_HTTP: &str = r#"
### Log in
# @name login
# @assert status == 200
# @capture token = body.$.access_token
POST {{baseUrl}}/auth/login

### Who am I
# @name whoami
# @ref login
# @assert status == 200
GET {{baseUrl}}/me
Authorization: Bearer {{token}}

### Always fresh
# @name whoami-fresh
# @forceRef login
# @assert status == 200
# @capture used = body.$.token
GET {{baseUrl}}/me
Authorization: Bearer {{token}}

### Region, then who am I
# @name region
# @capture region = body.$.region
GET {{baseUrl}}/region

### Two levels down
# @name whoami-in-region
# @ref whoami
# @ref region
# @assert status == 200
GET {{baseUrl}}/me?region={{region}}
Authorization: Bearer {{token}}

### A diamond: both paths lead to login, which must run once
# @name diamond
# @ref whoami
# @ref whoami-in-region
# @assert status == 200
GET {{baseUrl}}/me?region={{region}}
Authorization: Bearer {{token}}

### Depends on a failing request
# @name broken-login
# @assert status == 200
# @capture token = body.$.access_token
GET {{baseUrl}}/fail

### Needs the broken one
# @name after-broken
# @ref broken-login
GET {{baseUrl}}/me
Authorization: Bearer {{token}}

### httpyac style: a response reference to a request # @ref runs
# @name whoami-by-response
# @ref login
# @assert status == 200
GET {{baseUrl}}/me
Authorization: Bearer {{login.response.body.$.access_token}}

### The same without the ref
# @name whoami-by-response-alone
GET {{baseUrl}}/me
Authorization: Bearer {{login.response.body.$.access_token}}

### No such ref
# @name dangling
# @ref nope
GET {{baseUrl}}/me
Authorization: Bearer {{token}}

### Cycle a
# @name cycle-a
# @ref cycle-b
GET {{baseUrl}}/me
Authorization: Bearer {{fromB}}

### Cycle b
# @name cycle-b
# @ref cycle-a
GET {{baseUrl}}/me
Authorization: Bearer {{fromA}}

### Refers to itself
# @name selfish
# @ref selfish
GET {{baseUrl}}/me
Authorization: Bearer {{fromSelf}}
"#;

pub(crate) fn ref_project() -> (tempfile::TempDir, Arc<AtomicUsize>, TestServer) {
    let (srv, logins) = ref_server();
    let dir = write_project(&[
        ("api.http", REF_HTTP),
        (
            "http-client.env.json",
            &format!(r#"{{"dev": {{"baseUrl": "{}"}}}}"#, srv.url()),
        ),
    ]);
    (dir, logins, srv)
}

fn dev(session: crate::SharedSession) -> Options {
    Options {
        env: "dev".into(),
        session: Some(session),
        ..Options::default()
    }
}

fn names(rs: &[Result]) -> Vec<&str> {
    rs.iter().map(|r| r.request.name.as_str()).collect()
}

#[test]
fn ref_runs_the_dependency_when_a_variable_is_missing() {
    let (dir, logins, _srv) = ref_project();
    let logins = || logins.load(Ordering::SeqCst);
    let r = new_runner(dir.path(), dev(memory_session()));

    let res = run_ok(&r, "whoami");
    assert!(res.ok, "whoami: {res:?}");
    assert_eq!(logins(), 1);
    assert!(
        res.deps.len() == 1
            && res.deps[0].request.name == "login"
            && res.deps[0].ok
            && res.deps[0].req().is_some_and(|q| q.name == "login"),
        "deps = {:?}",
        res.deps
    );
    assert!(res.req().is_some_and(|q| q.name == "whoami"));
    // The dependency's result is in the JSON under ran_first.
    let out: serde_json::Value =
        serde_json::from_str(&serde_json::to_string(&res).unwrap()).unwrap();
    let first = out["ran_first"].as_array().expect("ran_first");
    assert!(
        first.len() == 1 && first[0]["request"]["name"] == "login" && first[0]["ok"] == true,
        "ran_first = {out}"
    );

    // The token is captured now: a second run does not log in again, and
    // reports no dependency.
    let res = run_ok(&r, "whoami");
    assert!(res.ok && res.deps.is_empty() && logins() == 1);

    // A fresh runner sharing the session finds the token and skips login.
    let r2 = new_runner(dir.path(), dev(r.session.clone().unwrap()));
    let res = run_ok(&r2, "whoami");
    assert!(res.ok && res.deps.is_empty() && logins() == 1);

    // Once the value is gone again (the UI keeps one runner across runs
    // and can clear the session), the ref runs again: "once" is per
    // invocation.
    r.session.as_ref().unwrap().lock().unwrap().clear("dev");
    r.captured.lock().unwrap().clear();
    let res = run_ok(&r, "whoami");
    assert!(
        res.ok && res.deps.len() == 1 && logins() == 2,
        "whoami after clearing: deps={:?} logins={}",
        names(&res.deps),
        logins()
    );
}

#[test]
fn force_ref_runs_every_time() {
    let (dir, logins, _srv) = ref_project();
    let r = new_runner(dir.path(), dev(memory_session()));
    for i in 1..=2 {
        let res = run_ok(&r, "whoami-fresh");
        assert!(res.ok && res.deps.len() == 1, "run {i}: {res:?}");
        assert_eq!(res.captures["used"], format!("tok-{i}"), "run {i}");
    }
    assert_eq!(logins.load(Ordering::SeqCst), 2);
}

#[test]
fn ref_runs_once_per_invocation_and_nests() {
    let (dir, logins, _srv) = ref_project();
    let r = new_runner(dir.path(), dev(memory_session()));
    let res = run_ok(&r, "whoami-in-region");
    assert!(res.ok, "{res:?}");
    // whoami ran first (pulling login in under it), then region.
    assert_eq!(names(&res.deps), ["whoami", "region"]);
    assert_eq!(names(&res.deps[0].deps), ["login"]);
    assert_eq!(logins.load(Ordering::SeqCst), 1);
    assert!(res.request.url.contains("region=eu"), "{}", res.request.url);
    // Once run, a ref is not run again by this runner even when a later
    // request is missing something else.
    let res = run_ok(&r, "whoami-in-region");
    assert!(
        res.ok && res.deps.is_empty(),
        "second run: {:?}",
        names(&res.deps)
    );
}

#[test]
fn ref_failure_stops_the_request() {
    let (dir, _logins, _srv) = ref_project();
    let r = new_runner(dir.path(), dev(memory_session()));
    let (res, err) = run(&r, "after-broken");
    assert!(
        err.is_none(),
        "an assertion failure in a dependency is not an error: {err:?}"
    );
    let res = res.unwrap();
    assert!(
        !res.ok && res.response.is_none() && res.deps.len() == 1 && !res.deps[0].ok,
        "{res:?}"
    );
    assert_eq!(res.errors, ["@ref broken-login failed"]);
    assert!(res.request.name == "after-broken" && res.req().is_some());
}

#[test]
fn ref_errors() {
    let (dir, _logins, _srv) = ref_project();
    let r = new_runner(dir.path(), dev(memory_session()));
    for (name, want) in [
        ("dangling", r#"@ref nope: no request named "nope""#),
        (
            "cycle-a",
            "@ref cycle-a is a cycle: cycle-a -> cycle-b -> cycle-a",
        ),
        ("selfish", "@ref selfish is a cycle: selfish -> selfish"),
    ] {
        let (res, err) = run(&r, name);
        let err = err.unwrap_or_else(|| panic!("{name}: no error"));
        assert!(
            err.usage().is_some() && err.to_string().contains(want),
            "{name}: want usage error containing {want:?}, got {err}"
        );
        assert_eq!(crate::code_of(&err), Code::Ref, "{name}");
        assert!(
            !res.is_some_and(|r| r.ok),
            "{name}: result should not be ok"
        );
    }
    // A dangling ref leaves a result that carries the error.
    let (res, _) = run(&r, "dangling");
    let res = res.expect("a result");
    assert_eq!(res.error.as_ref().unwrap().code, Code::Ref);
    assert_eq!(res.error.as_ref().unwrap().message, res.errors[0]);
    // Without a ref the hint still tells the user what to run.
    let dir2 = write_project(&[
        ("api.http", &REF_HTTP.replace("# @ref login\n", "")),
        (
            "http-client.env.json",
            r#"{"dev": {"baseUrl": "http://127.0.0.1:9"}}"#,
        ),
    ]);
    let r2 = new_runner(dir2.path(), dev(memory_session()));
    let err = run_err(&r2, "whoami");
    assert!(
        err.to_string().contains(r#"captured by request "login""#),
        "want the capture hint, got {err}"
    );
}

#[test]
fn describe_reports_refs() {
    let (dir, _logins, _srv) = ref_project();
    let r = new_runner(dir.path(), dev(memory_session()));
    let d = r.describe(&lookup(&r, "whoami"));
    assert_eq!(d.refs, ["login"]);
    let token = d
        .variables
        .iter()
        .find(|v| v.name == "token")
        .expect("the token variable");
    assert!(
        token.missing && token.captured_by == "login" && token.ref_runs,
        "token = {token:?}"
    );
    assert!(r.describe(&lookup(&r, "region")).refs.is_empty());
    // A dry run leaves what the ref would supply in place, and says so.
    let dry = r.dry_run(&lookup(&r, "whoami")).unwrap();
    assert!(dry.dry_run && dry.ok && dry.response.is_none());
    assert_eq!(
        dry.warnings,
        ["{{token}} is not set; a run would send login first (# @ref) and capture it"]
    );
    let dry = r.dry_run(&lookup(&r, "whoami-by-response")).unwrap();
    assert_eq!(
        dry.warnings,
        ["{{login.response.body.$.access_token}} is not set; a run would send login first (# @ref) and capture it"]
    );
    let err = r.dry_run(&lookup(&r, "dangling")).unwrap_err();
    assert_eq!(crate::code_of(&err), Code::MissingVariable);
}

/// Two dependencies that both lead to login share the "already ran" set
/// of the invocation, so login goes out once even though it is reachable
/// twice.
#[test]
fn ref_diamond_runs_shared_dependency_once() {
    let (dir, logins, _srv) = ref_project();
    let r = new_runner(dir.path(), dev(memory_session()));
    let res = run_ok(&r, "diamond");
    assert!(res.ok, "{res:?}");
    assert_eq!(logins.load(Ordering::SeqCst), 1);
    // whoami (with login under it), then whoami-in-region (with only
    // region under it: whoami had already run).
    assert_eq!(names(&res.deps), ["whoami", "whoami-in-region"]);
    assert_eq!(names(&res.deps[1].deps), ["region"]);
}

/// A response reference to a request that has not run is missing, like a
/// captured variable, so `# @ref` runs the request first: the way httpyac
/// files are written. Without the ref the error says to add one.
#[test]
fn ref_runs_for_a_response_reference() {
    let (dir, logins, _srv) = ref_project();
    let r = new_runner(dir.path(), dev(memory_session()));

    let d = r.describe(&lookup(&r, "whoami-by-response"));
    assert!(
        d.ready
            && d.variables.len() == 2
            && d.variables[0].name == "login.response.body.$.access_token"
            && d.variables[0].ref_runs,
        "describe: ready={} vars={:?}",
        d.ready,
        d.variables
    );
    let res = run_ok(&r, "whoami-by-response");
    assert!(
        res.ok && res.deps.len() == 1 && logins.load(Ordering::SeqCst) == 1,
        "{res:?}"
    );

    let r2 = new_runner(dir.path(), dev(memory_session()));
    let err = run_err(&r2, "whoami-by-response-alone");
    assert!(
        crate::code_of(&err) == Code::MissingVariable
            && err
                .to_string()
                .contains("request \"login\" has not run in this invocation; add `# @ref login`"),
        "want a missing-variable error suggesting @ref, got {err}"
    );
    assert_eq!(
        logins.load(Ordering::SeqCst),
        1,
        "login must not run without a ref"
    );
}
