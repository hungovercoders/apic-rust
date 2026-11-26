//! The `--json` shape is a public contract: the keys, their order and
//! what is left out when empty are pinned here, byte for byte.

use apic_httpfile::Header;

use super::*;
use crate::{
    info, parse_result, usage, Body, Description, Iteration, Resolved, Response, Timings, TlsInfo,
    VarInfo, CODE_REF,
};

/// Writes `<`, `>`, `&` and U+2028 the way the output escapes them.
fn esc(s: &str) -> String {
    s.replace('&', "\\u0026")
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('\u{2028}', "\\u2028")
}

fn full() -> Result {
    let header = |n: &str, v: &str| Header {
        name: n.into(),
        value: v.into(),
    };
    Result {
        ok: false,
        request: Resolved {
            name: "get".into(),
            file: "a.http".into(),
            line: 3,
            method: "GET".into(),
            url: "https://h/x?a=1&b".into(),
            headers: vec![
                header("X-B", "<2>"),
                header("Accept", "a&b"),
                header("Cookie", "c"),
            ],
            body: "b\u{2028}".into(),
            auth: "bearer".into(),
            http_version: "HTTP/2".into(),
            tls: Some(TlsInfo {
                insecure: true,
                ..TlsInfo::default()
            }),
            ..Resolved::default()
        },
        response: Some(Response {
            status: 200,
            status_text: "OK".into(),
            headers: vars(&[("content-type", "application/json"), ("set-cookie", "s=1")]),
            body: Body::json("{ \"z\": [1.50, \"<&>\"],\n \"a\": null }"),
            duration_ms: 12,
            size: 34,
            proto: "HTTP/2.0".into(),
            timings: Some(Timings {
                dns_ms: 1,
                connect_ms: 2,
                tls_ms: 3,
                ttfb_ms: 4,
                total_ms: 12,
                reused: true,
            }),
            ..Response::default()
        }),
        captures: vars(&[("b", "2"), ("a", "1")]),
        asserts: vec![apic_assert::Result {
            expr: "status == 201".into(),
            pass: false,
            actual: "200".into(),
            expected: "201".into(),
            error: String::new(),
        }],
        errors: vec!["boom".into()],
        warnings: vec!["careful".into()],
        error: Some(info(&usage(CODE_REF, "x").into())),
        dry_run: true,
        attempts: 2,
        saved_to: "out.json".into(),
        deps: vec![Result {
            ok: true,
            request: Resolved {
                file: "a.http".into(),
                line: 1,
                method: "POST".into(),
                url: "http://h/login".into(),
                ..Resolved::default()
            },
            ..Result::default()
        }],
        skipped: "disabled".into(),
        iteration: Some(Iteration {
            index: 1,
            total: 2,
            row: vars(&[("k", "v")]),
        }),
        recorded: true,
        record_key: "get".into(),
        unrecorded: "never shown".into(),
        body_limit: 0,
        ..Result::default()
    }
}

#[test]
fn result_json_is_pinned() {
    let res = full();
    let want = r#"{"ok":false,"errors":["boom"],"warnings":["careful"],"error":{"code":"E107","title":"@ref problem","message":"x","hint":"Point # @ref at exactly one request, and break any cycle.","exit":2,"url":"https://apic.sh/errors/#e107"},"dry_run":true,"attempts":2,"saved_to":"out.json","ran_first":[{"ok":true,"request":{"file":"a.http","line":1,"method":"POST","url":"http://h/login","headers":{}}}],"skipped":"disabled","request":{"name":"get","file":"a.http","line":3,"method":"GET","url":"https://h/x?a=1&b","auth":"bearer","http_version":"HTTP/2","tls":{"insecure":true},"headers":{"Accept":"a&b","Cookie":"***","X-B":"<2>"},"body":"b "},"captures":{"a":"1","b":"2"},"response":{"status":200,"status_text":"OK","headers":{"content-type":"application/json","set-cookie":"***"},"body":{"z":[1.50,"<&>"],"a":null},"duration_ms":12,"size":34,"proto":"HTTP/2.0","timings":{"dns_ms":1,"connect_ms":2,"tls_ms":3,"ttfb_ms":4,"total_ms":12,"reused":true}},"asserts":[{"expr":"status == 201","pass":false,"actual":"200","expected":"201"}],"iteration":{"index":1,"total":2,"row":{"k":"v"}}}"#;
    let want = esc(want);
    assert_eq!(res.to_json(), want);
    // Serialising gives the same text, on its own and inside another
    // document.
    assert_eq!(serde_json::to_string(&res).unwrap(), want);
    assert_eq!(
        serde_json::to_value(&res).unwrap(),
        serde_json::from_str::<serde_json::Value>(&want).unwrap()
    );
    assert_eq!(
        serde_json::to_string(&vec![&res]).unwrap(),
        format!("[{want}]")
    );

    let mut red = res.clone();
    red.redact = true;
    let want = r#"{"ok":false,"errors":["boom"],"warnings":["careful"],"error":{"code":"E107","title":"@ref problem","message":"x","hint":"Point # @ref at exactly one request, and break any cycle.","exit":2,"url":"https://apic.sh/errors/#e107"},"dry_run":true,"attempts":2,"saved_to":"out.json","ran_first":[{"ok":true,"request":{"file":"a.http","line":1,"method":"POST","url":"http://h/login","headers":{}}}],"skipped":"disabled","request":{"name":"get","file":"a.http","line":3,"method":"GET","url":"https://h/x?a=***&b","auth":"bearer","http_version":"HTTP/2","tls":{"insecure":true},"headers":{"Accept":"***","Cookie":"***","X-B":"***"},"body":"***"},"captures":{"a":"***","b":"***"},"response":{"status":200,"status_text":"OK","headers":{"content-type":"***","set-cookie":"***"},"body":"***","duration_ms":12,"size":34,"proto":"HTTP/2.0","timings":{"dns_ms":1,"connect_ms":2,"tls_ms":3,"ttfb_ms":4,"total_ms":12,"reused":true}},"asserts":[{"expr":"status == ***","pass":false,"actual":"***","expected":"***"}],"iteration":{"index":1,"total":2,"row":{"k":"***"}}}"#;
    assert_eq!(red.to_json(), esc(want));
}

#[test]
fn minimal_and_special_bodies() {
    let mut res = Result {
        ok: true,
        ..Result::default()
    };
    assert_eq!(
        res.to_json(),
        r#"{"ok":true,"request":{"file":"","line":0,"method":"","url":"","headers":{}}}"#
    );
    res.response = Some(Response::default());
    assert_eq!(
        res.to_json(),
        r#"{"ok":true,"request":{"file":"","line":0,"method":"","url":"","headers":{}},"response":{"status":0,"status_text":"","headers":{},"body":null,"duration_ms":0,"size":0}}"#
    );
    let resp = Response {
        body: Body::text("//4A"),
        body_encoding: "base64".into(),
        body_truncated: true,
        ..Response::default()
    };
    assert_eq!(
        serde_json::to_string(&resp).unwrap(),
        r#"{"status":0,"status_text":"","headers":{},"body":"//4A","body_encoding":"base64","body_truncated":true,"duration_ms":0,"size":0}"#
    );
    let back: Response = serde_json::from_str(&serde_json::to_string(&resp).unwrap()).unwrap();
    assert_eq!(back, resp);
}

#[test]
fn stored_results_read_back() {
    let res = full();
    let stored = parse_result(&res.to_json()).unwrap();
    assert!(!stored.ok && stored.attempts == 2 && stored.saved_to == "out.json");
    assert!(stored.deps.is_empty() && stored.error.is_none() && stored.warnings.is_empty());
    assert_eq!(stored.request.url, "https://h/x?a=1&b");
    let names: Vec<&str> = stored
        .request
        .headers
        .iter()
        .map(|h| h.name.as_str())
        .collect();
    assert_eq!(names, ["Accept", "Cookie", "X-B"]);
    assert_eq!(stored.request.headers[1].value, "***");
    let raw = stored.raw().unwrap();
    // The body is the document written afresh: keys sorted, numbers as
    // they were, `<`, `>` and `&` escaped.
    assert_eq!(
        String::from_utf8_lossy(&raw.body),
        esc(r#"{"a":null,"z":[1.50,"<&>"]}"#)
    );
    assert_eq!(raw.status, 200);
    assert_eq!(raw.headers.get("Content-Type"), Some("application/json"));
    assert_eq!(raw.duration, std::time::Duration::from_millis(12));
    assert_eq!(stored.asserts, res.asserts);
    assert_eq!(stored.captures, res.captures);
    // Text and binary bodies come back as their bytes.
    let text = r#"{"ok":true,"request":{"file":"a","line":1,"method":"GET","url":"u","headers":{}},"response":{"status":200,"status_text":"OK","headers":{},"body":"plain","duration_ms":1,"size":5}}"#;
    assert_eq!(parse_result(text).unwrap().raw().unwrap().body, b"plain");
    let binary = text.replace(
        r#""body":"plain""#,
        r#""body":"//4A","body_encoding":"base64""#,
    );
    assert_eq!(
        parse_result(&binary).unwrap().raw().unwrap().body,
        [0xff, 0xfe, 0x00]
    );
    assert!(parse_result("{").is_err());
    assert!(parse_result(r#"{"ok":true}"#).unwrap().raw().is_none());
}

#[test]
fn description_json_is_pinned() {
    let d = Description {
        id: "a.http#1".into(),
        file: "a.http".into(),
        line: 1,
        method: "GET".into(),
        url_template: "{{u}}".into(),
        url: "{{u}}".into(),
        ready: true,
        ..Description::default()
    };
    // A request without placeholders has no list of variables at all.
    assert_eq!(
        serde_json::to_string(&d).unwrap(),
        r#"{"id":"a.http#1","file":"a.http","line":1,"method":"GET","url_template":"{{u}}","url":"{{u}}","headers":{},"variables":null,"ready":true}"#
    );
    let back: Description = serde_json::from_str(&serde_json::to_string(&d).unwrap()).unwrap();
    assert_eq!(back, d);
    let v = VarInfo {
        name: "token".into(),
        source: "missing".into(),
        missing: true,
        captured_by: "login".into(),
        ref_runs: true,
        ..VarInfo::default()
    };
    assert_eq!(
        serde_json::to_string(&v).unwrap(),
        r#"{"name":"token","source":"missing","missing":true,"captured_by":"login","ref_runs":true}"#
    );
}
