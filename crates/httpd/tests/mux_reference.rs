//! The mux against a table of patterns, requests and conflicts whose
//! expected answers are recorded in `testdata/mux_reference*.txt`, one
//! line each: `METHOD target|host => status|Location|Allow|body`, and
//! `CONFLICT a || b => ok|conflict`.

use apic_httpd::{Mux, Request, Response};

const PATTERNS: [&str; 18] = [
    "/",
    "GET /todos",
    "POST /todos",
    "GET /todos/{id}",
    "GET /todos/latest",
    "DELETE /todos/{id}",
    "/files/",
    "/static/{rest...}",
    "GET /exact/{$}",
    "api.example.com/",
    "/any",
    "GET /basic-auth/{user}/{password}",
    "GET /a/{x}/c",
    "GET /a/b/c",
    "PUT /only/",
    "HEAD /headonly",
    "GET /sp%20ace/{v}",
    "GET /{$}",
];

/// The patterns of the second table: no catch-all, so a miss is a 404 or
/// a 405, and two patterns for one host only.
const STRICT: [&str; 7] = [
    "GET /todos/{id}",
    "DELETE /todos/{id}",
    "POST /todos",
    "PUT /dir/",
    "/free",
    "h.test/hosted",
    "POST h.test/todos/{id}",
];

#[test]
fn matches_the_reference() {
    let checked = check(&PATTERNS, include_str!("../testdata/mux_reference.txt"));
    assert!(checked > 100);
}

#[test]
fn matches_the_reference_without_a_catch_all() {
    let checked = check(&STRICT, include_str!("../testdata/mux_reference_405.txt"));
    assert_eq!(checked, 24);
}

fn check(patterns: &[&'static str], reference: &str) -> usize {
    let mut mux = Mux::new();
    for &pattern in patterns {
        mux.handle_fn(pattern, move |r| {
            Response::new(200).with_body(format!(
                "{pattern} id={} rest={} x={} y={} v={}",
                r.path_value("id"),
                r.path_value("rest"),
                r.path_value("x"),
                r.path_value("y"),
                r.path_value("v")
            ))
        })
        .unwrap();
    }
    let mut checked = 0;
    for line in reference.lines() {
        let (left, want) = line.split_once(" => ").unwrap();
        if let Some(pair) = left.strip_prefix("CONFLICT ") {
            let (a, b) = pair.split_once(" || ").unwrap();
            let mut m: Mux<()> = Mux::new();
            m.handle(a, ()).unwrap();
            let got = if m.handle(b, ()).is_ok() {
                "ok"
            } else {
                "conflict"
            };
            assert_eq!(got, want, "{line}");
        } else {
            let (request, host) = left.split_once('|').unwrap();
            let (method, target) = request.split_once(' ').unwrap();
            let req = Request::new(method, &target.replace(' ', "%20")).with_host(host);
            let resp = mux.serve(&req);
            let got = format!(
                "{}|{}|{}|{}",
                resp.status,
                resp.headers.get("Location").unwrap_or(""),
                resp.headers.get("Allow").unwrap_or(""),
                String::from_utf8_lossy(&resp.body).replace('\n', "\\n")
            );
            assert_eq!(got, want, "{line}");
        }
        checked += 1;
    }
    checked
}
