use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use apic_httpd::{Mux, Response as HttpResponse, TestServer};

use super::*;

/// A JetBrains project's {{$auth.token("name")}} runs the Security.Auth
/// grant once, through the session's token cache, and describe names the
/// configuration without fetching.
#[test]
fn jetbrains_auth_token() {
    let grants = Arc::new(AtomicUsize::new(0));
    let count = grants.clone();
    let mut mux = Mux::new();
    mux.handle_fn("POST /token", move |r| {
        count.fetch_add(1, Ordering::SeqCst);
        if r.form_value("client_secret") != "s3cret" || r.form_value("audience") != "api://x" {
            return HttpResponse::new(401).with_body(r#"{"error":"invalid_client"}"#);
        }
        HttpResponse::json(
            200,
            r#"{"access_token":"at-1","id_token":"id-1","expires_in":3600}"#,
        )
    })
    .unwrap();
    mux.handle_fn("GET /me", |r| {
        HttpResponse::new(200).with_body(format!(
            "{}|{}",
            r.header("Authorization").unwrap_or(""),
            r.header("X-Id").unwrap_or("")
        ))
    })
    .unwrap();
    let srv = TestServer::new(mux.into_handler());
    let dir = write_project(&[
        (
            "api.http",
            r#"
### me
# @name me
GET {{baseUrl}}/me
Authorization: Bearer {{$auth.token("api")}}
X-Id: {{$auth.idToken("api")}}

### unknown
# @name unknown
GET {{baseUrl}}/me
Authorization: Bearer {{$auth.token("nope")}}
"#,
        ),
        (
            "http-client.env.json",
            &format!(
                r#"{{"dev": {{"baseUrl": "{}", "Security": {{"Auth": {{"api": {{
  "Type": "OAuth2", "Grant Type": "Client Credentials", "Token URL": "{{{{baseUrl}}}}/token",
  "Client ID": "cli", "Custom Request Parameters": {{"audience": "api://x"}}, "Acquire Automatically": true}}}}}}}}}}"#,
                srv.url()
            ),
        ),
        (
            "http-client.private.env.json",
            r#"{"dev": {"Security": {"Auth": {"api": {"Client Secret": "s3cret"}}}}}"#,
        ),
    ]);
    let sess = memory_session();
    let opts = || Options {
        env: "dev".into(),
        session: Some(sess.clone()),
        ..Options::default()
    };
    let r = new_runner(dir.path(), opts());
    let me = lookup(&r, "me");
    let d = r.describe(&me);
    assert!(
        grants.load(Ordering::SeqCst) == 0 && d.ready,
        "describe fetched a token or is not ready: {:?}",
        d.variables
    );
    let src = &d
        .variables
        .iter()
        .find(|v| v.name == r#"$auth.token("api")"#)
        .expect("the placeholder is described")
        .source;
    assert!(
        src.contains(r#"Security.Auth "api""#)
            && src.contains("grant=client_credentials")
            && !src.contains("s3cret"),
        "describe source: {src:?}"
    );
    let res = run_ok(&r, "me");
    assert!(res.ok, "{res:?}");
    assert_eq!(
        String::from_utf8_lossy(&res.raw().unwrap().body),
        "Bearer at-1|id-1"
    );
    assert_eq!(
        grants.load(Ordering::SeqCst),
        1,
        "one grant serves both placeholders"
    );
    // A second runner (a later invocation) reuses the cached token.
    let r2 = new_runner(dir.path(), opts());
    let res2 = run_ok(&r2, "me");
    assert!(
        res2.raw().unwrap().body == b"Bearer at-1|id-1" && grants.load(Ordering::SeqCst) == 1,
        "cached: {res2:?}"
    );
    // Tokens are secrets: the header shows masked.
    for h in res.request.display_headers(false) {
        assert!(
            !h.value.contains("at-1") && !h.value.contains("id-1"),
            "token shown: {}: {}",
            h.name,
            h.value
        );
    }
    let err = run_err(&r, "unknown");
    assert!(
        err.to_string().contains(
            r#"no Security.Auth configuration of that name for environment "dev" (declared: api)"#
        ),
        "unknown: {err}"
    );
    assert_eq!(crate::code_of(&err), crate::Code::Auth);
    assert!(
        !r.describe(&lookup(&r, "unknown")).ready,
        "an unknown configuration makes the request unready"
    );
    let cfgs = r.auth_configs();
    assert!(
        cfgs.len() == 1
            && cfgs[0].name == "api"
            && !cfgs[0].spec.contains("s3cret")
            && cfgs[0].spec.contains("clientSecret=***"),
        "configs: {cfgs:?}"
    );
}
