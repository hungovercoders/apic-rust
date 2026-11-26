//! The client against real servers on loopback.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use apic_httpc::{
    follow_redirects, CancelToken, Client, CookieJar, ErrorKind, Protocols, Proxy, RedirectAction,
    RedirectPolicy, Request, TlsConfig, TlsFailure,
};
use apic_httpd::{testcerts, Response, TestServer};

/// Answers with the request as text: the method and target, the host,
/// the headers in canonical form sorted by name, and the body's length.
fn echo(r: &apic_httpd::Request) -> Response {
    let mut out = format!("{} {} {}\nhost={}\n", r.method, r.target, r.proto, r.host);
    for (name, values) in r.headers.to_map() {
        out.push_str(&format!("{name}: {}\n", values.join(" | ")));
    }
    out.push_str(&format!("body={}\n", r.text()));
    Response::text(200, out)
}

fn trusting() -> Client {
    Client::builder()
        .tls(TlsConfig {
            ca_pem: Some(testcerts::CA_PEM.as_bytes().to_vec()),
            ..TlsConfig::default()
        })
        .build()
        .unwrap()
}

#[test]
fn methods_headers_and_defaults() {
    let srv = TestServer::new(echo);
    let client = Client::new();
    let resp = client
        .send(
            &Request::new("PURGE", &format!("{}/a/../b?q=a b&x=%41", srv.url()))
                .header("x-api-key", "k1")
                .header("X-Multi", "one")
                .header("x-multi", "two")
                .header("Content-Length", "999")
                .body("hello"),
        )
        .unwrap();
    assert_eq!(resp.status_code, 200);
    assert_eq!(resp.status, "200 OK");
    assert_eq!(resp.status_text(), "OK");
    assert_eq!(resp.proto, "HTTP/1.1");
    assert_eq!(
        resp.header("content-type"),
        Some("text/plain; charset=utf-8")
    );
    assert!(resp.headers.names().contains(&"Content-Type"));
    assert!(resp.headers.contains("Date"));
    let text = resp.text();
    // The path and the query go out as written, a space escaped.
    assert!(
        text.starts_with("PURGE /a/../b?q=a%20b&x=%41 HTTP/1.1\n"),
        "{text}"
    );
    assert!(text.contains(&format!("host={}\n", srv.addr())), "{text}");
    assert!(text.contains("X-Api-Key: k1\n"), "{text}");
    assert!(text.contains("X-Multi: one | two\n"), "{text}");
    assert!(text.contains("Content-Length: 5\n"), "{text}");
    assert!(text.contains("User-Agent: Go-http-client/1.1\n"), "{text}");
    assert!(text.contains("Accept-Encoding: gzip\n"), "{text}");
    assert!(text.contains("body=hello\n"), "{text}");

    // A Host override, a User-Agent of the caller's, basic auth from the URL.
    let url = format!("http://alice:s3cret@{}/me", srv.addr());
    let text = client
        .send(
            &Request::get(&url)
                .host("api.example.test")
                .header("User-Agent", "apic/1.0"),
        )
        .unwrap()
        .text();
    assert!(text.contains("host=api.example.test\n"), "{text}");
    assert!(text.contains("User-Agent: apic/1.0\n"), "{text}");
    assert!(
        text.contains("Authorization: Basic YWxpY2U6czNjcmV0\n"),
        "{text}"
    );
    assert!(!text.contains("Content-Length"), "{text}");
    // A Host header does the same as the field.
    let text = client
        .send(&Request::get(&srv.url()).header("Host", "other.test"))
        .unwrap()
        .text();
    assert!(text.contains("host=other.test\n"), "{text}");

    // An empty POST still declares its length; an empty DELETE does not.
    let text = client.send(&Request::post(&srv.url(), "")).unwrap().text();
    assert!(text.contains("Content-Length: 0\n"), "{text}");
    let text = client
        .send(&Request::new("DELETE", &srv.url()))
        .unwrap()
        .text();
    assert!(!text.contains("Content-Length"), "{text}");
}

/// Reads one request's head from a raw connection and answers 200.
fn raw_exchange(listener: &TcpListener) -> String {
    let (mut conn, _) = listener.accept().unwrap();
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if conn.read(&mut byte).unwrap() == 0 {
            break;
        }
        head.push(byte[0]);
    }
    conn.write_all(b"HTTP/1.1 200 Fine By Me\r\nContent-Length: 2\r\nX-ODD-case: 1\r\n\r\nok")
        .unwrap();
    String::from_utf8(head).unwrap()
}

#[test]
fn request_on_the_wire() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || raw_exchange(&listener));
    let resp = Client::new()
        .send(
            &Request::post(&format!("http://{addr}/submit?a=1"), "{}")
                .header("x-zebra", "z")
                .header("content-type", "application/json")
                .header("accept", "*/*")
                .header("User-Agent", "apic/test"),
        )
        .unwrap();
    let head = server.join().unwrap();
    // Canonical names; Host, User-Agent and the length first, the rest
    // sorted, Accept-Encoding last.
    assert_eq!(
        head,
        format!(
            "POST /submit?a=1 HTTP/1.1\r\nHost: {addr}\r\nUser-Agent: apic/test\r\nContent-Length: 2\r\nAccept: */*\r\nContent-Type: application/json\r\nX-Zebra: z\r\nAccept-Encoding: gzip\r\n\r\n"
        )
    );
    // The server's own reason phrase and a canonical header name.
    assert_eq!(resp.status, "200 Fine By Me");
    assert_eq!(resp.status_text(), "Fine By Me");
    assert_eq!(resp.headers.names(), ["Content-Length", "X-Odd-Case"]);
    assert_eq!(resp.body, b"ok");
}

#[test]
fn body_cap() {
    let srv = TestServer::new(|_| Response::text(200, "x".repeat(10_000)));
    let client = Client::new();
    let resp = client
        .send(&Request::get(&srv.url()).max_body(100))
        .unwrap();
    assert!(resp.truncated);
    assert_eq!(resp.body.len(), 100);
    let resp = client
        .send(&Request::get(&srv.url()).max_body(10_000))
        .unwrap();
    assert!(!resp.truncated);
    assert_eq!(resp.body.len(), 10_000);
    let resp = client.send(&Request::get(&srv.url())).unwrap();
    assert_eq!((resp.truncated, resp.body.len()), (false, 10_000));
}

#[test]
fn timings_and_reuse() {
    let handler = |_: &apic_httpd::Request| {
        std::thread::sleep(Duration::from_millis(15));
        Response::json(200, "{\"ok\":true}")
    };
    for tls in [false, true] {
        let srv = if tls {
            TestServer::new_tls(handler)
        } else {
            TestServer::new(handler)
        };
        let client = trusting();
        let url = srv.url().replace("127.0.0.1", "localhost");
        let first = client.send(&Request::get(&format!("{url}/a"))).unwrap();
        let t = first.timings;
        assert!(!t.reused, "{t:?}");
        assert!(t.connect > Duration::ZERO, "{t:?}");
        assert_eq!(t.tls > Duration::ZERO, tls, "{t:?}");
        assert!(
            t.ttfb >= Duration::from_millis(15) && t.ttfb <= t.total,
            "{t:?}"
        );
        assert!(t.dns + t.connect + t.tls <= t.ttfb, "{t:?}");
        assert_eq!(first.proto, "HTTP/1.1");

        let second = client.send(&Request::get(&format!("{url}/b"))).unwrap();
        let t = second.timings;
        assert!(t.reused, "{t:?}");
        assert_eq!(
            (t.dns, t.connect, t.tls),
            (Duration::ZERO, Duration::ZERO, Duration::ZERO)
        );
        assert!(t.ttfb >= Duration::from_millis(15));

        // A clone shares the pool; another client does not.
        assert!(
            client
                .clone()
                .send(&Request::get(&url))
                .unwrap()
                .timings
                .reused
        );
        assert!(!trusting().send(&Request::get(&url)).unwrap().timings.reused);
        // An IP address needs no lookup.
        let direct = trusting().send(&Request::get(&srv.url())).unwrap();
        assert_eq!(direct.timings.dns, Duration::ZERO);
        client.close_idle_connections();
        assert!(!client.send(&Request::get(&url)).unwrap().timings.reused);
    }
}

#[test]
fn http2_over_tls() {
    let proto = |r: &apic_httpd::Request| {
        let alpn = r.tls.as_ref().map(|t| t.alpn.clone()).unwrap_or_default();
        Response::text(200, format!("{} {alpn} {}", r.proto, r.host))
    };
    let h2 = TestServer::builder().http2(true).start(proto);
    let h1 = TestServer::new_tls(proto);
    let with = |protocols| {
        Client::builder()
            .tls(TlsConfig {
                ca_pem: Some(testcerts::CA_PEM.as_bytes().to_vec()),
                ..TlsConfig::default()
            })
            .protocols(protocols)
            .build()
            .unwrap()
    };

    let any = with(Protocols::Any);
    let resp = any.send(&Request::get(&h2.url())).unwrap();
    assert_eq!(resp.proto, "HTTP/2.0");
    assert_eq!(resp.status, "200 OK");
    assert_eq!(resp.text(), format!("HTTP/2.0 h2 {}", h2.addr()));
    assert!(!resp.timings.reused);
    assert!(resp.timings.tls > Duration::ZERO);
    let again = any
        .send(&Request::post(&h2.url(), "x").host("override.test"))
        .unwrap();
    assert!(again.timings.reused);
    assert_eq!(again.text(), "HTTP/2.0 h2 override.test");
    assert_eq!(
        any.send(&Request::get(&h1.url())).unwrap().proto,
        "HTTP/1.1"
    );

    let one = with(Protocols::Http1);
    assert_eq!(
        one.send(&Request::get(&h2.url())).unwrap().text(),
        format!("HTTP/1.1 http/1.1 {}", h2.addr())
    );

    let two = with(Protocols::Http2);
    assert_eq!(
        two.send(&Request::get(&h2.url())).unwrap().proto,
        "HTTP/2.0"
    );
    // A server without HTTP/2 refuses the handshake that offers nothing else.
    let err = two.send(&Request::get(&h1.url())).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Tls(TlsFailure::Other), "{err}");
    assert!(
        err.message.contains("tls: no application protocol"),
        "{err}"
    );
    let plain = TestServer::new(proto);
    let err = two.send(&Request::get(&plain.url())).unwrap_err();
    assert_eq!(err.message, "http2: unsupported scheme");
}

#[test]
fn certificate_verification() {
    let srv = TestServer::new_tls(|_| Response::text(200, "secret"));
    let url = srv.url();

    // The test authority is not a system root.
    let err = Client::new().send(&Request::get(&url)).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Tls(TlsFailure::UnknownIssuer));
    assert_eq!(
        err.to_string(),
        format!("Get \"{url}\": tls: failed to verify certificate: x509: certificate signed by unknown authority")
    );
    assert_eq!((err.op.as_str(), err.url.as_str()), ("Get", url.as_str()));

    // Trusted once the authority is added, by address and by name.
    let client = trusting();
    assert_eq!(client.send(&Request::get(&url)).unwrap().body, b"secret");
    let by_name = url.replace("127.0.0.1", "localhost");
    assert_eq!(
        client.send(&Request::get(&by_name)).unwrap().body,
        b"secret"
    );

    // Verification off.
    let insecure = Client::builder()
        .tls(TlsConfig {
            insecure: true,
            ..TlsConfig::default()
        })
        .build()
        .unwrap();
    assert_eq!(insecure.send(&Request::get(&url)).unwrap().body, b"secret");

    // A name the certificate is not for.
    let elsewhere = TestServer::builder()
        .addr("127.0.0.2:0")
        .tls()
        .start(|_| Response::new(204));
    let err = client
        .send(&Request::post(&elsewhere.url(), ""))
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Tls(TlsFailure::HostnameMismatch));
    assert_eq!(
        err.to_string(),
        format!(
            "Post \"{}\": tls: failed to verify certificate: x509: certificate is valid for 127.0.0.1, ::1, not 127.0.0.2",
            elsewhere.url()
        )
    );
    assert_eq!(
        insecure
            .send(&Request::get(&elsewhere.url()))
            .unwrap()
            .status_code,
        204
    );

    // An expired certificate.
    let expired = TestServer::builder()
        .tls_options(apic_httpd::TlsOptions {
            cert_pem: include_bytes!("../testdata/expired.pem").to_vec(),
            key_pem: include_bytes!("../testdata/expired-key.pem").to_vec(),
            ..Default::default()
        })
        .start(|_| Response::new(204));
    let err = client.send(&Request::get(&expired.url())).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Tls(TlsFailure::Expired));
    assert!(
        err.message
            .ends_with("x509: certificate has expired or is not yet valid"),
        "{err}"
    );
}

#[test]
fn mutual_tls() {
    let srv = TestServer::builder().require_client_cert().start(|r| {
        let name = r
            .tls
            .as_ref()
            .and_then(|t| t.peer_common_name())
            .unwrap_or_default();
        Response::json(200, format!("{{\"user\":\"{name}\"}}"))
    });
    let with = |cert: Option<(&str, &str)>, insecure: bool| {
        Client::builder()
            .tls(TlsConfig {
                ca_pem: Some(testcerts::CA_PEM.as_bytes().to_vec()),
                cert_pem: cert.map(|c| c.0.as_bytes().to_vec()),
                key_pem: cert.map(|c| c.1.as_bytes().to_vec()),
                insecure,
            })
            .build()
            .unwrap()
    };
    let alice = with(
        Some((testcerts::CLIENT_CERT_PEM, testcerts::CLIENT_KEY_PEM)),
        false,
    );
    let resp = alice.send(&Request::get(&srv.url())).unwrap();
    assert_eq!(resp.text(), "{\"user\":\"alice\"}");

    // No certificate, with or without verifying the server.
    for insecure in [false, true] {
        let err = with(None, insecure)
            .send(&Request::get(&srv.url()))
            .unwrap_err();
        assert_eq!(err.kind, ErrorKind::Tls(TlsFailure::Other), "{err}");
        assert!(err.message.starts_with("remote error: tls: "), "{err}");
    }
    // The same over HTTP/2, where the refusal arrives after the handshake.
    let h2 = TestServer::builder()
        .http2(true)
        .require_client_cert()
        .start(|_| Response::new(204));
    assert_eq!(
        alice.send(&Request::get(&h2.url())).unwrap().proto,
        "HTTP/2.0"
    );
    let err = with(None, false)
        .send(&Request::get(&h2.url()))
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Tls(TlsFailure::Other), "{err}");
    assert_eq!(err.message, "remote error: tls: certificate required");
    // A certificate the server does not trust.
    let mallory = with(
        Some((testcerts::STRANGER_CERT_PEM, testcerts::STRANGER_KEY_PEM)),
        false,
    );
    let err = mallory.send(&Request::get(&srv.url())).unwrap_err();
    assert!(matches!(err.kind, ErrorKind::Tls(_)), "{err}");

    // The key in the same PEM as the certificate.
    let combined = format!(
        "{}{}",
        testcerts::CLIENT_CERT_PEM,
        testcerts::CLIENT_KEY_PEM
    );
    let client = Client::builder()
        .tls(TlsConfig {
            ca_pem: Some(testcerts::CA_PEM.as_bytes().to_vec()),
            cert_pem: Some(combined.into_bytes()),
            ..TlsConfig::default()
        })
        .build()
        .unwrap();
    assert_eq!(
        client.send(&Request::get(&srv.url())).unwrap().status_code,
        200
    );

    // Settings that cannot be used are refused when the client is built.
    let mismatch = Client::builder()
        .tls(TlsConfig {
            cert_pem: Some(testcerts::CLIENT_CERT_PEM.as_bytes().to_vec()),
            key_pem: Some(testcerts::STRANGER_KEY_PEM.as_bytes().to_vec()),
            ..TlsConfig::default()
        })
        .build()
        .unwrap_err();
    assert_eq!(
        mismatch.to_string(),
        "tls: private key does not match public key"
    );
    let no_key = Client::builder()
        .tls(TlsConfig {
            cert_pem: Some(testcerts::CLIENT_CERT_PEM.as_bytes().to_vec()),
            ..TlsConfig::default()
        })
        .build()
        .unwrap_err();
    assert_eq!(
        no_key.to_string(),
        "tls: failed to find any PEM data in key input"
    );
    let no_ca = Client::builder()
        .tls(TlsConfig {
            ca_pem: Some(b"### not a certificate\n".to_vec()),
            ..TlsConfig::default()
        })
        .build()
        .unwrap_err();
    assert_eq!(no_ca.to_string(), "no PEM certificates found");
}

#[test]
fn timeout_and_cancellation() {
    let srv = TestServer::new(|r| {
        if r.path == "/slow" {
            std::thread::sleep(Duration::from_millis(600));
        }
        Response::text(200, "done")
    });
    let client = Client::new();
    let url = format!("{}/slow", srv.url());

    let started = Instant::now();
    let err = client
        .send(&Request::get(&url).timeout(Duration::from_millis(80)))
        .unwrap_err();
    assert!(started.elapsed() < Duration::from_millis(500));
    assert_eq!(err.kind, ErrorKind::Timeout);
    assert!(err.is_timeout());
    assert_eq!(
        err.to_string(),
        format!("Get \"{url}\": context deadline exceeded")
    );
    // A deadline in the past fails without sending.
    let err = client
        .send(&Request {
            deadline: Some(Instant::now()),
            ..Request::get(&srv.url())
        })
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Timeout);

    let token = CancelToken::new();
    let canceller = token.clone();
    let thread = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(60));
        canceller.cancel();
    });
    let started = Instant::now();
    let err = client.send(&Request::get(&url).cancel(&token)).unwrap_err();
    assert!(started.elapsed() < Duration::from_millis(500));
    assert_eq!(err.kind, ErrorKind::Cancelled);
    assert!(err.is_cancelled());
    assert_eq!(err.message, "context canceled");
    thread.join().unwrap();
    // A token already cancelled fails at once; one never cancelled is harmless.
    assert!(client
        .send(&Request::get(&srv.url()).cancel(&token))
        .is_err());
    let quiet = CancelToken::new();
    assert_eq!(
        client
            .send(&Request::get(&srv.url()).cancel(&quiet))
            .unwrap()
            .body,
        b"done"
    );
    assert!(!quiet.wait_timeout(Duration::from_millis(1)));
    assert!(token.wait_timeout(Duration::from_secs(5)));
    // The connection is still good after all that.
    assert_eq!(
        client.send(&Request::get(&srv.url())).unwrap().body,
        b"done"
    );
}

#[test]
fn failures_without_a_response() {
    let client = Client::new();
    // Nothing listens: a refused connection.
    let free = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let err = client
        .send(&Request::get(&format!("http://{free}/x")))
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Connect);
    assert_eq!(
        err.to_string(),
        format!("Get \"http://{free}/x\": dial tcp {free}: connect: connection refused")
    );

    // A name that does not resolve.
    let err = client
        .send(&Request::get("http://no-such-host.invalid/").timeout(Duration::from_secs(20)))
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Dns, "{err}");
    assert!(
        err.message
            .starts_with("dial tcp: lookup no-such-host.invalid: "),
        "{err}"
    );

    // A server that hangs up without answering.
    let srv = TestServer::new(|_| Response::hang_up());
    let err = client.send(&Request::post(&srv.url(), "x")).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Other);
    assert_eq!(err.to_string(), format!("Post \"{}\": EOF", srv.url()));

    // Requests that cannot be built.
    for (method, url, want) in [
        (
            "GET",
            "ftp://example.com/",
            "unsupported protocol scheme \"ftp\"",
        ),
        ("GET", "example.com", "unsupported protocol scheme \"\""),
        ("GET", "http:///path", "http: no Host in request URL"),
        (
            "BAD METHOD",
            "http://example.com/",
            "net/http: invalid method \"BAD METHOD\"",
        ),
    ] {
        let req = Request::new(method, url);
        assert_eq!(req.check().unwrap_err(), want);
        let err = client.send(&req).unwrap_err();
        assert_eq!((err.kind, err.message.as_str()), (ErrorKind::Other, want));
    }
    assert!(Request::get("https://example.com/x?y=1").check().is_ok());
    let err = client
        .send(&Request::get(&srv.url()).header("X-Bad", "line\nbreak"))
        .unwrap_err();
    assert_eq!(
        err.message,
        "net/http: invalid header field value for \"X-Bad\""
    );
    let err = client
        .send(&Request::get(&srv.url()).header("Bad Name", "v"))
        .unwrap_err();
    assert_eq!(
        err.message,
        "net/http: invalid header field name \"Bad Name\""
    );
}

#[test]
fn stale_connection_is_replaced() {
    let listener = apic_httpd::Listener::bind("127.0.0.1:0").unwrap();
    let addr = listener.addr();
    let first = apic_httpd::Server::start(listener, |_| Response::text(200, "one"));
    let client = Client::new();
    let url = format!("http://{addr}/");
    assert_eq!(client.send(&Request::get(&url)).unwrap().body, b"one");
    first.shutdown();
    let second =
        apic_httpd::Server::bind(&addr.to_string(), |_| Response::text(200, "two")).unwrap();
    let resp = client.send(&Request::get(&url)).unwrap();
    assert_eq!(resp.body, b"two");
    assert!(!resp.timings.reused);
    assert!(
        client
            .send(&Request::post(&url, "x"))
            .unwrap()
            .timings
            .reused
    );
    drop(second);
}

#[test]
fn gzip_is_transparent() {
    let srv = TestServer::new(|r| {
        let asked = r.header("Accept-Encoding").unwrap_or("").contains("gzip");
        let body = "compressible ".repeat(500);
        if !asked {
            return Response::text(200, body);
        }
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(body.as_bytes()).unwrap();
        Response::new(200)
            .with_header("Content-Type", "text/plain")
            .with_header("Content-Encoding", "gzip")
            .with_body(enc.finish().unwrap())
    });
    let client = Client::new();
    let resp = client.send(&Request::get(&srv.url())).unwrap();
    assert!(resp.uncompressed);
    assert_eq!(resp.text(), "compressible ".repeat(500));
    assert!(!resp.headers.contains("Content-Encoding"));
    assert!(!resp.headers.contains("Content-Length"));
    // The cap counts decoded bytes.
    let capped = client
        .send(&Request::get(&srv.url()).max_body(1000))
        .unwrap();
    assert!(capped.truncated);
    assert_eq!(capped.body, "compressible ".repeat(500).as_bytes()[..1000]);
    // A caller that asks for gzip itself gets it as sent.
    let raw = client
        .send(&Request::get(&srv.url()).header("Accept-Encoding", "gzip"))
        .unwrap();
    assert!(!raw.uncompressed);
    assert_eq!(raw.header("Content-Encoding"), Some("gzip"));
    assert_eq!(&raw.body[..2], b"\x1f\x8b");
    // HEAD is not asked to compress.
    let head = TestServer::new(echo);
    let resp = client.send(&Request::new("HEAD", &head.url())).unwrap();
    assert!(resp.body.is_empty() && !resp.uncompressed);
}

#[test]
fn http_proxy_for_an_http_target() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    // The proxy answers every request itself, saying what it was asked.
    let proxy = TestServer::new(move |r| {
        log.lock().unwrap().push(format!(
            "{} {} host={} auth={}",
            r.method,
            r.url(),
            r.host,
            r.header("Proxy-Authorization").unwrap_or("")
        ));
        Response::json(200, "{\"via\":\"proxy\"}")
    });
    let via = Proxy::parse(&format!("http://alice:s3cret@{}", proxy.addr())).unwrap();
    let client = Client::new();
    let resp = client
        .send(&Request::get("http://api.example.test/ping?x=1").proxy(via.clone()))
        .unwrap();
    assert_eq!(resp.text(), "{\"via\":\"proxy\"}");
    assert!(!resp.timings.reused);
    // Another host rides the same connection to the proxy.
    let resp = client
        .send(&Request::get("http://other.example.test:8080/").proxy(via))
        .unwrap();
    assert!(resp.timings.reused);
    assert_eq!(
        *seen.lock().unwrap(),
        [
            "GET http://api.example.test/ping?x=1 host=api.example.test auth=Basic YWxpY2U6czNjcmV0",
            "GET http://other.example.test:8080/ host=other.example.test:8080 auth=Basic YWxpY2U6czNjcmV0",
        ]
    );
    // A proxy that is not there.
    let free = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let err = client
        .send(
            &Request::get("http://api.example.test/")
                .proxy(Proxy::parse(&format!("http://{free}")).unwrap()),
        )
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Connect);
    assert_eq!(
        err.message,
        format!("proxyconnect tcp: dial tcp {free}: connect: connection refused")
    );
}

/// Copies both ways between two sockets until either closes.
fn splice(a: TcpStream, b: TcpStream) {
    let (mut a2, mut b2) = (a.try_clone().unwrap(), b.try_clone().unwrap());
    let (mut a, mut b) = (a, b);
    std::thread::spawn(move || {
        let _ = std::io::copy(&mut a2, &mut b2);
        let _ = b2.shutdown(std::net::Shutdown::Write);
    });
    let _ = std::io::copy(&mut b, &mut a);
    let _ = a.shutdown(std::net::Shutdown::Write);
}

/// A CONNECT proxy: tunnels to wherever it is asked, or answers 407
/// without the right credentials. Records each CONNECT's head.
fn connect_proxy(
    require_auth: Option<&'static str>,
) -> (std::net::SocketAddr, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut conn) = conn else { return };
            let log = log.clone();
            std::thread::spawn(move || {
                let mut head = Vec::new();
                let mut byte = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") {
                    if conn.read(&mut byte).unwrap_or(0) == 0 {
                        return;
                    }
                    head.push(byte[0]);
                }
                let head = String::from_utf8_lossy(&head).into_owned();
                log.lock().unwrap().push(head.clone());
                if let Some(auth) = require_auth {
                    if !head.contains(&format!("Proxy-Authorization: {auth}\r\n")) {
                        let _ = conn.write_all(b"HTTP/1.1 407 Proxy Authentication Required\r\nContent-Length: 0\r\n\r\n");
                        return;
                    }
                }
                let target = head.split(' ').nth(1).unwrap_or("").to_string();
                // The test's names all live on loopback.
                let port = target.rsplit(':').next().unwrap_or("");
                let Ok(upstream) = TcpStream::connect(format!("127.0.0.1:{port}")) else {
                    let _ = conn.write_all(b"HTTP/1.1 502 Bad Gateway\r\n\r\n");
                    return;
                };
                let _ = conn.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n");
                splice(conn, upstream);
            });
        }
    });
    (addr, seen)
}

#[test]
fn connect_proxy_for_an_https_target() {
    let target = TestServer::builder().http2(true).start(echo);
    let port = target.addr().port();
    let (proxy_addr, seen) = connect_proxy(Some("Basic Ym9iOnB3"));
    let client = trusting();
    // The target is named as the certificate names it; only the proxy
    // knows where it is.
    let url = format!("https://api.example.com:{port}/secure");
    let via = Proxy::parse(&format!("http://bob:pw@{proxy_addr}")).unwrap();
    let resp = client.send(&Request::get(&url).proxy(via.clone())).unwrap();
    assert_eq!(resp.proto, "HTTP/2.0");
    let text = resp.text();
    assert!(text.starts_with("GET /secure HTTP/2.0\n"), "{text}");
    assert!(
        text.contains(&format!("host=api.example.com:{port}\n")),
        "{text}"
    );
    // The credentials went to the proxy, not through the tunnel.
    assert!(!text.contains("Proxy-Authorization"), "{text}");
    assert!(resp.timings.tls > Duration::ZERO && resp.timings.connect > Duration::ZERO);
    let head = seen.lock().unwrap()[0].clone();
    assert!(
        head.starts_with(&format!(
            "CONNECT api.example.com:{port} HTTP/1.1\r\nHost: api.example.com:{port}\r\n"
        )),
        "{head}"
    );
    assert!(
        client
            .send(&Request::get(&url).proxy(via))
            .unwrap()
            .timings
            .reused
    );
    assert_eq!(seen.lock().unwrap().len(), 1);

    // Without the credentials the proxy refuses, and its reason is the error.
    let bare = Proxy::parse(&format!("http://{proxy_addr}")).unwrap();
    let err = client.send(&Request::get(&url).proxy(bare)).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Proxy);
    assert_eq!(err.message, "Proxy Authentication Required");
}

/// A SOCKS5 proxy that wants a user name and password and connects every
/// name to loopback. Records the address each client asked for.
fn socks_proxy(
    user: &'static str,
    pass: &'static str,
) -> (std::net::SocketAddr, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut conn) = conn else { return };
            let log = log.clone();
            std::thread::spawn(move || {
                let mut head = [0u8; 2];
                conn.read_exact(&mut head).unwrap();
                let mut methods = vec![0u8; head[1] as usize];
                conn.read_exact(&mut methods).unwrap();
                if !methods.contains(&2) {
                    conn.write_all(&[5, 0xff]).unwrap();
                    return;
                }
                conn.write_all(&[5, 2]).unwrap();
                let mut b = [0u8; 2];
                conn.read_exact(&mut b).unwrap();
                let mut name = vec![0u8; b[1] as usize];
                conn.read_exact(&mut name).unwrap();
                conn.read_exact(&mut b[..1]).unwrap();
                let mut word = vec![0u8; b[0] as usize];
                conn.read_exact(&mut word).unwrap();
                if name != user.as_bytes() || word != pass.as_bytes() {
                    conn.write_all(&[1, 1]).unwrap();
                    return;
                }
                conn.write_all(&[1, 0]).unwrap();
                let mut req = [0u8; 4];
                conn.read_exact(&mut req).unwrap();
                let host = match req[3] {
                    3 => {
                        conn.read_exact(&mut b[..1]).unwrap();
                        let mut host = vec![0u8; b[0] as usize];
                        conn.read_exact(&mut host).unwrap();
                        String::from_utf8(host).unwrap()
                    }
                    1 => {
                        let mut ip = [0u8; 4];
                        conn.read_exact(&mut ip).unwrap();
                        std::net::Ipv4Addr::from(ip).to_string()
                    }
                    _ => return,
                };
                conn.read_exact(&mut b).unwrap();
                let port = u16::from_be_bytes(b);
                log.lock().unwrap().push(format!("{host}:{port}"));
                match TcpStream::connect(("127.0.0.1", port)) {
                    Ok(upstream) => {
                        conn.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0]).unwrap();
                        splice(conn, upstream);
                    }
                    Err(_) => conn.write_all(&[5, 5, 0, 1, 0, 0, 0, 0, 0, 0]).unwrap(),
                }
            });
        }
    });
    (addr, seen)
}

#[test]
fn socks5_proxy() {
    let plain = TestServer::new(echo);
    let secure = TestServer::new_tls(echo);
    let (proxy_addr, seen) = socks_proxy("carol", "pw");
    let client = trusting();
    for scheme in ["socks5", "socks5h"] {
        let via = Proxy::parse(&format!("{scheme}://carol:pw@{proxy_addr}")).unwrap();
        let url = format!("http://plain.example.test:{}/p", plain.addr().port());
        let text = client
            .send(&Request::get(&url).proxy(via.clone()))
            .unwrap()
            .text();
        assert!(text.starts_with("GET /p HTTP/1.1\n"), "{text}");
        assert!(
            text.contains(&format!(
                "host=plain.example.test:{}\n",
                plain.addr().port()
            )),
            "{text}"
        );
        let url = format!("https://www.example.com:{}/s", secure.addr().port());
        let resp = client.send(&Request::get(&url).proxy(via)).unwrap();
        assert!(resp.text().starts_with("GET /s HTTP/1.1\n"));
    }
    let asked = seen.lock().unwrap().clone();
    assert_eq!(
        asked[0],
        format!("plain.example.test:{}", plain.addr().port())
    );
    assert_eq!(
        asked[1],
        format!("www.example.com:{}", secure.addr().port())
    );

    // An address goes to the proxy as an address.
    let via = Proxy::parse(&format!("socks5://carol:pw@{proxy_addr}")).unwrap();
    client.send(&Request::get(&plain.url()).proxy(via)).unwrap();
    assert_eq!(
        seen.lock().unwrap().last().unwrap(),
        &plain.addr().to_string()
    );

    // Wrong password, no password, and a target the proxy cannot reach.
    let wrong = Proxy::parse(&format!("socks5://carol:nope@{proxy_addr}")).unwrap();
    let err = client
        .send(&Request::get(&plain.url()).proxy(wrong))
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Connect);
    assert_eq!(
        err.message,
        format!(
            "socks connect tcp {proxy_addr}->{}: username/password authentication failed",
            plain.addr()
        )
    );
    let anonymous = Proxy::parse(&format!("socks5://{proxy_addr}")).unwrap();
    let err = client
        .send(&Request::get(&plain.url()).proxy(anonymous))
        .unwrap_err();
    assert!(
        err.message
            .ends_with("no acceptable authentication methods"),
        "{err}"
    );
    let free = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let via = Proxy::parse(&format!("socks5://carol:pw@{proxy_addr}")).unwrap();
    let err = client
        .send(&Request::get(&format!("http://{free}/")).proxy(via))
        .unwrap_err();
    assert!(
        err.message.ends_with("unknown error connection refused"),
        "{err}"
    );
}

#[test]
fn https_proxy() {
    // An HTTP proxy that is itself reached over TLS, answering for a
    // plain HTTP target.
    let proxy = TestServer::new_tls(|r| Response::text(200, format!("proxied {}", r.url())));
    let via = Proxy::parse(&format!("https://{}", proxy.addr())).unwrap();
    let resp = trusting()
        .send(&Request::get("http://api.example.test/x").proxy(via))
        .unwrap();
    assert_eq!(resp.text(), "proxied http://api.example.test/x");
    assert!(resp.timings.tls > Duration::ZERO);
}

/// A server for the redirect tests. `/hop/N` redirects N more times and
/// `/to?code=&url=` redirects with a status and a target of the test's.
fn redirector() -> TestServer {
    TestServer::new(|r| {
        if let Some(n) = r.path.strip_prefix("/hop/") {
            let n: u32 = n.parse().unwrap();
            if n > 0 {
                return Response::new(302).with_header("Location", &format!("/hop/{}", n - 1));
            }
            return echo(r);
        }
        match r.path.as_str() {
            "/to" => {
                let code: u16 = r.query_get("code").unwrap().parse().unwrap();
                let mut resp = Response::new(code).with_body("redirecting");
                if let Some(url) = r.query_get("url") {
                    resp = resp.with_header("Location", &url);
                }
                resp
            }
            "/set" => Response::new(302)
                .with_header("Location", "/final")
                .add_header("Set-Cookie", "sid=fresh; Path=/")
                .add_header("Set-Cookie", "theme=dark"),
            _ => echo(r),
        }
    })
}

#[test]
fn redirects_are_not_followed_by_send() {
    let srv = redirector();
    let resp = Client::new()
        .send(&Request::get(&format!("{}/hop/2", srv.url())))
        .unwrap();
    assert_eq!(resp.status_code, 302);
    assert_eq!(resp.header("Location"), Some("/hop/1"));
}

#[test]
fn redirect_methods_and_bodies() {
    let srv = redirector();
    let client = Client::new();
    let to = |code: u16, url: &str| format!("{}/to?code={code}&url={url}", srv.url());

    let resp = client
        .execute(&Request::get(&format!("{}/hop/3", srv.url())))
        .unwrap();
    assert_eq!(resp.status_code, 200);
    assert_eq!(resp.url, format!("{}/hop/0", srv.url()));
    let text = resp.text();
    assert!(text.starts_with("GET /hop/0 HTTP/1.1\n"), "{text}");
    assert!(
        text.contains(&format!("Referer: {}/hop/1\n", srv.url())),
        "{text}"
    );
    // The timings cover the whole chain; the hops shared a connection.
    assert!(resp.timings.reused);
    assert!(resp.timings.connect > Duration::ZERO);
    assert!(resp.timings.ttfb <= resp.timings.total);

    // 301, 302 and 303 turn a POST into a GET without a body; HEAD stays HEAD.
    for code in [301, 302, 303] {
        let resp = client
            .execute(
                &Request::post(&to(code, "/landed"), "payload")
                    .header("Content-Type", "text/plain")
                    .header("X-Keep", "1"),
            )
            .unwrap();
        let text = resp.text();
        assert!(text.starts_with("GET /landed HTTP/1.1\n"), "{code}: {text}");
        assert!(
            text.contains("body=\n") && !text.contains("Content-Length"),
            "{code}: {text}"
        );
        assert!(text.contains("X-Keep: 1\n"), "{code}: {text}");
        assert_eq!(resp.method, "GET");
        let resp = client
            .execute(&Request::new("HEAD", &to(code, "/landed")))
            .unwrap();
        assert_eq!((resp.method.as_str(), resp.status_code), ("HEAD", 200));
    }
    // 307 and 308 repeat the method and the body.
    for code in [307, 308] {
        let text = client
            .execute(&Request::new("PUT", &to(code, "/landed")).body("payload"))
            .unwrap()
            .text();
        assert!(text.starts_with("PUT /landed HTTP/1.1\n"), "{code}: {text}");
        assert!(text.contains("body=payload\n"), "{code}: {text}");
    }
    // A redirect without a Location, or with another 3xx, is the answer.
    let resp = client
        .execute(&Request::get(&format!("{}/to?code=302", srv.url())))
        .unwrap();
    assert_eq!(
        (resp.status_code, resp.text().as_str()),
        (302, "redirecting")
    );
    let resp = client.execute(&Request::get(&to(304, "/landed"))).unwrap();
    assert_eq!(resp.status_code, 304);
}

#[test]
fn redirect_limit_and_policy() {
    let srv = redirector();
    let client = Client::new();
    // Ten requests are sent; the eleventh is refused.
    assert_eq!(
        client
            .execute(&Request::get(&format!("{}/hop/9", srv.url())))
            .unwrap()
            .status_code,
        200
    );
    let err = client
        .execute(&Request::get(&format!("{}/hop/10", srv.url())))
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Other);
    assert_eq!(
        err.to_string(),
        "Get \"/hop/0\": stopped after 10 redirects"
    );

    // A policy that never follows returns the redirect itself.
    let stop = |_: &mut Request, _: &[Request]| Ok(RedirectAction::Stop);
    let policy = RedirectPolicy {
        check: Some(&stop),
        jar: None,
    };
    let resp = follow_redirects(
        &client,
        &Request::get(&format!("{}/hop/2", srv.url())),
        &policy,
    )
    .unwrap();
    assert_eq!(
        (resp.status_code, resp.header("Location")),
        (302, Some("/hop/1"))
    );

    // A policy sees the hops so far, may edit the next request and may refuse.
    let calls = AtomicUsize::new(0);
    let check = |next: &mut Request, via: &[Request]| {
        calls.fetch_add(1, Ordering::Relaxed);
        assert!(via[0].url.ends_with("/hop/3"));
        assert_eq!(via[0].headers.get("X-Secret"), Some("s"));
        if via.len() >= 3 {
            return Err("that is enough".to_string());
        }
        next.headers.del("X-Secret");
        next.headers.set("X-Hop", &via.len().to_string());
        Ok(RedirectAction::Follow)
    };
    let policy = RedirectPolicy {
        check: Some(&check),
        jar: None,
    };
    let req = Request::get(&format!("{}/hop/3", srv.url())).header("X-Secret", "s");
    let err = follow_redirects(&client, &req, &policy).unwrap_err();
    assert_eq!(err.to_string(), "Get \"/hop/0\": that is enough");
    assert_eq!(calls.load(Ordering::Relaxed), 3);
    // With two hops the edits reach the last request.
    let req = Request::get(&format!("{}/hop/2", srv.url())).header("X-Secret", "s");
    let text = follow_redirects(
        &client,
        &Request {
            url: req.url.replace("/hop/2", "/hop/3"),
            ..req
        },
        &RedirectPolicy::default(),
    )
    .unwrap()
    .text();
    assert!(text.contains("X-Secret: s\n"), "{text}");
}

#[test]
fn redirect_across_hosts_drops_credentials() {
    let first = redirector();
    let second = TestServer::new(echo);
    let client = Client::new();
    let send = |target: &str| {
        client
            .execute(
                &Request::get(&format!("{}/to?code=302&url={target}", first.url()))
                    .header("Authorization", "Bearer t")
                    .header("Cookie", "a=1")
                    .header("X-Api-Key", "k")
                    .host("override.test"),
            )
            .unwrap()
            .text()
    };
    // The same host keeps everything, the Host override included.
    let text = send("/same");
    assert!(
        text.contains("Authorization: Bearer t\n") && text.contains("Cookie: a=1\n"),
        "{text}"
    );
    assert!(text.contains("host=override.test\n"), "{text}");
    // Another port of the same host keeps the credentials; the override
    // does not survive an absolute redirect.
    let text = send(&format!("{}/port", second.url()));
    assert!(
        text.contains("Authorization: Bearer t\n") && text.contains("Cookie: a=1\n"),
        "{text}"
    );
    assert!(
        text.contains(&format!("host={}\n", second.addr())),
        "{text}"
    );
    // Another host loses the credentials, not the rest.
    let text = send(&format!("http://localhost:{}/other", second.addr().port()));
    assert!(text.starts_with("GET /other "), "{text}");
    assert!(
        !text.contains("Authorization") && !text.contains("Cookie"),
        "{text}"
    );
    assert!(text.contains("X-Api-Key: k\n"), "{text}");
    assert!(
        text.contains(&format!("host=localhost:{}\n", second.addr().port())),
        "{text}"
    );
}

#[test]
fn redirect_shares_one_deadline() {
    let srv = TestServer::new(|r| {
        std::thread::sleep(Duration::from_millis(60));
        let n: u32 = r.path.trim_start_matches('/').parse().unwrap_or(0);
        Response::new(302).with_header("Location", &format!("/{}", n + 1))
    });
    let started = Instant::now();
    let err = Client::new()
        .execute(&Request::get(&format!("{}/0", srv.url())).timeout(Duration::from_millis(200)))
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Timeout);
    assert!(
        started.elapsed() < Duration::from_millis(450),
        "{:?}",
        started.elapsed()
    );
}

#[derive(Default)]
struct Jar {
    stored: Mutex<Vec<(String, String)>>,
    calls: Mutex<Vec<String>>,
}

impl CookieJar for Jar {
    fn cookies(&self, _url: &str) -> Vec<(String, String)> {
        self.stored.lock().unwrap().clone()
    }

    fn set_cookies(&self, url: &str, set_cookie: &[String]) {
        self.calls
            .lock()
            .unwrap()
            .push(format!("{url} {}", set_cookie.join(" | ")));
        for line in set_cookie {
            let pair = line.split(';').next().unwrap();
            let (name, value) = pair.split_once('=').unwrap();
            let mut stored = self.stored.lock().unwrap();
            stored.retain(|(n, _)| n != name);
            stored.push((name.to_string(), value.to_string()));
        }
    }
}

#[test]
fn redirect_with_a_cookie_jar() {
    let srv = redirector();
    let jar = Jar::default();
    jar.stored
        .lock()
        .unwrap()
        .push(("old".to_string(), "1".to_string()));
    let policy = RedirectPolicy {
        check: None,
        jar: Some(&jar),
    };
    let client = Client::new();
    // The request writes sid and extra by hand; the redirect sets sid, so
    // the jar's value takes over and the hand-written one is dropped.
    let req = Request::get(&format!("{}/set", srv.url())).header("Cookie", "sid=stale; extra=x");
    let text = follow_redirects(&client, &req, &policy).unwrap().text();
    assert!(
        text.contains("Cookie: extra=x; old=1; sid=fresh; theme=dark\n"),
        "{text}"
    );
    assert_eq!(
        *jar.calls.lock().unwrap(),
        [format!("{}/set sid=fresh; Path=/ | theme=dark", srv.url())]
    );
    // The jar's cookies go on a request that has none of its own.
    let text = follow_redirects(&client, &Request::get(&srv.url()), &policy)
        .unwrap()
        .text();
    assert!(
        text.contains("Cookie: old=1; sid=fresh; theme=dark\n"),
        "{text}"
    );
}

#[test]
fn many_threads_share_a_client() {
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    let srv = TestServer::new(move |r| {
        counter.fetch_add(1, Ordering::Relaxed);
        std::thread::sleep(Duration::from_millis(20));
        Response::text(200, r.path.clone())
    });
    let client = Client::new();
    let started = Instant::now();
    let threads: Vec<_> = (0..16)
        .map(|i| {
            let client = client.clone();
            let url = format!("{}/{i}", srv.url());
            std::thread::spawn(move || client.send(&Request::get(&url)).unwrap().text())
        })
        .collect();
    for (i, t) in threads.into_iter().enumerate() {
        assert_eq!(t.join().unwrap(), format!("/{i}"));
    }
    assert_eq!(hits.load(Ordering::Relaxed), 16);
    // They ran side by side, not one after another.
    assert!(
        started.elapsed() < Duration::from_millis(250),
        "{:?}",
        started.elapsed()
    );
}

#[test]
fn shareable_types() {
    fn send_sync<T: Send + Sync>() {}
    send_sync::<Client>();
    send_sync::<Request>();
    send_sync::<apic_httpc::Response>();
    send_sync::<apic_httpc::Error>();
    send_sync::<CancelToken>();
    send_sync::<Proxy>();
    send_sync::<apic_httpd::Request>();
    send_sync::<apic_httpd::TestServer>();
}
