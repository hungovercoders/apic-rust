use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use apic_httpd::{Mux, Request as HttpRequest, Response as HttpResponse, TestServer};
use base64::Engine;

use super::*;
use crate::EXIT_USAGE;

fn json_body(v: serde_json::Value) -> HttpResponse {
    HttpResponse::new(200).with_body(format!("{v}\n"))
}

fn dev(no_session: bool) -> Options {
    Options {
        env: "dev".into(),
        no_session,
        ..Options::default()
    }
}

#[test]
fn auth_directive_default_and_none() {
    let token_calls = Arc::new(AtomicUsize::new(0));
    let calls = token_calls.clone();
    let mut mux = Mux::new();
    mux.handle_fn("/token", move |_| {
        calls.fetch_add(1, Ordering::SeqCst);
        json_body(serde_json::json!({"access_token": "o-tok", "expires_in": 3600}))
    })
    .unwrap();
    mux.handle_fn("/", |r| {
        json_body(serde_json::json!({
            "authz": r.header("Authorization").unwrap_or(""),
            "key": r.header("X-Api-Key").unwrap_or(""),
        }))
    })
    .unwrap();
    let srv = TestServer::new(mux.into_handler());

    let dir = write_project(&[
        (
            "apic.yaml",
            "auth:\n  default: bearer {{token}}\n  allowExec: true\n",
        ),
        (
            "http-client.env.json",
            &format!(
                r#"{{"dev": {{"baseUrl": "{}", "user": "alice"}}}}"#,
                srv.url()
            ),
        ),
        (
            "http-client.private.env.json",
            r#"{"dev": {"password": "pw", "token": "default-tok", "clientSecret": "sec"}}"#,
        ),
        (
            "api.http",
            r#"
### default
# @name default
# @assert body.$.authz == "Bearer default-tok"
GET {{baseUrl}}/a

### none
# @name none
# @assert body.$.authz == ""
# @auth none
GET {{baseUrl}}/b

### basic
# @name basic
# @auth basic {{user}} {{password}}
GET {{baseUrl}}/c

### oauth2
# @name oauth2
# @auth oauth2 tokenUrl={{baseUrl}}/token clientId=cid clientSecret={{clientSecret}}
# @assert body.$.authz == "Bearer o-tok"
GET {{baseUrl}}/d

### exec
# @name exec
# @auth exec uname header=X-Api-Key prefix=
GET {{baseUrl}}/e

### missing
# @name missing
# @auth bearer {{nope}}
GET {{baseUrl}}/f
"#,
        ),
    ]);
    let r = new_runner(dir.path(), dev(false));
    let run = |name: &str| {
        let res = run_ok(&r, name);
        assert!(res.ok, "{name}: {res:?}");
        res
    };
    assert_eq!(run("default").request.auth, "bearer");
    run("none");
    let res = run("basic");
    let body: serde_json::Value = serde_json::from_slice(&res.raw().unwrap().body).unwrap();
    assert_eq!(
        body["authz"],
        format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode("alice:pw")
        )
    );
    run("oauth2");
    run("oauth2");
    assert_eq!(
        token_calls.load(Ordering::SeqCst),
        1,
        "the token should be cached in the session"
    );
    // A new runner reuses the cached token from the session file.
    let r2 = new_runner(dir.path(), dev(false));
    let res = run_ok(&r2, "oauth2");
    assert!(
        res.ok && token_calls.load(Ordering::SeqCst) == 1,
        "session cache"
    );
    let res = run("exec");
    let body: serde_json::Value = serde_json::from_slice(&res.raw().unwrap().body).unwrap();
    let key = body["key"].as_str().unwrap();
    assert!(!key.is_empty() && !key.contains(' '), "exec: {body}");

    // Missing variable inside the auth spec is reported like any other.
    let err = run_err(&r, "missing");
    assert!(err.to_string().contains("{{nope}}"), "{err}");

    // Describe reports the auth spec and its variables.
    let d = r.describe(&lookup(&r, "basic"));
    assert!(
        d.auth == "basic {{user}} {{password}}" && d.auth_source == "request",
        "{d:?}"
    );
    let names: Vec<&str> = d.variables.iter().map(|v| v.name.as_str()).collect();
    assert!(
        names.contains(&"user") && names.contains(&"password"),
        "{names:?}"
    );
    assert_eq!(
        r.describe(&lookup(&r, "default")).auth_source,
        apic_project::CONFIG_FILE
    );

    // Cached tokens are not listed as ordinary variables.
    for v in r.env_vars() {
        assert!(
            !v.name.starts_with('$'),
            "token cache leaked into env vars: {}",
            v.name
        );
    }
}

#[test]
fn auth_validate() {
    let dir = write_project(&[
        ("apic.yaml", "auth:\n  default: magic\n"),
        (
            "api.http",
            "### a\n# @auth exec whoami\nGET http://x\n\n### b\n# @auth aws sigv4\nGET http://y\n",
        ),
    ]);
    let p = load(dir.path());
    let joined: Vec<String> = p
        .validate()
        .iter()
        .map(|d| format!("{}: {}", d.severity, d.message))
        .collect();
    let joined = joined.join("\n");
    for want in [
        "error: auth.default",
        "warning: @auth exec will be refused",
        "error: @auth aws takes only",
    ] {
        assert!(joined.contains(want), "missing {want:?} in:\n{joined}");
    }
}

#[test]
fn empty_auth_directive_returns_usage_error_instead_of_using_default() {
    let dir = write_project(&[
        ("apic.yaml", "auth:\n  default: bearer {{token}}\n"),
        ("api.http", "### t\n# @auth\nGET http://example.com\n"),
    ]);
    let r = new_runner(
        dir.path(),
        Options {
            vars: vars(&[("token", "tok")]),
            no_session: true,
            ..Options::default()
        },
    );
    let reqs = r.project.resolve("api.http#1").unwrap();
    let (_, err) = r.run(&ctx(), &reqs[0]);
    let err = err.expect("an error");
    assert!(err.to_string().contains("@auth needs a type"), "{err}");
}

#[test]
fn insecure_also_applies_to_oauth_token_client() {
    let mut mux = Mux::new();
    mux.handle_fn("/token", |_| {
        HttpResponse::json(200, r#"{"access_token":"tok","expires_in":60}"#)
    })
    .unwrap();
    mux.handle_fn("/", |r| {
        json_body(serde_json::json!({"authz": r.header("Authorization").unwrap_or("")}))
    })
    .unwrap();
    let srv = TestServer::new_tls(mux.into_handler());
    let dir = write_project(&[
        (
            "http-client.env.json",
            &format!(r#"{{"default": {{"baseUrl": "{}"}}}}"#, srv.url()),
        ),
        (
            "http-client.private.env.json",
            r#"{"default": {"clientSecret": "sec"}}"#,
        ),
        (
            "api.http",
            "\n### oauth2\n# @name oauth2\n# @auth oauth2 tokenUrl={{baseUrl}}/token clientId=cid clientSecret={{clientSecret}}\nGET {{baseUrl}}/\n",
        ),
    ]);
    let r = new_runner(
        dir.path(),
        Options {
            env: "default".into(),
            insecure: true,
            no_session: true,
            ..Options::default()
        },
    );
    let res = run_ok(&r, "oauth2");
    assert!(res.ok, "{res:?}");
}

#[test]
fn digest_auth_answers_challenge_through_run() {
    let hits = Arc::new(AtomicUsize::new(0));
    let count = hits.clone();
    let srv = TestServer::new(move |r: &HttpRequest| {
        count.fetch_add(1, Ordering::SeqCst);
        let authz = r.header("Authorization").unwrap_or("");
        if !authz.starts_with("Digest ") {
            return HttpResponse::error(401, r#"{"error":"challenge"}"#).with_header(
                "WWW-Authenticate",
                r#"Digest realm="apic", nonce="n1", algorithm=SHA-256, qop="auth""#,
            );
        }
        if !authz.contains(r#"username="alice""#)
            || !authz.contains(r#"uri="/secret?x=1""#)
            || !authz.contains("nc=0000000")
        {
            return HttpResponse::error(401, r#"{"error":"bad"}"#);
        }
        let at = authz.find("nc=").unwrap() + 3;
        HttpResponse::json(
            200,
            serde_json::json!({"ok": true, "nc": &authz[at..at + 8]}).to_string(),
        )
    });
    let dir = write_project(&[
        (
            "http-client.env.json",
            &format!(
                r#"{{"dev": {{"baseUrl": "{}", "user": "alice"}}}}"#,
                srv.url()
            ),
        ),
        (
            "http-client.private.env.json",
            r#"{"dev": {"password": "s3cret"}}"#,
        ),
        (
            "api.http",
            r#"
### secret
# @name secret
# @auth digest {{user}} {{password}}
# @assert status == 200
# @assert body.$.ok == true
GET {{baseUrl}}/secret?x=1

### again
# @name again
# @auth digest {{user}} {{password}}
# @assert body.$.nc == "00000002"
GET {{baseUrl}}/secret?x=1
"#,
        ),
    ]);
    let r = new_runner(dir.path(), dev(true));
    let res = run_ok(&r, "secret");
    assert!(res.ok, "problem={}", res.problem());
    assert_eq!(res.request.auth, "digest");
    assert_eq!(
        res.auth_note(),
        "digest: 401 challenge answered (2 requests)"
    );
    let data = serde_json::to_string(&res).unwrap();
    assert!(
        data.contains(r#""auth":"digest""#) && !data.contains("s3cret"),
        "json: {data}"
    );
    // The second request in the same invocation is answered first time
    // with the next nonce count.
    let res = run_ok(&r, "again");
    assert!(
        res.ok && res.auth_note() == "digest: answered from an earlier challenge",
        "again: note={:?} problem={}",
        res.auth_note(),
        res.problem()
    );
    assert_eq!(hits.load(Ordering::SeqCst), 3, "challenge, answer, answer");
}

#[test]
fn api_key_auth_through_run() {
    let srv = TestServer::new(|r: &HttpRequest| {
        json_body(serde_json::json!({
            "header": r.header("X-Api-Key").unwrap_or(""),
            "token": r.header("X-Auth-Token").unwrap_or(""),
            "query": r.query_get("api_key").unwrap_or_default(),
        }))
    });
    let dir = write_project(&[
        ("apic.yaml", "auth:\n  default: apikey {{apiKey}}\n"),
        (
            "http-client.env.json",
            &format!(r#"{{"dev": {{"baseUrl": "{}"}}}}"#, srv.url()),
        ),
        (
            "http-client.private.env.json",
            r#"{"dev": {"apiKey": "k-123"}}"#,
        ),
        (
            "api.http",
            r#"
### header
# @name header
# @assert body.$.header == "k-123"
GET {{baseUrl}}/a

### custom
# @name custom
# @auth apikey {{apiKey}} header=X-Auth-Token prefix="Token "
# @assert body.$.token == "Token k-123"
GET {{baseUrl}}/b

### query
# @name query
# @auth apikey {{apiKey}} query=api_key
# @assert body.$.query == "k-123"
GET {{baseUrl}}/c?keep=1
"#,
        ),
    ]);
    let r = new_runner(dir.path(), dev(true));
    for name in ["header", "custom", "query"] {
        let res = run_ok(&r, name);
        assert!(res.ok, "{name}: problem={}", res.problem());
        assert_eq!(res.request.auth, "apikey", "{name}");
        // The server echoes the key in its body; the request side never
        // shows it, in any form.
        let data = serde_json::to_string(&res.request).unwrap();
        assert!(
            !data.contains("k-123"),
            "{name}: key in request output: {data}"
        );
    }
}

#[test]
fn authorization_code_needs_a_terminal() {
    let dir = write_project(&[
        (
            "http-client.env.json",
            r#"{"dev": {"baseUrl": "http://api.example.test"}}"#,
        ),
        (
            "api.http",
            "\n### me\n# @name me\n# @auth oauth2 grant=authorization_code authUrl=http://idp.example.test/authorize tokenUrl=http://idp.example.test/token clientId=cid\nGET {{baseUrl}}/me\n",
        ),
    ]);
    let r = new_runner(dir.path(), dev(true));
    let err = run_err(&r, "me");
    assert!(
        exit(Some(&err)) == EXIT_USAGE
            && err
                .to_string()
                .contains("run the request once interactively"),
        "{err}"
    );
}

/// The `# @auth` hint that validate, the language server's completion and
/// hover show names every type the auth crate accepts, so a new type
/// cannot be left out of it again.
#[test]
fn auth_hint_names_every_type() {
    let hint = apic_httpfile::known_directive("auth").expect("the auth directive");
    let re = regex::Regex::new("`# @auth ([a-z0-9|]+) ").unwrap();
    let m = re
        .captures(hint)
        .unwrap_or_else(|| panic!("auth hint has no type list: {hint:?}"));
    let mut got: Vec<&str> = m[1].split('|').collect();
    let mut want = apic_auth::type_names();
    got.sort_unstable();
    want.sort_unstable();
    assert_eq!(got, want);
}
