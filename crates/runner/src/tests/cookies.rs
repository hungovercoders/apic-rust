use apic_httpd::{Mux, Response as HttpResponse, TestServer};

use super::*;
use crate::MASKED;

const COOKIE_HTTP: &str = r#"
### Log in with a form; the server answers with a session cookie
# @name form-login
# @assert status == 204
# @assert cookie.sid exists
# @capture sid = cookie.sid
POST {{baseUrl}}/login

### Only a browser-style session gets in
# @name me
# @assert status == 200
GET {{baseUrl}}/me

### The same request without the jar
# @name me-anon
# @no-cookies
# @assert status == 401
GET {{baseUrl}}/me
"#;

fn cookie_server() -> TestServer {
    let mut mux = Mux::new();
    mux.handle_fn("POST /login", |_| {
        HttpResponse::new(204).with_header("Set-Cookie", "sid=s-1; Path=/; HttpOnly")
    })
    .unwrap();
    mux.handle_fn("GET /me", |r| {
        if r.cookie("sid").as_deref() != Some("s-1") {
            return HttpResponse::error(401, "no session");
        }
        HttpResponse::json(200, r#"{"user":"alice"}"#)
    })
    .unwrap();
    TestServer::new(mux.into_handler())
}

#[test]
fn cookie_jar_is_opt_in_persisted_per_environment_and_skippable() {
    let srv = cookie_server();
    let url = srv.url();
    let dir = write_project(&[
        ("api.http", COOKIE_HTTP),
        (
            "http-client.env.json",
            &format!(r#"{{"dev": {{"baseUrl": "{url}"}}, "staging": {{"baseUrl": "{url}"}}}}"#),
        ),
    ]);
    let dir = dir.path();
    let opts = |env: &str, cookies: bool, no_session: bool| Options {
        env: env.into(),
        cookies,
        no_session,
        ..Options::default()
    };

    // Off by default: the cookie is visible to selectors but never sent.
    let r = new_runner(dir, opts("dev", false, true));
    assert!(r.jar.is_none(), "jar on without --cookies");
    let res = run_ok(&r, "form-login");
    assert!(
        res.ok && res.captures["sid"] == "s-1",
        "login without jar: {res:?}"
    );
    let res = run_ok(&r, "me");
    assert!(
        !res.ok && res.response.as_ref().unwrap().status == 401,
        "{res:?}"
    );

    // On: the cookie is sent with later requests, and survives the
    // process.
    let r = new_runner(dir, opts("dev", true, false));
    assert!(run_ok(&r, "form-login").ok);
    assert!(run_ok(&r, "me").ok, "me with jar");
    assert!(run_ok(&r, "me-anon").ok, "@no-cookies should send none");
    assert!(
        dir.join(".apic").join(apic_session::COOKIE_FILE).exists(),
        "jar not persisted"
    );
    let r2 = new_runner(dir, opts("dev", true, false));
    assert!(run_ok(&r2, "me").ok, "me in a new process");
    let got = r2.jar.as_ref().unwrap().cookies("dev");
    assert!(
        got.len() == 1 && got[0].name == "sid" && got[0].http_only,
        "stored = {got:?}"
    );

    // Per environment: staging has its own, empty, jar.
    let r3 = new_runner(dir, opts("staging", true, false));
    assert!(!run_ok(&r3, "me").ok, "staging should not see dev's cookie");

    // apic.yaml switches it on too; --no-session keeps the jar in memory
    // for the run without touching the file.
    std::fs::write(dir.join("apic.yaml"), "cookies: true\n").unwrap();
    let r4 = new_runner(dir, opts("staging", false, true));
    assert!(
        r4.jar.is_some(),
        "apic.yaml cookies: true should open a jar"
    );
    run_ok(&r4, "form-login");
    assert!(run_ok(&r4, "me").ok, "in-memory jar within a run");
    let r5 = new_runner(dir, opts("staging", true, false));
    assert!(
        !run_ok(&r5, "me").ok,
        "--no-session must not persist cookies"
    );

    // The jar is redacted in output like any other cookie header.
    let r6 = new_runner(
        dir,
        Options {
            redact: true,
            ..opts("dev", true, false)
        },
    );
    let res = run_ok(&r6, "form-login");
    assert_eq!(
        res.display_response().unwrap().headers["set-cookie"],
        MASKED
    );
    assert!(
        !res.to_json().contains("s-1"),
        "cookie value leaked: {}",
        res.to_json()
    );
}
