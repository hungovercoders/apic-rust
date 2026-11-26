//! The server, seen from a client.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use apic_httpc::{CancelToken, Client, ErrorKind, Request as ClientRequest, TlsConfig};
use apic_httpd::{serve, Listener, Mux, Request, Response, Server, TestServer};

/// Sends raw bytes and returns everything the server answers before it
/// closes the connection.
fn raw(addr: std::net::SocketAddr, request: &str) -> String {
    let mut conn = TcpStream::connect(addr).unwrap();
    conn.write_all(request.as_bytes()).unwrap();
    let mut out = Vec::new();
    conn.read_to_end(&mut out).unwrap();
    String::from_utf8_lossy(&out).into_owned()
}

fn api() -> Mux {
    let mut mux = Mux::new();
    mux.handle_fn("GET /todos/{id}", |r| {
        Response::json(
            200,
            format!(
                "{{\"id\":\"{}\",\"pattern\":\"{}\"}}",
                r.path_value("id"),
                r.pattern
            ),
        )
    })
    .unwrap();
    mux.handle_fn("POST /todos", |r| Response::json(201, r.body.to_vec()))
        .unwrap();
    mux.handle_fn("GET /status/{code}", |r| {
        Response::new(r.path_value("code").parse().unwrap_or(500))
    })
    .unwrap();
    mux.handle_fn("GET /plain", |_| Response::new(200).with_body("just text"))
        .unwrap();
    mux.handle_fn("GET /page", |_| {
        Response::new(200).with_body("<!doctype html><title>apic</title>")
    })
    .unwrap();
    mux.handle_fn("GET /binary", |_| {
        Response::new(200).with_body(vec![0u8, 1, 2, 3])
    })
    .unwrap();
    mux.handle_fn("GET /big", |r| {
        let n: usize = r
            .query_get("n")
            .and_then(|n| n.parse().ok())
            .unwrap_or(5000);
        Response::text(200, "x".repeat(n))
    })
    .unwrap();
    mux.handle_fn("GET /redirect", |r| {
        Response::redirect(r, "/status/200", 302)
    })
    .unwrap();
    mux.handle_fn("GET /who", |r| {
        Response::text(
            200,
            format!(
                "{} {} tls={}",
                r.remote_addr.starts_with("127.0.0.1:"),
                r.host,
                r.tls.is_some()
            ),
        )
    })
    .unwrap();
    mux
}

#[test]
fn routes_and_defaults() {
    let srv = TestServer::new(api().into_handler());
    let client = Client::new();
    let get = |path: &str| {
        client
            .send(&ClientRequest::get(&format!("{}{path}", srv.url())))
            .unwrap()
    };

    let resp = get("/todos/42");
    assert_eq!(resp.status, "200 OK");
    assert_eq!(
        resp.text(),
        "{\"id\":\"42\",\"pattern\":\"GET /todos/{id}\"}"
    );
    assert_eq!(resp.header("Content-Type"), Some("application/json"));
    assert_eq!(resp.header("Content-Length"), Some("39"));
    // A Date in the HTTP format.
    let date = resp.header("Date").unwrap();
    assert!(date.ends_with(" GMT") && date.len() == 29, "{date}");

    let resp = client
        .send(&ClientRequest::post(
            &format!("{}/todos", srv.url()),
            "{\"title\":\"x\"}",
        ))
        .unwrap();
    assert_eq!(
        (resp.status.as_str(), resp.text().as_str()),
        ("201 Created", "{\"title\":\"x\"}")
    );

    // 404 and 405 as the mux words them.
    let resp = get("/nope");
    assert_eq!(resp.status, "404 Not Found");
    assert_eq!(resp.text(), "404 page not found\n");
    assert_eq!(
        resp.header("Content-Type"),
        Some("text/plain; charset=utf-8")
    );
    assert_eq!(resp.header("X-Content-Type-Options"), Some("nosniff"));
    let resp = client
        .send(&ClientRequest::new(
            "DELETE",
            &format!("{}/todos", srv.url()),
        ))
        .unwrap();
    assert_eq!(resp.status, "405 Method Not Allowed");
    assert_eq!(resp.header("Allow"), Some("POST"));
    assert_eq!(resp.text(), "Method Not Allowed\n");

    // A Content-Type is sniffed when the handler sets none; JSON has no
    // signature of its own.
    assert_eq!(
        get("/plain").header("Content-Type"),
        Some("text/plain; charset=utf-8")
    );
    assert_eq!(
        get("/page").header("Content-Type"),
        Some("text/html; charset=utf-8")
    );
    assert_eq!(
        get("/binary").header("Content-Type"),
        Some("application/octet-stream")
    );

    // Statuses: no body and no length for 204 and 304, a reason for an
    // unknown code.
    let resp = get("/status/204");
    assert_eq!(resp.status, "204 No Content");
    assert!(!resp.headers.contains("Content-Length") && !resp.headers.contains("Content-Type"));
    assert_eq!(get("/status/304").status, "304 Not Modified");
    assert_eq!(get("/status/418").status, "418 I'm a teapot");
    assert_eq!(get("/status/413").status, "413 Request Entity Too Large");
    assert_eq!(get("/status/599").status, "599 status code 599");
    assert_eq!(get("/status/200").header("Content-Length"), Some("0"));

    let resp = get("/redirect");
    assert_eq!(resp.status, "302 Found");
    assert_eq!(resp.header("Location"), Some("/status/200"));
    assert_eq!(resp.text(), "<a href=\"/status/200\">Found</a>.\n\n");

    assert_eq!(get("/who").text(), format!("true {} tls=false", srv.addr()));
}

#[test]
fn length_or_chunks() {
    let srv = TestServer::new(api().into_handler());
    let client = Client::new();
    let get = |n: usize| {
        client
            .send(
                &ClientRequest::get(&format!("{}/big?n={n}", srv.url()))
                    .header("Accept-Encoding", "identity"),
            )
            .unwrap()
    };
    // What fits the write buffer has a length; more is sent in chunks.
    let small = get(2048);
    assert_eq!(small.header("Content-Length"), Some("2048"));
    let large = get(2049);
    assert_eq!(large.header("Content-Length"), None);
    assert_eq!(large.body.len(), 2049);
    let text = raw(
        srv.addr(),
        "GET /big?n=3000 HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
    );
    assert!(
        text.contains("\r\nTransfer-Encoding: chunked\r\n"),
        "{text}"
    );

    // The wire carries canonical header names and the usual reason.
    let text = raw(
        srv.addr(),
        "GET /status/413 HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
    );
    assert!(
        text.starts_with("HTTP/1.1 413 Request Entity Too Large\r\n"),
        "{text}"
    );
    assert!(
        text.contains("\r\nContent-Length: 0\r\n") && text.contains("\r\nDate: "),
        "{text}"
    );
}

#[test]
fn head_requests() {
    let srv = TestServer::new(api().into_handler());
    let resp = Client::new()
        .send(&ClientRequest::new(
            "HEAD",
            &format!("{}/todos/7", srv.url()),
        ))
        .unwrap();
    assert_eq!(resp.status_code, 200);
    assert!(resp.body.is_empty());
    assert_eq!(resp.header("Content-Type"), Some("application/json"));
    assert_eq!(resp.header("Content-Length"), Some("38"));
    let text = raw(
        srv.addr(),
        "HEAD /plain HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
    );
    assert!(text.contains("\r\nContent-Length: 9\r\n"), "{text}");
    assert!(
        text.contains("\r\nContent-Type: text/plain; charset=utf-8\r\n"),
        "{text}"
    );
    assert!(text.ends_with("\r\n\r\n"), "{text}");
}

#[test]
fn request_fields() {
    let srv = TestServer::new(|r: &Request| {
        let mut lines = vec![
            format!("method={}", r.method),
            format!("path={}", r.path),
            format!("raw_path={}", r.raw_path),
            format!("raw_query={}", r.raw_query),
            format!("target={}", r.target),
            format!("proto={}", r.proto),
            format!("q={:?}", r.query()),
            format!("form={}", r.form_value("b")),
            format!("auth={:?}", r.basic_auth()),
            format!("cookie={:?}", r.cookie("sid")),
            format!("has_host_header={}", r.headers.contains("Host")),
        ];
        for (name, value) in r.headers.iter().filter(|(n, _)| n.starts_with("X-")) {
            lines.push(format!("{name}={value}"));
        }
        Response::text(200, lines.join("\n"))
    });
    let url = format!("http://u:p@{}/a%20b/c?x=1&y=two+words", srv.addr());
    let resp = Client::new()
        .send(
            &ClientRequest::post(&url, "b=from+body")
                .header("Content-Type", "application/x-www-form-urlencoded")
                .header("x-trace", "t1")
                .header("X-TRACE", "t2")
                .header("Cookie", "sid=abc"),
        )
        .unwrap();
    assert_eq!(
        resp.text(),
        "method=POST\npath=/a b/c\nraw_path=/a%20b/c\nraw_query=x=1&y=two+words\ntarget=/a%20b/c?x=1&y=two+words\nproto=HTTP/1.1\nq=[(\"x\", \"1\"), (\"y\", \"two words\")]\nform=from body\nauth=Some((\"u\", \"p\"))\ncookie=Some(\"abc\")\nhas_host_header=false\nX-Trace=t1\nX-Trace=t2"
    );
}

#[test]
fn tls_and_http2() {
    let handler = |r: &Request| {
        Response::text(
            200,
            format!(
                "{} {}",
                r.proto,
                r.tls.as_ref().map_or("-", |t| t.alpn.as_str())
            ),
        )
    };
    let trusting = |srv: &TestServer| {
        Client::builder()
            .tls(TlsConfig {
                ca_pem: Some(srv.cert_pem().as_bytes().to_vec()),
                ..TlsConfig::default()
            })
            .build()
            .unwrap()
    };
    let srv = TestServer::new_tls(handler);
    assert!(srv.url().starts_with("https://127.0.0.1:"));
    let resp = trusting(&srv)
        .send(&ClientRequest::get(&srv.url()))
        .unwrap();
    assert_eq!(resp.text(), "HTTP/1.1 http/1.1");

    let srv = TestServer::builder().http2(true).start(handler);
    let client = trusting(&srv);
    let resp = client.send(&ClientRequest::get(&srv.url())).unwrap();
    assert_eq!(
        (resp.proto.as_str(), resp.text().as_str()),
        ("HTTP/2.0", "HTTP/2.0 h2")
    );
    // Over HTTP/2 the write buffer is larger.
    let big = TestServer::builder()
        .http2(true)
        .start(api().into_handler());
    let client = trusting(&big);
    let get = |n: usize| {
        client
            .send(
                &ClientRequest::get(&format!("{}/big?n={n}", big.url()))
                    .header("Accept-Encoding", "identity"),
            )
            .unwrap()
    };
    assert_eq!(get(4096).header("Content-Length"), Some("4096"));
    let large = get(4097);
    assert_eq!(
        (large.header("Content-Length"), large.body.len()),
        (None, 4097)
    );
    let head = client
        .send(&ClientRequest::new("HEAD", &format!("{}/plain", big.url())))
        .unwrap();
    assert_eq!(
        (head.header("Content-Length"), head.body.len()),
        (Some("9"), 0)
    );
}

#[test]
fn streaming_body() {
    let srv = TestServer::new(|_| {
        let (resp, body) = Response::streaming(200);
        std::thread::spawn(move || {
            for i in 0..3 {
                assert!(body.send(format!("data: {i}\n\n")));
                std::thread::sleep(Duration::from_millis(20));
            }
        });
        resp.with_header("Content-Type", "text/event-stream")
    });
    let resp = Client::new().send(&ClientRequest::get(&srv.url())).unwrap();
    assert_eq!(resp.text(), "data: 0\n\ndata: 1\n\ndata: 2\n\n");
    assert_eq!(resp.header("Content-Type"), Some("text/event-stream"));
    assert_eq!(resp.header("Content-Length"), None);
    assert!(resp.timings.total >= Duration::from_millis(40));
    assert!(resp.timings.ttfb < resp.timings.total);
}

#[test]
fn slow_handlers_do_not_block_others() {
    let srv = TestServer::new(|r: &Request| {
        if r.path == "/slow" {
            std::thread::sleep(Duration::from_millis(300));
        }
        Response::text(200, r.path.clone())
    });
    let slow_url = format!("{}/slow", srv.url());
    let slow =
        std::thread::spawn(move || Client::new().send(&ClientRequest::get(&slow_url)).unwrap());
    std::thread::sleep(Duration::from_millis(50));
    let started = Instant::now();
    let resp = Client::new()
        .send(&ClientRequest::get(&format!("{}/fast", srv.url())))
        .unwrap();
    assert_eq!(resp.text(), "/fast");
    assert!(started.elapsed() < Duration::from_millis(200));
    assert_eq!(slow.join().unwrap().text(), "/slow");
}

#[test]
fn handler_that_panics_or_hangs_up() {
    let srv = TestServer::new(|r: &Request| match r.path.as_str() {
        "/panic" => panic!("handler failed on purpose"),
        "/hangup" => Response::hang_up(),
        _ => Response::text(200, "fine"),
    });
    let client = Client::new();
    for path in ["/panic", "/hangup"] {
        let err = client
            .send(&ClientRequest::get(&format!("{}{path}", srv.url())))
            .unwrap_err();
        assert_eq!(
            (err.kind, err.message.as_str()),
            (ErrorKind::Other, "EOF"),
            "{path}"
        );
    }
    // The server itself is unharmed.
    assert_eq!(
        client.send(&ClientRequest::get(&srv.url())).unwrap().text(),
        "fine"
    );
}

#[test]
fn shutdown_waits_for_requests_in_flight() {
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    let server = Server::bind("127.0.0.1:0", move |_| {
        std::thread::sleep(Duration::from_millis(150));
        counter.fetch_add(1, Ordering::SeqCst);
        Response::text(200, "finished")
    })
    .unwrap();
    assert_eq!(server.url(), format!("http://{}", server.addr()));
    let url = server.url();
    let client = Client::new();
    // An idle keep-alive connection must not hold the shutdown up.
    let idle = TcpStream::connect(server.addr()).unwrap();
    let in_flight = {
        let (client, url) = (client.clone(), url.clone());
        std::thread::spawn(move || client.send(&ClientRequest::get(&url)))
    };
    std::thread::sleep(Duration::from_millis(50));
    let started = Instant::now();
    server.shutdown();
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(in_flight.join().unwrap().unwrap().text(), "finished");
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    drop(idle);
    // Nothing listens any more; a second shutdown and the drop are harmless.
    let err = Client::new().send(&ClientRequest::get(&url)).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Connect);
    server.shutdown();
}

#[test]
fn blocking_serve_until_cancelled() {
    let listener = Listener::bind("127.0.0.1:0").unwrap();
    let addr = listener.addr();
    let stop = CancelToken::new();
    let token = stop.clone();
    let serving = std::thread::spawn(move || {
        serve(listener, |_| Response::text(200, "demo"), &token);
    });
    let resp = Client::new()
        .send(&ClientRequest::get(&format!("http://{addr}/")))
        .unwrap();
    assert_eq!(resp.text(), "demo");
    assert!(!serving.is_finished());
    stop.cancel();
    serving.join().unwrap();

    // A server built with a stop token stops when it is cancelled.
    let stop = CancelToken::new();
    let server = Server::builder()
        .stop_on(&stop)
        .start(Listener::bind("127.0.0.1:0").unwrap(), |_| {
            Response::new(204)
        })
        .unwrap();
    assert!(!server.stop_token().is_cancelled());
    stop.cancel();
    server.wait();
    assert!(TcpStream::connect(server.addr()).is_err());
}

#[test]
fn bind_errors() {
    let taken = Listener::bind("127.0.0.1:0").unwrap();
    let addr = taken.addr().to_string();
    let err = Listener::bind(&addr).unwrap_err();
    assert_eq!(
        err.to_string(),
        format!("listen tcp {addr}: bind: address already in use")
    );
    let err = Server::bind(&addr, |_| Response::new(200)).unwrap_err();
    assert_eq!(err.reason, "bind: address already in use");
    assert!(Listener::bind("256.0.0.1:1").is_err());
    let bad = Server::builder()
        .tls(apic_httpd::TlsOptions::default())
        .start(Listener::bind("127.0.0.1:0").unwrap(), |_| {
            Response::new(200)
        })
        .unwrap_err();
    assert_eq!(bad, "tls: failed to find any PEM data in certificate input");
}

#[test]
fn keep_alive_and_many_requests() {
    let srv = TestServer::new(|r: &Request| Response::text(200, r.text()));
    let client = Client::new();
    for i in 0..50 {
        let resp = client
            .send(&ClientRequest::post(&srv.url(), format!("n={i}")))
            .unwrap();
        assert_eq!(resp.text(), format!("n={i}"));
        assert_eq!(resp.timings.reused, i > 0);
    }
    // A megabyte each way.
    let big = vec![b'z'; 1 << 20];
    let resp = client
        .send(&ClientRequest::post(&srv.url(), big.clone()))
        .unwrap();
    assert_eq!(resp.body, big);
    srv.close();
}
