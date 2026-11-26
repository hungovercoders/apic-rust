use apic_httpd::{Request as HttpRequest, Response as HttpResponse, TestServer};

use super::*;
use crate::{EXIT_TRANSPORT, EXIT_USAGE};

/// Answers with the protocol it served, over TLS, offering HTTP/2 only
/// when `h2` is set.
fn tls_server(h2: bool) -> TestServer {
    let handler = |r: &HttpRequest| HttpResponse::new(200).with_body(r.proto.clone());
    if h2 {
        // Offers both, as a real server does.
        TestServer::builder().http2(true).start(handler)
    } else {
        TestServer::new_tls(handler)
    }
}

/// The HTTP version on the request line pins the protocol: HTTP/1.1 turns
/// HTTP/2 off, HTTP/2 requires it, and none negotiates as before.
#[test]
fn request_line_version() {
    let (h2, h1) = (tls_server(true), tls_server(false));
    let plain = TestServer::new(|_: &HttpRequest| HttpResponse::new(200));
    let dir = write_project(&[
        (
            "api.http",
            r#"
### any
# @name any
GET {{h2}}/

### one
# @name one
GET {{h2}}/ HTTP/1.1

### two
# @name two
GET {{h2}}/ HTTP/2

### two-refused
# @name two-refused
GET {{h1}}/ HTTP/2

### two-plain
# @name two-plain
GET {{plain}}/ HTTP/2
"#,
        ),
        (
            "http-client.env.json",
            &format!(
                r#"{{"dev": {{"h2": "{}", "h1": "{}", "plain": "{}"}}}}"#,
                h2.url(),
                h1.url(),
                plain.url()
            ),
        ),
    ]);
    let r = new_runner(
        dir.path(),
        Options {
            env: "dev".into(),
            no_session: true,
            insecure: true,
            ..Options::default()
        },
    );
    for (name, want) in [
        ("any", "HTTP/2.0"),
        ("one", "HTTP/1.1"),
        ("two", "HTTP/2.0"),
    ] {
        let res = run_ok(&r, name);
        assert_eq!(res.response.as_ref().unwrap().proto, want, "{name}");
        assert_eq!(
            String::from_utf8_lossy(&res.raw().unwrap().body),
            want,
            "{name}"
        );
    }
    assert_eq!(run_ok(&r, "two").request.http_version, "HTTP/2");
    let err = run_err(&r, "two-refused");
    assert!(
        exit(Some(&err)) == EXIT_TRANSPORT && err.to_string().contains("HTTP/2"),
        "two-refused: {err}"
    );
    let err = run_err(&r, "two-plain");
    assert!(
        exit(Some(&err)) == EXIT_USAGE && err.to_string().contains("h2c"),
        "two-plain: {err}"
    );
}
