use std::sync::{Arc, Mutex};

use apic_httpd::{Request as HttpRequest, Response as HttpResponse, TestServer};

use super::*;
use crate::gourl::UrlParts;
use crate::proxy::{env_proxy, no_proxy_match};
use crate::{Timings, EXIT_USAGE};

/// An HTTP proxy that answers every absolute-URI request itself,
/// recording what it was asked for, so a test can tell whether a request
/// went through it without a second server behind it.
fn proxy_server(name: &'static str) -> (TestServer, Arc<Mutex<Vec<String>>>) {
    let seen: Arc<Mutex<Vec<String>>> = Arc::default();
    let sink = seen.clone();
    let srv = TestServer::new(move |r: &HttpRequest| {
        sink.lock().unwrap().push(format!(
            "{} {} auth={}",
            r.method,
            r.target,
            r.header("Proxy-Authorization").unwrap_or("")
        ));
        HttpResponse::json(200, format!(r#"{{"via":"{name}"}}"#))
    });
    (srv, seen)
}

fn direct_server() -> TestServer {
    TestServer::new(|_: &HttpRequest| HttpResponse::new(200).with_body(r#"{"via":"direct"}"#))
}

const PROXY_HTTP: &str =
    "\n### Ping\n# @name ping\n# @assert status == 200\nGET {{baseUrl}}/ping\n";

fn proxy_project(base_url: &str, yaml: &str) -> tempfile::TempDir {
    let env = format!(r#"{{"dev": {{"baseUrl": "{base_url}"}}}}"#);
    let mut files = vec![
        ("api.http", PROXY_HTTP),
        ("http-client.env.json", env.as_str()),
    ];
    if !yaml.is_empty() {
        files.push(("apic.yaml", yaml));
    }
    write_project(&files)
}

fn dev() -> Options {
    Options {
        env: "dev".into(),
        ..Options::default()
    }
}

fn via(res: &Result) -> String {
    let body = res
        .response
        .as_ref()
        .unwrap()
        .body
        .as_json()
        .unwrap_or("{}");
    let v: serde_json::Value = serde_json::from_str(body).unwrap_or_default();
    v["via"].as_str().unwrap_or("").to_string()
}

#[test]
fn proxy_precedence_flag_config_environment() {
    let (flag_proxy, flag_seen) = proxy_server("flag");
    let (conf_proxy, conf_seen) = proxy_server("config");
    let (env_proxy, env_seen) = proxy_server("env");
    let target = direct_server();
    // The target is on 127.0.0.1, which the environment's proxy would
    // skip the way every client does; give it a name the proxy answers
    // for instead, since the proxies never dial anything.
    let _env = EnvGuard::set(&[
        ("HTTP_PROXY", &env_proxy.url()),
        ("NO_PROXY", ""),
        ("no_proxy", ""),
    ]);
    let yaml = format!("proxy: {}\n", conf_proxy.url());

    let dir = proxy_project("http://api.example.test", &yaml);
    let r = new_runner(
        dir.path(),
        Options {
            proxy: flag_proxy.url(),
            ..dev()
        },
    );
    assert_eq!(via(&run_ok(&r, "ping")), "flag", "--proxy should win");
    let seen = flag_seen.lock().unwrap().clone();
    assert!(
        seen.len() == 1 && seen[0].starts_with("GET http://api.example.test/ping"),
        "proxy saw {seen:?}"
    );

    let r = new_runner(dir.path(), dev());
    assert_eq!(
        via(&run_ok(&r, "ping")),
        "config",
        "apic.yaml proxy should beat the environment"
    );

    let dir = proxy_project("http://api.example.test", "");
    let r = new_runner(dir.path(), dev());
    assert_eq!(
        via(&run_ok(&r, "ping")),
        "env",
        "HTTP_PROXY should apply without flag or config"
    );
    assert!(conf_seen.lock().unwrap().len() == 1 && env_seen.lock().unwrap().len() == 1);

    // --no-proxy ignores all three and goes straight to the target.
    let dir = proxy_project(&target.url(), &yaml);
    let r = new_runner(
        dir.path(),
        Options {
            proxy: flag_proxy.url(),
            no_proxy: true,
            ..dev()
        },
    );
    assert_eq!(
        via(&run_ok(&r, "ping")),
        "direct",
        "--no-proxy should go direct"
    );
    assert!(
        flag_seen.lock().unwrap().len() == 1 && conf_seen.lock().unwrap().len() == 1,
        "--no-proxy still touched a proxy"
    );
}

#[test]
fn proxy_no_proxy_list_and_reporting() {
    let _env = EnvGuard::set(&[]);
    let (proxy, seen) = proxy_server("proxy");
    let target = direct_server();
    let host = target.url().trim_start_matches("http://").to_string();
    let proxy_host = proxy.url().trim_start_matches("http://").to_string();
    let with_auth = format!("http://alice:s3cret@{proxy_host}");
    let dir = proxy_project(
        &target.url(),
        &format!("proxy: {with_auth}\nnoProxy: [\".example.test\", \"{host}\"]\n"),
    );
    let r = new_runner(dir.path(), dev());

    // The target is on the noProxy list, so the request goes direct and
    // describe says so.
    assert_eq!(via(&run_ok(&r, "ping")), "direct");
    assert!(seen.lock().unwrap().is_empty());
    let d = r.describe(&lookup(&r, "ping"));
    let p = d.proxy.expect("a proxy is configured");
    assert!(
        p.off && p.source == "noProxy" && p.to_string() == "none (noProxy)",
        "describe proxy = {p:?}"
    );

    // Another host goes through it, with the credentials sent to the
    // proxy and masked everywhere apic shows the setting.
    let info = r.proxy_info("http://other.example.org/x").expect("a proxy");
    assert!(
        !info.off && info.source == "apic.yaml" && info.url == format!("http://***@{proxy_host}"),
        "proxy_info = {info:?}"
    );
    assert_eq!(info.raw(), Some(with_auth.as_str()));
    let dir = proxy_project("http://other.example.org", &format!("proxy: {with_auth}\n"));
    let r = new_runner(dir.path(), dev());
    let res = run_ok(&r, "ping");
    let seen = seen.lock().unwrap().clone();
    assert!(
        via(&res) == "proxy" && seen.len() == 1 && seen[0].contains("auth=Basic "),
        "proxy saw {seen:?}"
    );
    let data = serde_json::to_string(&res).unwrap();
    assert!(
        !data.contains("s3cret")
            && data.contains(&format!(
                r#""proxy":{{"url":"http://***@{proxy_host}","source":"apic.yaml"}}"#
            )),
        "result JSON should carry the masked proxy:\n{data}"
    );
}

#[test]
fn proxy_rejects_bad_urls() {
    let dir = proxy_project(
        "http://api.example.test",
        "proxy: ftp://proxy.internal:21\n",
    );
    let p = load(dir.path());
    let err = Runner::new(p.clone(), dev()).err().expect("an error");
    assert!(
        exit(Some(&err)) == EXIT_USAGE && err.to_string().contains("apic.yaml: proxy scheme"),
        "{err}"
    );
    assert_eq!(crate::code_of(&err), crate::Code::Proxy);
    let err = Runner::new(
        p.clone(),
        Options {
            proxy: "http://".into(),
            ..dev()
        },
    )
    .err()
    .expect("an error");
    assert!(err.to_string().contains("--proxy: bad proxy URL"), "{err}");
    // A bare host:port is an HTTP proxy, as for curl -x.
    let r = Runner::new(
        p,
        Options {
            proxy: "proxy.internal:3128".into(),
            ..dev()
        },
    )
    .unwrap();
    let info = r.proxy_info("").expect("a proxy");
    assert!(
        info.url == "http://proxy.internal:3128" && info.source == "--proxy",
        "{info:?}"
    );
}

#[test]
fn env_proxy_skips_loopback_and_honours_no_proxy() {
    let _env = EnvGuard::set(&[
        ("HTTPS_PROXY", "http://proxy.internal:3128"),
        ("HTTP_PROXY", ""),
        ("http_proxy", ""),
        ("NO_PROXY", ".internal, 10.0.0.0/8"),
    ]);
    for (raw, want) in [
        (
            "https://api.example.com/x",
            "http://proxy.internal:3128 (HTTPS_PROXY)",
        ),
        ("https://localhost:8443/x", "none (noProxy)"),
        ("https://127.0.0.1/x", "none (noProxy)"),
        ("https://db.internal/x", "none (noProxy)"),
        ("https://10.1.2.3/x", "none (noProxy)"),
        ("http://api.example.com/x", ""),
        (
            "https://api.example.com:443",
            "http://proxy.internal:3128 (HTTPS_PROXY)",
        ),
    ] {
        let u = UrlParts::parse(raw).unwrap();
        let got = env_proxy(&u).map(|p| p.to_string()).unwrap_or_default();
        assert_eq!(got, want, "{raw}");
    }
}

#[test]
fn bad_proxy_variable_refuses_the_request() {
    let _env = EnvGuard::set(&[("HTTP_PROXY", "ftp://proxy.internal:21")]);
    // Loopback would skip the proxy; a name goes through it, and the
    // broken setting must not become a silent direct connection.
    let dir = proxy_project("http://api.example.test", "");
    let r = new_runner(dir.path(), dev());
    let err = run_err(&r, "ping");
    assert!(
        exit(Some(&err)) == EXIT_USAGE
            && err.to_string().contains("HTTP_PROXY: proxy scheme \"ftp\""),
        "{err}"
    );
    let info = r.describe(&lookup(&r, "ping")).proxy.expect("a proxy");
    assert!(
        !info.error.is_empty() && info.to_string().starts_with("invalid (HTTP_PROXY): "),
        "describe proxy = {info:?}"
    );
}

/// The timings a result reports are the client's, in whole milliseconds.
#[test]
fn timings_are_whole_milliseconds() {
    let ms = std::time::Duration::from_micros;
    let got = Timings::from_http(&apic_httpc::Timings {
        dns: ms(1_900),
        connect: ms(2_100),
        tls: ms(0),
        ttfb: ms(4_999),
        total: ms(5_000),
        reused: false,
        started: std::time::Instant::now(),
    });
    assert_eq!(
        got,
        Timings {
            dns_ms: 1,
            connect_ms: 2,
            tls_ms: 0,
            ttfb_ms: 4,
            total_ms: 5,
            reused: false
        }
    );
}

#[test]
fn no_proxy_match_cases() {
    let list = [
        "localhost",
        ".corp.example",
        "*.svc",
        "10.0.0.0/8",
        "db.internal:5432",
        "EXACT.Host",
    ];
    for (raw, want) in [
        ("http://localhost/", true),
        ("https://a.corp.example/", true),
        ("https://corp.example/", true),
        ("https://a.b.svc/", true),
        ("http://10.20.30.40/", true),
        ("http://11.0.0.1/", false),
        ("http://db.internal:5432/", true),
        ("http://db.internal:5433/", false),
        ("http://exact.host/", true),
        ("http://notexact.host/", false),
        ("https://api.example.com/", false),
        ("https://corp.example.com/", false),
        ("https://xcorp.example/", false),
        ("http://db.internal/", false),
        ("http://DB.INTERNAL:5432/", true),
    ] {
        let u = UrlParts::parse(raw).unwrap();
        assert_eq!(no_proxy_match(&list, &u), want, "{raw}");
    }
    let any = UrlParts {
        scheme: "https".into(),
        host: "anything".into(),
    };
    assert!(no_proxy_match(&["*"], &any), "* should match everything");
}
