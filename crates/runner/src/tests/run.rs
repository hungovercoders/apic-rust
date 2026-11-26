use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use apic_httpd::{Mux, Request as HttpRequest, Response as HttpResponse, TestServer};
use apic_httpfile::Header;

use super::*;
use crate::run::json_or_string;
use crate::{Body, Resolved, Response, EXIT_TRANSPORT, EXIT_USAGE, MASKED};

pub(crate) fn test_server() -> TestServer {
    let mut mux = Mux::new();
    mux.handle_fn("POST /auth/login", |r| {
        let input: serde_json::Value = serde_json::from_slice(&r.body).unwrap_or_default();
        if input["user"] != "alice" || input["password"] != "s3cret" {
            return HttpResponse::error(401, r#"{"error":"bad credentials"}"#);
        }
        HttpResponse::json(200, r#"{"access_token":"tok-123","expires":3600}"#)
    })
    .unwrap();
    mux.handle_fn("GET /users/{id}", |r| {
        if r.header("Authorization") != Some("Bearer tok-123") {
            return HttpResponse::error(401, r#"{"error":"unauthorised"}"#);
        }
        HttpResponse::json(
            200,
            format!(
                r#"{{"id":{},"email":"alice@example.com"}}"#,
                r.path_value("id")
            ),
        )
    })
    .unwrap();
    mux.handle_fn("GET /redirect", |r| {
        HttpResponse::redirect(r, "/users/1", 302)
    })
    .unwrap();
    TestServer::new(mux.into_handler())
}

pub(crate) const API_HTTP: &str = r#"
### Log in
# @name login
# @assert status == 200
# @capture token = body.$.access_token
POST {{baseUrl}}/auth/login
Content-Type: application/json

{"user": "{{user}}", "password": "{{password}}"}

### Get user
# @name get-user
# @assert status == 200
# @assert body.$.id == {{userId}}
# @assert header.content-type contains json
# @capture email = body.$.email
GET {{baseUrl}}/users/{{userId}}
Authorization: Bearer {{token}}

### Flow reference
# @name get-user-ref
GET {{baseUrl}}/users/2
Authorization: Bearer {{login.response.body.$.access_token}}

### Redirect
# @name redirect
# @no-redirect
# @assert status == 302
# @assert header.location == /users/1
GET {{baseUrl}}/redirect
"#;

#[test]
fn login_capture_and_reuse_across_invocations() {
    let srv = test_server();
    let dir = write_project(&[
        ("api.http", API_HTTP),
        (
            "http-client.env.json",
            &format!(
                r#"{{"dev": {{"baseUrl": "{}", "user": "alice", "userId": 7}}}}"#,
                srv.url()
            ),
        ),
        (
            "http-client.private.env.json",
            r#"{"dev": {"password": "s3cret"}}"#,
        ),
    ]);
    let dir = dir.path();
    let dev = || Options {
        env: "dev".into(),
        ..Options::default()
    };

    // First invocation: get-user fails with a helpful hint because token
    // is missing.
    let r = new_runner(dir, dev());
    let err = run_err(&r, "get-user");
    assert!(
        err.usage().is_some() && err.to_string().contains(r#"captured by request "login""#),
        "want missing-variable hint, got {err}"
    );
    assert_eq!(exit(Some(&err)), EXIT_USAGE);

    // Log in: token captured and persisted.
    let res = run_ok(&r, "login");
    assert!(
        res.ok && res.captures["token"] == "tok-123",
        "login: {res:?}"
    );

    // Second invocation: a fresh runner picks the token up from the
    // session.
    let r2 = new_runner(dir, dev());
    let res = run_ok(&r2, "get-user");
    assert!(res.ok, "get-user: {res:?}");
    let resp = res.response.as_ref().unwrap();
    assert!(
        resp.status == 200
            && res.captures["email"] == "alice@example.com"
            && res.asserts.len() == 3,
        "{res:?}"
    );
    assert!(
        resp.body.is_json(),
        "body should be raw JSON, got {:?}",
        resp.body
    );
    assert!(
        dir.join(".apic").join(".gitignore").exists(),
        "session .gitignore not written"
    );

    // Describe reports sources.
    let req = lookup(&r2, "get-user");
    let d = r2.describe(&req);
    let src: BTreeMap<_, _> = d
        .variables
        .iter()
        .map(|v| (v.name.as_str(), v.source.as_str()))
        .collect();
    assert!(
        src["token"] == "session"
            && src["baseUrl"].contains("http-client.env.json [dev]")
            && d.ready,
        "describe: {:?}",
        d.variables
    );
    assert_eq!(d.url, "{{baseUrl}}/users/{{userId}}");

    // --var beats everything and wrong id fails the assert (exit 1
    // territory, no error).
    let r3 = new_runner(
        dir,
        Options {
            vars: vars(&[("userId", "9")]),
            ..dev()
        },
    );
    let res = run_ok(&r3, "get-user");
    assert!(res.ok && res.request.url.ends_with("/users/9"), "{res:?}");
    let r4 = new_runner(
        dir,
        Options {
            vars: vars(&[("token", "wrong")]),
            ..dev()
        },
    );
    let res = run_ok(&r4, "get-user");
    assert!(
        !res.ok && !res.asserts[0].pass,
        "expected failed assert: {res:?}"
    );
}

#[test]
fn flow_response_reference_and_no_redirect() {
    let srv = test_server();
    // The shell variable is this test's own, so that no other test reads it.
    let api = API_HTTP.replace("{{baseUrl}}", "{{flowBaseUrl}}");
    let dir = write_project(&[("api.http", &api), (".env", "password=s3cret\n")]);
    let _env = EnvGuard::set(&[("APIC_VAR_flowBaseUrl", &srv.url())]);
    let r = new_runner(
        dir.path(),
        Options {
            vars: vars(&[("user", "alice"), ("userId", "1")]),
            no_session: true,
            ..Options::default()
        },
    );
    let (results, err) = r.run_all(&ctx(), &r.project.requests());
    assert!(err.is_none(), "{err:?}");
    assert_eq!(results.len(), 4);
    for res in &results {
        assert!(res.ok, "{} failed: {res:?}", res.request.name);
    }
    assert_eq!(results[2].response.as_ref().unwrap().status, 200);
    assert!(
        !dir.path().join(".apic").exists(),
        "no_session must not write .apic"
    );
}

#[test]
fn flow_stops_on_failure() {
    let srv = test_server();
    let dir = write_project(&[
        ("api.http", API_HTTP),
        (
            "http-client.env.json",
            &format!(
                r#"{{"dev": {{"baseUrl": "{}", "user": "alice", "userId": 7, "password": "nope"}}}}"#,
                srv.url()
            ),
        ),
    ]);
    let r = new_runner(
        dir.path(),
        Options {
            env: "dev".into(),
            no_session: true,
            ..Options::default()
        },
    );
    let (results, err) = r.run_all(&ctx(), &r.project.requests());
    assert!(
        err.is_none() && results.len() == 1 && !results[0].ok,
        "want stop after failed login: {} results, err={err:?}",
        results.len()
    );
    assert!(
        results[0].errors[0].contains("capture token"),
        "want capture error, got {:?}",
        results[0].errors
    );
}

#[test]
fn transport_error() {
    let dir = write_project(&[("api.http", "GET http://127.0.0.1:1/nope\n")]);
    let r = new_runner(
        dir.path(),
        Options {
            no_session: true,
            ..Options::default()
        },
    );
    let (_, err) = r.run(&ctx(), &r.project.requests()[0]);
    assert_eq!(exit(err.as_ref()), EXIT_TRANSPORT, "{err:?}");
    assert_eq!(crate::code_of(err.as_ref().unwrap()), crate::Code::Connect);
}

#[test]
fn unknown_env() {
    let dir = write_project(&[
        ("api.http", "GET http://x\n"),
        ("http-client.env.json", r#"{"dev": {}}"#),
    ]);
    let err = Runner::new(
        load(dir.path()),
        Options {
            env: "prod".into(),
            ..Options::default()
        },
    )
    .err()
    .expect("an error");
    assert!(err.to_string().contains("have: dev"), "{err}");
    assert_eq!(crate::code_of(&err), crate::Code::Environment);
}

#[test]
fn new_rejects_parse_diagnostics() {
    let dir = tempfile::tempdir().unwrap();
    let p = apic_project::Project::new(
        dir.path().to_str().unwrap().to_string(),
        apic_project::Config::default(),
        Vec::new(),
        vec![apic_httpfile::Diagnostic {
            path: "bad.http".into(),
            line: 3,
            severity: "error".into(),
            message: "bad directive".into(),
            ..apic_httpfile::Diagnostic::default()
        }],
    );
    let err = Runner::new(Arc::new(p), Options::default())
        .err()
        .expect("an error");
    assert!(err.to_string().contains("bad.http:3"), "{err}");
}

fn counting_server(hits: Arc<AtomicUsize>) -> TestServer {
    TestServer::new(move |_: &HttpRequest| {
        hits.fetch_add(1, Ordering::SeqCst);
        HttpResponse::new(200)
    })
}

fn no_session() -> Options {
    Options {
        no_session: true,
        ..Options::default()
    }
}

#[test]
fn run_preflights_assert_templates_and_skips_transport() {
    let hits = Arc::new(AtomicUsize::new(0));
    let srv = counting_server(hits.clone());
    let dir = write_project(&[(
        "api.http",
        &format!(
            "\n# @name t\n# @assert status == {{{{missing}}}}\nGET {}\n",
            srv.url()
        ),
    )]);
    let r = new_runner(dir.path(), no_session());
    let (_, err) = r.run(&ctx(), &r.project.requests()[0]);
    assert_eq!(exit(err.as_ref()), EXIT_USAGE, "{err:?}");
    assert_eq!(
        hits.load(Ordering::SeqCst),
        0,
        "request should not have been sent"
    );
}

#[test]
fn run_all_keep_going_returns_first_error() {
    let srv = counting_server(Arc::new(AtomicUsize::new(0)));
    let dir = write_project(&[(
        "api.http",
        &format!("\nGET {{{{missing}}}}\n###\nGET {}\n", srv.url()),
    )]);
    let r = new_runner(
        dir.path(),
        Options {
            keep_going: true,
            ..no_session()
        },
    );
    let (results, err) = r.run_all(&ctx(), &r.project.requests());
    assert_eq!(results.len(), 2);
    assert_eq!(exit(err.as_ref()), EXIT_USAGE, "{err:?}");
    assert!(results[0].error.is_some() && results[1].ok);
}

#[test]
fn run_rejects_body_file_outside_project() {
    let base = tempfile::tempdir().unwrap();
    std::fs::write(base.path().join("secret.txt"), "secret").unwrap();
    let proj = base.path().join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    std::fs::write(
        proj.join("api.http"),
        "\nPOST https://example.com\n\n< ../secret.txt\n",
    )
    .unwrap();
    let r = new_runner(&proj, no_session());
    let (_, err) = r.run(&ctx(), &r.project.requests()[0]);
    let err = err.expect("an error");
    assert!(
        exit(Some(&err)) == EXIT_USAGE && err.to_string().contains("outside project root"),
        "{err}"
    );
}

#[test]
fn json_or_string_accepts_scalar_json() {
    for input in ["true", "7", "null", r#""x""#] {
        let (got, _) = json_or_string(input.as_bytes());
        assert!(got.is_json(), "want JSON for {input:?}, got {got:?}");
    }
    // Text stays text; bytes that are not UTF-8 become base64, and say so.
    assert_eq!(
        json_or_string(b"plain text"),
        (Body::text("plain text"), String::new())
    );
    assert_eq!(
        json_or_string(&[0xff, 0xfe, 0x00]),
        (Body::text("//4A"), "base64".to_string())
    );
}

#[test]
fn nested_file_var_missing_propagates() {
    let dir = write_project(&[("api.http", "\n@base = {{missing}}\nGET {{base}}/x\n")]);
    let r = new_runner(dir.path(), no_session());
    let (_, err) = r.run(&ctx(), &r.project.requests()[0]);
    let err = err.expect("an error");
    assert!(
        exit(Some(&err)) == EXIT_USAGE && err.to_string().contains("{{missing}}"),
        "{err}"
    );
}

#[test]
fn describe_marks_dotenv_builtins_secret() {
    let dir = write_project(&[
        (
            "api.http",
            "\n# @name t\nGET https://example.com?q={{$dotenv X}}\n",
        ),
        (".env", "X=1"),
    ]);
    let r = new_runner(dir.path(), no_session());
    let d = r.describe(&r.project.requests()[0]);
    assert!(d.ready, "{d:?}");
    let v = d
        .variables
        .iter()
        .find(|v| v.name == "$dotenv X")
        .expect("the dotenv variable is reported");
    assert!(v.secret, "{v:?}");
}

#[test]
fn session_save_failure_marks_result_failed() {
    let srv = TestServer::new(|_: &HttpRequest| HttpResponse::json(200, r#"{"token":"x"}"#));
    let dir = write_project(&[(
        "api.http",
        &format!(
            "\n# @name t\n# @capture token = body.$.token\nGET {}\n",
            srv.url()
        ),
    )]);
    let r = new_runner(dir.path(), Options::default());
    // Make session persistence fail deterministically: .apic must be a
    // directory.
    std::fs::write(dir.path().join(".apic"), "not-a-dir").unwrap();
    let res = run_ok(&r, "t");
    assert!(!res.ok, "{res:?}");
    assert!(res.errors[0].contains("session:"), "{:?}", res.errors);
}

#[test]
fn result_json_masks_sensitive_headers_and_redacts() {
    let header = |n: &str, v: &str| Header {
        name: n.into(),
        value: v.into(),
    };
    let mut res = Result {
        ok: true,
        request: Resolved {
            method: "GET".into(),
            url: "https://example.com/users?token=secret&id=1".into(),
            headers: vec![
                header("Authorization", "Bearer secret-token"),
                header("X-Tenant", "from-private-file"),
                header("Accept", "application/json"),
            ],
            secret_headers: ["X-Tenant".to_string()].into(),
            body: "plain-body".into(),
            ..Resolved::default()
        },
        captures: vars(&[("token", "captured")]),
        ..Result::default()
    };
    let s = serde_json::to_string(&res).unwrap();
    for bad in ["secret-token", "from-private-file"] {
        assert!(!s.contains(bad), "json should mask {bad:?}: {s}");
    }
    let v: serde_json::Value = serde_json::from_str(&s).unwrap();
    assert_eq!(
        v["request"]["url"],
        "https://example.com/users?token=secret&id=1"
    );
    assert_eq!(v["request"]["headers"]["Accept"], "application/json");
    assert_eq!(v["request"]["body"], "plain-body");
    assert_eq!(v["captures"]["token"], "captured");

    res.redact = true;
    let s = serde_json::to_string(&res).unwrap();
    for bad in [
        "secret",
        "plain-body",
        "captured",
        "application/json",
        "id=1",
    ] {
        assert!(!s.contains(bad), "--redact should mask {bad:?}: {s}");
    }
    let v: serde_json::Value = serde_json::from_str(&s).unwrap();
    assert_eq!(
        v["request"]["url"],
        "https://example.com/users?token=***&id=***"
    );
    assert_eq!(v["captures"]["token"], "***");
}

#[test]
fn resolve_marks_secret_headers() {
    let dir = write_project(&[
        (
            "api.http",
            "### a\n# @name a\nGET http://x/\nX-Tenant: {{tenant}}\nX-Plain: {{plain}}\nAuthorization: Bearer {{token}}\n",
        ),
        ("http-client.env.json", r#"{"dev": {"plain": "p"}}"#),
        (
            "http-client.private.env.json",
            r#"{"dev": {"tenant": "t", "token": "s"}}"#,
        ),
    ]);
    let r = new_runner(
        dir.path(),
        Options {
            env: "dev".into(),
            ..no_session()
        },
    );
    let res = r.resolve(&r.project.requests()[0]).unwrap();
    assert!(
        res.secret_headers.contains("X-Tenant") && !res.secret_headers.contains("X-Plain"),
        "{:?}",
        res.secret_headers
    );
    let shown: BTreeMap<_, _> = res
        .display_headers(false)
        .into_iter()
        .map(|h| (h.name, h.value))
        .collect();
    assert!(
        shown["X-Tenant"] == MASKED && shown["Authorization"] == MASKED && shown["X-Plain"] == "p",
        "{shown:?}"
    );
}

/// Covers the half of --redact that used to be missing: the response body,
/// its headers and the values assertions carry. Before this, `apic run
/// login --redact` masked the captured token while printing the response
/// body it was captured from.
#[test]
fn result_json_redacts_response() {
    let new_result = || Result {
        ok: false,
        request: Resolved {
            method: "POST".into(),
            url: "https://example.com/login".into(),
            ..Resolved::default()
        },
        response: Some(Response {
            status: 200,
            status_text: "200 OK".into(),
            duration_ms: 12,
            size: 44,
            headers: vars(&[
                ("content-type", "application/json"),
                ("set-cookie", "sid=abc123"),
            ]),
            body: serde_json::json!({"access_token": "secret-token"}).into(),
            ..Response::default()
        }),
        asserts: vec![apic_assert::Result {
            expr: "body.$.access_token == secret-token".into(),
            expected: "secret-token".into(),
            actual: "secret-token".into(),
            ..apic_assert::Result::default()
        }],
        captures: vars(&[("token", "secret-token")]),
        ..Result::default()
    };

    // Without --redact the response stays intact so agents can chain it,
    // except set-cookie, which is always masked.
    let s = serde_json::to_string(&new_result()).unwrap();
    assert!(
        s.contains("secret-token") && s.contains("application/json"),
        "{s}"
    );
    assert!(!s.contains("sid=abc123"), "{s}");

    // With --redact nothing that carries a value survives.
    let mut res = new_result();
    res.redact = true;
    let s = serde_json::to_string(&res).unwrap();
    for bad in ["secret-token", "sid=abc123", "application/json"] {
        assert!(
            !s.contains(bad),
            "redacted json should not contain {bad:?}: {s}"
        );
    }

    // Structure survives, so CI can still see what failed.
    let v: serde_json::Value = serde_json::from_str(&s).unwrap();
    let resp = &v["response"];
    assert!(
        resp["status"] == 200 && resp["duration_ms"] == 12 && resp["size"] == 44,
        "redact should keep status, timing and size: {s}"
    );
    assert_eq!(resp["body"], MASKED);
    let a = &v["asserts"][0];
    assert!(a["actual"] == MASKED && a["expected"] == MASKED, "{s}");
    assert_eq!(a["expr"], format!("body.$.access_token == {MASKED}"));
}

/// Pins that the documented CI secret channels are treated as secret, so a
/// header built from one is not printed in clear.
#[test]
fn var_and_shell_var_are_secret() {
    let dir = write_project(&[(
        "api.http",
        "# @name get\nGET https://example.com/x\nX-Tenant: {{tenant}}\nX-Other: {{shellSecretOther}}\n",
    )]);
    let _env = EnvGuard::set(&[("APIC_VAR_shellSecretOther", "shell-secret")]);
    let r = new_runner(
        dir.path(),
        Options {
            vars: vars(&[("tenant", "var-secret")]),
            ..no_session()
        },
    );
    let req = r.project.resolve("get").unwrap();
    let res = r.resolve(&req[0]).unwrap();
    assert!(
        res.secret_headers.contains("X-Tenant") && res.secret_headers.contains("X-Other"),
        "{:?}",
        res.secret_headers
    );
    for h in res.display_headers(false) {
        assert!(
            h.value != "var-secret" && h.value != "shell-secret",
            "{h:?}"
        );
    }
}

/// Pins that apic's own credential headers do not follow a redirect off
/// the host they were addressed to. The client strips Authorization,
/// Cookie and Proxy-Authorization itself; the rest of the sensitive
/// headers is ours to handle, and X-Amz-Security-Token is set by our own
/// SigV4 signer.
#[test]
fn redirect_strips_sensitive_headers_cross_host() {
    let got: Arc<Mutex<Option<apic_httpd::Headers>>> = Arc::default();
    let seen = got.clone();
    let last = TestServer::new(move |r: &HttpRequest| {
        *seen.lock().unwrap() = Some(r.headers.clone());
        HttpResponse::json(200, r#"{"ok":true}"#)
    });
    let same_host: Arc<Mutex<Option<apic_httpd::Headers>>> = Arc::default();
    let seen = same_host.clone();
    let last_url = last.url();
    let start = TestServer::new(move |r: &HttpRequest| match r.path.as_str() {
        "/away" => HttpResponse::redirect(r, &format!("{last_url}/landed"), 302),
        "/local" => HttpResponse::redirect(r, &format!("http://{}/here", r.host), 302),
        _ => {
            *seen.lock().unwrap() = Some(r.headers.clone());
            HttpResponse::json(200, r#"{"ok":true}"#)
        }
    });
    let src = "# @name away\nGET {{base}}/away\nX-Api-Key: api-secret\nX-Amz-Security-Token: sess-secret\nAuthorization: Bearer bearer-secret\nAccept: application/json\n\n### \n# @name local\nGET {{base}}/local\nX-Api-Key: api-secret\n";
    let dir = write_project(&[("api.http", src)]);
    let r = new_runner(
        dir.path(),
        Options {
            vars: vars(&[("base", &start.url())]),
            keep_going: true,
            ..no_session()
        },
    );
    run_ok(&r, "away");
    let got = got
        .lock()
        .unwrap()
        .clone()
        .expect("the redirect was followed");
    for h in ["X-Api-Key", "X-Amz-Security-Token", "Authorization"] {
        assert_eq!(
            got.get(h),
            None,
            "{h} should not cross to another host on redirect"
        );
    }
    // Non-credential headers still travel.
    assert_eq!(got.get("Accept"), Some("application/json"));

    // A same-host redirect keeps them: the credential was meant for that
    // host.
    run_ok(&r, "local");
    let same = same_host
        .lock()
        .unwrap()
        .clone()
        .expect("the redirect was followed");
    assert_eq!(same.get("X-Api-Key"), Some("api-secret"));
}

/// Pins that an oversized response is an error rather than an unbounded
/// read into memory.
#[test]
fn response_body_is_bounded() {
    let srv = TestServer::new(|_: &HttpRequest| HttpResponse::new(200).with_body(vec![0u8; 4096]));
    let dir = write_project(&[("api.http", "# @name big\nGET {{base}}/big\n")]);
    let opts = |max| Options {
        vars: vars(&[("base", &srv.url())]),
        max_body_bytes: max,
        ..no_session()
    };
    let r = new_runner(dir.path(), opts(1024));
    let err = run_err(&r, "big");
    assert!(err.to_string().contains("maxBodyBytes"), "{err}");
    assert_eq!(crate::code_of(&err), crate::Code::Protocol);

    // Under the cap it reads normally.
    let r2 = new_runner(dir.path(), opts(1 << 20));
    let res = run_ok(&r2, "big");
    assert_eq!(res.response.unwrap().size, 4096);
}
