use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use apic_httpc::{follow_redirects, Client, RedirectPolicy, Request, Response, RoundTrip};
use apic_httpd::TestServer;

use crate::apply::{apply, Env};
use crate::digest::{
    digest_hash, digest_host, digest_response, hex_hash, parse_auth_params,
    parse_digest_challenges, DigestChallenge, DigestState, DigestTransport,
};
use crate::spec::parse;

fn params(header: &str) -> std::collections::BTreeMap<String, String> {
    parse_auth_params(header.strip_prefix("Digest ").unwrap_or(header))
}

fn p<'a>(map: &'a std::collections::BTreeMap<String, String>, key: &str) -> &'a str {
    map.get(key).map(String::as_str).unwrap_or("")
}

// Pins the computation to the example of RFC 2617 section 3.5 (the MD5
// vector RFC 7616 keeps), then checks the SHA-256 and -sess variants
// against the formulas of RFC 7616 section 3.4.
#[test]
fn digest_vectors() {
    let cs = parse_digest_challenges(&[
        r#"Digest realm="testrealm@host.com", qop="auth,auth-int", nonce="dcd98b7102dd2f0e8b11d0f600bfb0c093", opaque="5ccc069c403ebaf9f0171e9517f40e41""#,
    ]);
    assert!(
        cs.len() == 1 && cs[0].algorithm == "MD5" && cs[0].qop.len() == 2,
        "parsed {cs:?}"
    );
    let got = digest_response(
        &cs[0],
        "Mufasa",
        "Circle Of Life",
        "GET",
        "/dir/index.html",
        b"",
        "0a4f113b",
        1,
    )
    .expect("not answered");
    assert_eq!(
        got,
        r#"Digest username="Mufasa", realm="testrealm@host.com", nonce="dcd98b7102dd2f0e8b11d0f600bfb0c093", uri="/dir/index.html", response="6629fae49393a05397450978507c4ef1", algorithm=MD5, qop=auth, nc=00000001, cnonce="0a4f113b", opaque="5ccc069c403ebaf9f0171e9517f40e41""#
    );
    let m = params(&got);
    assert!(
        p(&m, "qop") == "auth"
            && p(&m, "nc") == "00000001"
            && p(&m, "uri") == "/dir/index.html"
            && p(&m, "opaque") == "5ccc069c403ebaf9f0171e9517f40e41"
            && p(&m, "username") == "Mufasa"
            && p(&m, "algorithm") == "MD5",
        "params {m:?}"
    );

    // The same inputs under SHA-256, SHA-256-sess and auth-int, computed
    // by hand from the RFC's formulas.
    let h = digest_hash("SHA-256").unwrap();
    let hh = |s: &str| hex_hash(h, s.as_bytes());
    let ha1 = hh("Mufasa:testrealm@host.com:Circle Of Life");
    let ha2 = hh("GET:/dir/index.html");
    let want = hh(&format!(
        "{ha1}:dcd98b7102dd2f0e8b11d0f600bfb0c093:00000002:0a4f113b:auth:{ha2}"
    ));
    let mut c = cs[0].clone();
    c.algorithm = "SHA-256".to_string();
    let got = digest_response(
        &c,
        "Mufasa",
        "Circle Of Life",
        "GET",
        "/dir/index.html",
        b"",
        "0a4f113b",
        2,
    )
    .unwrap();
    let m = params(&got);
    assert!(
        p(&m, "response") == want && p(&m, "nc") == "00000002" && p(&m, "algorithm") == "SHA-256",
        "SHA-256: {m:?} want response {want}"
    );
    c.algorithm = "SHA-256-SESS".to_string();
    let sess = hh(&format!(
        "{ha1}:dcd98b7102dd2f0e8b11d0f600bfb0c093:0a4f113b"
    ));
    let body = br#"{"a":1}"#;
    let ha2int = hh(&format!("POST:/dir/index.html:{}", hex_hash(h, body)));
    let want = hh(&format!(
        "{sess}:dcd98b7102dd2f0e8b11d0f600bfb0c093:00000001:0a4f113b:auth-int:{ha2int}"
    ));
    let got = digest_response(
        &c,
        "Mufasa",
        "Circle Of Life",
        "POST",
        "/dir/index.html",
        body,
        "0a4f113b",
        1,
    )
    .unwrap();
    let m = params(&got);
    assert!(
        p(&m, "response") == want
            && p(&m, "qop") == "auth-int"
            && p(&m, "algorithm") == "SHA-256-SESS",
        "SHA-256-sess auth-int: {m:?} want response {want}"
    );
    // Without qop the RFC 2069 form applies.
    c.algorithm = "MD5".to_string();
    c.qop.clear();
    let md = digest_hash("MD5").unwrap();
    let mh = |s: &str| hex_hash(md, s.as_bytes());
    let want = mh(&format!(
        "{}:dcd98b7102dd2f0e8b11d0f600bfb0c093:{}",
        mh("Mufasa:testrealm@host.com:Circle Of Life"),
        mh("GET:/dir/index.html")
    ));
    let got = digest_response(
        &c,
        "Mufasa",
        "Circle Of Life",
        "GET",
        "/dir/index.html",
        b"",
        "0a4f113b",
        1,
    )
    .unwrap();
    let m = params(&got);
    assert!(
        p(&m, "response") == want && p(&m, "qop").is_empty() && p(&m, "nc").is_empty(),
        "no qop: {m:?} want response {want}"
    );
    // A hashed user name and quoted parameters.
    c.userhash = true;
    c.realm = "a \"b\" \\c".to_string();
    let got = digest_response(&c, "u", "p", "GET", "/", b"", "c", 1).unwrap();
    assert!(
        got.starts_with(&format!(
            "Digest username=\"{}\", realm=\"a \\\"b\\\" \\\\c\", ",
            mh("u:a \"b\" \\c")
        )) && got.contains(", userhash=true, algorithm=MD5"),
        "{got}"
    );
    assert_eq!(p(&params(&got), "realm"), "a \"b\" \\c");
}

#[test]
fn digest_challenge_parsing() {
    // Two challenges in one header, a quoted comma, a Basic one to skip, and
    // an unknown algorithm to pass over.
    let values = [
        r#"Basic realm="site", Digest realm="a, b", nonce="n1", algorithm=SHA-512-256, qop="auth""#,
        r#"Digest realm="c", nonce="n2", stale=TRUE, userhash=true"#,
    ];
    let cs = parse_digest_challenges(&values);
    assert_eq!(cs.len(), 2, "challenges = {cs:?}");
    assert!(
        cs[0].realm == "a, b" && cs[0].nonce == "n1" && cs[0].algorithm == "SHA-512-256",
        "first = {:?}",
        cs[0]
    );
    assert!(
        cs[1].realm == "c" && cs[1].stale && cs[1].userhash && cs[1].algorithm == "MD5",
        "second = {:?}",
        cs[1]
    );
    assert!(
        digest_hash(&cs[0].algorithm).is_none(),
        "SHA-512-256 should be unsupported"
    );
    assert!(
        digest_response(&cs[0], "u", "p", "GET", "/", b"", "c", 1).is_none(),
        "unsupported algorithm must not be answered"
    );
}

/// The request line's path and query, as the server received it.
fn request_uri(r: &apic_httpd::Request) -> String {
    r.target.clone()
}

/// Challenges every request without a valid Authorization and verifies
/// the ones that carry one by recomputing the response.
fn digest_server(
    algorithm: &'static str,
    qop: &'static str,
    nonce: &'static str,
) -> (TestServer, Arc<AtomicUsize>) {
    let hits = Arc::new(AtomicUsize::new(0));
    let count = hits.clone();
    let srv = TestServer::new(move |r: &apic_httpd::Request| {
        count.fetch_add(1, Ordering::SeqCst);
        let challenge = format!(
            r#"Digest realm="apic", nonce="{nonce}", algorithm={algorithm}, qop="{qop}", opaque="op""#
        );
        let authz = r.header("Authorization").unwrap_or("");
        if !authz.starts_with("Digest ") {
            return apic_httpd::Response::error(401, "challenge")
                .with_header("WWW-Authenticate", &challenge);
        }
        let got = params(authz);
        let c = parse_digest_challenges(&[challenge.as_str()]).remove(0);
        let nc = u32::from_str_radix(p(&got, "nc"), 16).unwrap_or(0);
        let want = digest_response(
            &c,
            "alice",
            "s3cret",
            &r.method,
            &request_uri(r),
            &r.body,
            p(&got, "cnonce"),
            nc,
        )
        .unwrap();
        let want = params(&want);
        if p(&got, "nonce") != nonce {
            return apic_httpd::Response::error(401, "stale")
                .with_header("WWW-Authenticate", &format!("{challenge}, stale=true"));
        }
        if p(&want, "response") != p(&got, "response") || p(&got, "opaque") != "op" {
            return apic_httpd::Response::error(401, "bad")
                .with_header("WWW-Authenticate", &challenge);
        }
        apic_httpd::Response::text(
            200,
            format!("ok nc={} qop={}", p(&got, "nc"), p(&got, "qop")),
        )
    });
    (srv, hits)
}

fn send(dt: &DigestTransport, req: &Request) -> Response {
    follow_redirects(dt, req, &RedirectPolicy::default()).unwrap()
}

#[test]
fn digest_transport_answers_challenge_and_reuses_nonce() {
    let (srv, hits) = digest_server("SHA-256", "auth", "nonce-1");
    let state = Arc::new(DigestState::new());
    let run = |method: &str, body: &str, pass: &str| {
        let dt =
            DigestTransport::new(Box::new(Client::new()), "alice", pass).with_state(state.clone());
        let req = Request::new(method, &format!("{}/thing?x=1", srv.url())).body(body);
        let resp = send(&dt, &req);
        (resp, dt)
    };

    let (resp, dt) = run("GET", "", "s3cret");
    assert_eq!(
        (resp.status_code, resp.text().as_str()),
        (200, "ok nc=00000001 qop=auth")
    );
    assert!(dt.challenged() && dt.answered() && dt.rounds() == 2);
    assert_eq!(dt.host(), srv.url());
    // The second request to the same server answers on the first try with
    // the next nonce count, body included.
    let (resp, dt) = run("POST", r#"{"a":1}"#, "s3cret");
    assert_eq!(
        (resp.status_code, resp.text().as_str()),
        (200, "ok nc=00000002 qop=auth")
    );
    assert!(!dt.challenged() && dt.answered() && dt.rounds() == 1);
    assert_eq!(hits.load(Ordering::SeqCst), 3, "server hits");
    // A wrong password is answered once and then reported as the 401 it is.
    let (resp, dt) = run("GET", "", "wrong");
    assert_eq!(resp.status_code, 401);
    assert_eq!(dt.rounds(), 1);
}

#[test]
fn digest_transport_without_state_is_challenged_every_time() {
    let (srv, hits) = digest_server("MD5", "auth", "nonce-2");
    for _ in 0..2 {
        let mut dt = DigestTransport::new(Box::new(Client::new()), "alice", "s3cret");
        dt.cnonce = Some(Box::new(|| "fixed".to_string()));
        let resp = send(&dt, &Request::get(&format!("{}/a b?q=\"x\"", srv.url())));
        assert_eq!(
            (resp.status_code, resp.text().as_str()),
            (200, "ok nc=00000001 qop=auth")
        );
        assert!(dt.challenged() && dt.rounds() == 2);
    }
    assert_eq!(hits.load(Ordering::SeqCst), 4);
}

#[test]
fn digest_transport_handles_stale_nonce_and_auth_int() {
    let (srv, _) = digest_server("MD5", "auth-int", "nonce-A");
    let state = Arc::new(DigestState::new());
    // Prime the state with an old nonce, as if the server had restarted.
    state.set(
        &srv.url(),
        DigestChallenge {
            realm: "apic".to_string(),
            nonce: "old".to_string(),
            algorithm: "MD5".to_string(),
            qop: vec!["auth-int".to_string()],
            opaque: "op".to_string(),
            ..DigestChallenge::default()
        },
    );
    let dt =
        DigestTransport::new(Box::new(Client::new()), "alice", "s3cret").with_state(state.clone());
    let req = Request::new("PUT", &format!("{}/thing", srv.url())).body("payload");
    let resp = send(&dt, &req);
    assert_eq!(
        (resp.status_code, resp.text().as_str()),
        (200, "ok nc=00000001 qop=auth-int")
    );
    assert!(dt.rounds() == 2 && dt.challenged());
}

#[test]
fn digest_never_answers_another_host() {
    // api redirects to elsewhere, which challenges: the credentials belong
    // to api and stay there.
    let elsewhere_authz: Arc<Mutex<Vec<String>>> = Arc::default();
    let record = elsewhere_authz.clone();
    let elsewhere = TestServer::new(move |r: &apic_httpd::Request| {
        record
            .lock()
            .unwrap()
            .push(r.header("Authorization").unwrap_or("").to_string());
        apic_httpd::Response::error(401, "challenge").with_header(
            "WWW-Authenticate",
            r#"Digest realm="other", nonce="n", algorithm=MD5, qop="auth""#,
        )
    });
    let target = format!("{}/x", elsewhere.url());
    let api = TestServer::new(move |r: &apic_httpd::Request| {
        apic_httpd::Response::redirect(r, &target, 302)
    });
    let dt = DigestTransport::new(Box::new(Client::new()), "alice", "s3cret")
        .with_state(Arc::new(DigestState::new()));
    let resp = send(&dt, &Request::get(&format!("{}/thing", api.url())));
    let seen = elsewhere_authz.lock().unwrap().clone();
    assert!(
        resp.status_code == 401 && seen == [""] && !dt.answered(),
        "status={} authz={:?} answered={}",
        resp.status_code,
        seen,
        dt.answered()
    );
    assert_eq!(dt.host(), api.url(), "pinned to the first host");
    assert_eq!(dt.rounds(), 2);

    // Pinned by the caller, the same holds from the first request.
    let dt = DigestTransport::new(Box::new(Client::new()), "alice", "s3cret")
        .with_host(&digest_host(&format!("{}/thing", api.url())));
    assert_eq!(dt.host(), api.url());
    let resp = dt
        .round_trip(&Request::get(&format!("{}/x", elsewhere.url())))
        .unwrap();
    assert!(resp.status_code == 401 && !dt.answered() && dt.rounds() == 1);
}

#[test]
fn digest_without_challenge_passes_through() {
    let srv = TestServer::new(|r: &apic_httpd::Request| {
        apic_httpd::Response::text(
            200,
            format!("authz={}", r.header("Authorization").unwrap_or("")),
        )
    });
    let dt = DigestTransport::new(Box::new(Client::new()), "u", "p")
        .with_state(Arc::new(DigestState::new()));
    let resp = send(&dt, &Request::get(&srv.url()));
    assert_eq!(resp.text(), "authz=");
    assert!(dt.rounds() == 1 && !dt.answered());
    // Applying a digest spec adds nothing: the transport does the work.
    let mut req = Request::get(&srv.url());
    let s = parse("digest u p").unwrap();
    apply(
        &apic_httpc::CancelToken::new(),
        &s,
        &mut req,
        b"",
        &Env::default(),
    )
    .unwrap();
    assert!(req.headers.is_empty());
}

#[test]
fn digest_hosts() {
    assert_eq!(
        digest_host("HTTPS://user:pw@API.example.com:8443/a?b#c"),
        "https://api.example.com:8443"
    );
    assert_eq!(digest_host("http://127.0.0.1:80"), "http://127.0.0.1:80");
}

#[test]
fn api_key_forms() {
    let bg = apic_httpc::CancelToken::new();
    let cases = [
        ("apikey {{key}}", "X-Api-Key", "", "k1"),
        (
            "apikey {{key}} header=X-Auth-Token",
            "X-Auth-Token",
            "",
            "k1",
        ),
        ("apikey {{key}} query=api_key", "", "api_key", "k1"),
        (
            r#"apikey {{key}} header=Authorization prefix="Token ""#,
            "Authorization",
            "",
            "Token k1",
        ),
    ];
    for (spec, header, query, value) in cases {
        let s = parse(spec).unwrap_or_else(|e| panic!("{spec}: {e}"));
        let s = s
            .render(|v| Ok::<_, String>(v.replace("{{key}}", "k1")))
            .unwrap();
        let mut req = Request::get("https://a.b/x?q=1");
        apply(&bg, &s, &mut req, b"", &Env::default()).unwrap_or_else(|e| panic!("{spec}: {e}"));
        if !header.is_empty() {
            assert_eq!(req.headers.get(header), Some(value), "{spec}: header");
        }
        if !query.is_empty() {
            assert_eq!(
                req.url,
                format!("https://a.b/x?q=1&{query}={value}"),
                "{spec}: query"
            );
        }
        assert_eq!(
            s.api_key_placement(),
            (header.to_string(), query.to_string()),
            "{spec}: placement"
        );
    }
    // The query form appends to the query as written: nothing is reordered,
    // re-encoded or dropped, even a pair a strict parser would refuse.
    let s = parse("apikey k1 query=api_key").unwrap();
    let mut req = Request::get("https://a.b/x?b=2&a=%2Fpath;v=1");
    apply(&bg, &s, &mut req, b"", &Env::default()).unwrap();
    assert_eq!(req.url, "https://a.b/x?b=2&a=%2Fpath;v=1&api_key=k1");
    let mut req = Request::get("https://a.b/x");
    apply(&bg, &s, &mut req, b"", &Env::default()).unwrap();
    assert_eq!(
        req.url, "https://a.b/x?api_key=k1",
        "query form on a bare URL"
    );
    let mut req = Request::get("https://a.b/x?#top");
    apply(&bg, &s, &mut req, b"", &Env::default()).unwrap();
    assert_eq!(
        req.url, "https://a.b/x?api_key=k1#top",
        "before the fragment"
    );
    let s = parse("apikey \"a b&c\" query=\"api key\"").unwrap();
    let mut req = Request::get("https://a.b/x");
    apply(&bg, &s, &mut req, b"", &Env::default()).unwrap();
    assert_eq!(req.url, "https://a.b/x?api+key=a+b%26c", "escaped");
    // A key containing '=' stays a key; header and query exclude each other.
    let s = parse("apikey abc=def==").unwrap();
    assert_eq!(s.args[0], "abc=def==", "key with '=': {s:?}");
    for bad in [
        "apikey",
        "apikey k header=A query=b",
        "apikey k more",
        "digest user",
        "digest",
    ] {
        assert!(parse(bad).is_err(), "{bad:?} should not parse");
    }
}
