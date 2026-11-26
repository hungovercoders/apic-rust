use std::time::Duration;

use apic_httpd::{Request as HttpRequest, Response as HttpResponse, TestServer};

use super::*;

#[test]
fn timings_breakdown_and_connection_reuse() {
    let srv = TestServer::new_tls(|_: &HttpRequest| {
        std::thread::sleep(Duration::from_millis(15));
        HttpResponse::new(200).with_body(r#"{"ok":true}"#)
    });
    let dir = write_project(&[
        (
            "http-client.env.json",
            &format!(r#"{{"dev": {{"baseUrl": "{}"}}}}"#, srv.url()),
        ),
        (
            "api.http",
            "### a\n# @name a\nGET {{baseUrl}}/a\n\n### b\n# @name b\nGET {{baseUrl}}/b\n",
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
    let res = run_ok(&r, "a");
    let resp = res.response.as_ref().unwrap();
    let tm = resp.timings.expect("timings");
    // A first request resolves, connects and shakes hands; each stage is
    // bounded by the next and the whole by the round trip.
    assert!(
        !tm.reused && tm.tls_ms >= 0 && tm.connect_ms >= 0 && tm.dns_ms >= 0,
        "first request: {tm:?}"
    );
    assert!(
        tm.ttfb_ms >= 15 && tm.ttfb_ms <= tm.total_ms && tm.total_ms == resp.duration_ms,
        "ttfb/total: {tm:?} duration={}",
        resp.duration_ms
    );
    let data = serde_json::to_string(&res).unwrap();
    assert!(
        data.contains(r#""timings":{"dns_ms":"#) && data.contains(r#""reused":false}"#),
        "json: {data}"
    );
    // The second request in the same invocation rides the same connection.
    let res = run_ok(&r, "b");
    let tm = res.response.as_ref().unwrap().timings.unwrap();
    assert!(
        tm.reused && tm.dns_ms == 0 && tm.connect_ms == 0 && tm.tls_ms == 0 && tm.ttfb_ms >= 15,
        "second request: {tm:?}"
    );
}
