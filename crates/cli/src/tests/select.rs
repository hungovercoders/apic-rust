use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Arc;

use apic_httpd::{Response, TestServer};
use serde_json::Value;
use tempfile::TempDir;

use super::{execute, join, must_write, server, tmp};
use crate::run::parse_size;

/// Serves a JSON object that changes on every call and keeps a history
/// of two responses.
fn select_project(history: &str) -> (TempDir, Arc<AtomicI32>, TestServer) {
    let hits = Arc::new(AtomicI32::new(0));
    let count = hits.clone();
    let srv = server(move |_| {
        let n = count.fetch_add(1, Ordering::SeqCst) + 1;
        Response::json(
            200,
            format!(
                r#"{{"call": {n}, "items": [{{"id": "a"}}, {{"id": "b"}}], "big": "{}"}}"#,
                "x".repeat(3000)
            ),
        )
        .with_header("Set-Cookie", "sid=secret")
    });
    let dir = tmp();
    must_write(join(dir.path(), "apic.yaml"), &format!("env: dev\n{history}"));
    must_write(join(dir.path(), "api.http"), "### thing\n# @name thing\nGET {{baseUrl}}/thing\n");
    must_write(
        join(dir.path(), "http-client.env.json"),
        &format!(r#"{{"dev":{{"baseUrl":"{}"}}}}"#, srv.url()),
    );
    (dir, hits, srv)
}

/// select reads the recorded response, so looking again costs no call.
#[test]
fn select_reads_the_last_response_without_sending() {
    let (tmp, hits, _srv) = select_project("history: 2\n");
    let dir = tmp.path().to_str().unwrap();
    for _ in 0..2 {
        let (code, _, errb) = execute(&["-C", dir, "run", "thing", "--json"]);
        assert_eq!(code, 0, "run: err={errb}");
    }
    let (code, out, errb) = execute(&["-C", dir, "select", "thing", "body.$.items[1].id"]);
    assert!(code == 0 && out == "b\n", "code={code} out={out:?} err={errb}");
    let (code, out, _) = execute(&["-C", dir, "select", "thing", "body.$.items", "--no-color"]);
    assert!(
        code == 0 && out.starts_with("[\n  {\n"),
        "an array pretty-printed: code={code} out={out:?}"
    );
    let (code, out, _) = execute(&["-C", dir, "--json", "select", "thing", "body.$.call", "--entry", "2"]);
    let got: Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("entry 2 as json: {e} {out}"));
    assert!(
        code == 0 && got["entry"] == 2 && got["found"] == true && got["value"] == 1 && got["selector"] == "body.$.call",
        "entry 2 as json: code={code} {out}"
    );
    let (code, out, _) = execute(&["-C", dir, "select", "thing", "status"]);
    assert!(code == 0 && out == "200\n", "status: code={code} out={out:?}");
    let (code, out, _) = execute(&["-C", dir, "select", "thing", "header.content-type"]);
    assert!(code == 0 && out == "application/json\n", "header: code={code} out={out:?}");
    // Nothing at the path is exit 1, like a failed capture; a bad
    // selector or entry is a flag error.
    let (code, out, errb) = execute(&["-C", dir, "--json", "select", "thing", "body.$.nope"]);
    assert!(
        code == 1 && out.contains(r#""found": false"#),
        "missing path: code={code} out={out} err={errb}"
    );
    let (code, _, errb) = execute(&["-C", dir, "select", "thing", "nope"]);
    assert!(code == 2 && errb.contains("selector"), "bad selector: code={code} err={errb}");
    let (code, _, errb) = execute(&["-C", dir, "select", "thing", "status", "--entry", "3"]);
    assert_eq!(code, 2, "entry past the history: err={errb}");
    let (code, _, errb) = execute(&["-C", dir, "select", "nothing", "status"]);
    assert!(code == 2 && errb.contains("nothing"), "unknown request: code={code} err={errb}");
    assert_eq!(hits.load(Ordering::SeqCst), 2, "select sent a request");
}

/// With history off the error says how to switch it on.
#[test]
fn select_needs_history() {
    let (tmp, _, _srv) = select_project("");
    let dir = tmp.path().to_str().unwrap();
    let (code, _, errb) = execute(&["-C", dir, "run", "thing"]);
    assert_eq!(code, 0, "run: err={errb}");
    let (code, _, errb) = execute(&["-C", dir, "select", "thing", "status"]);
    assert!(
        code == 2 && errb.contains("history: 20") && errb.contains("no recorded response"),
        "code={code} err={errb}"
    );
}

/// --body-limit bounds what the displays show, marks it, and leaves the
/// history whole for select to read.
#[test]
fn run_body_limit() {
    let (tmp, _, _srv) = select_project("history: 1\n");
    let dir = tmp.path().to_str().unwrap();
    let (code, out, errb) = execute(&["-C", dir, "--json", "run", "thing", "--body-limit", "1k"]);
    assert_eq!(code, 0, "err={errb}");
    let obj: Value = serde_json::from_str(&out).unwrap();
    let body = obj["response"]["body"].as_str().unwrap_or("");
    assert!(
        obj["response"]["body_truncated"] == true
            && body.len() == 1024
            && obj["response"]["size"].as_i64().unwrap_or(0) > 1024
            && body.starts_with(r#"{"call": "#),
        "truncated: {}",
        &out[..200]
    );
    let (code, out, _) = execute(&["-C", dir, "run", "thing", "--body-limit", "100", "--body-only"]);
    assert!(code == 0 && out.len() == 101, "body-only: code={code} len={}", out.len());
    let (code, out, _) = execute(&["-C", dir, "run", "thing", "--body-limit", "100", "--no-color"]);
    assert!(
        code == 0 && out.contains("100 B of 3.") && out.contains("apic select thing body.$.<path>"),
        "human: code={code}\n{out}"
    );
    // The whole body is in the history.
    let (code, out, _) = execute(&["-C", dir, "select", "thing", "body.$.big"]);
    assert!(code == 0 && out.len() == 3001, "select after a limited run: code={code} len={}", out.len());
    // A body within the limit is untouched, and a bad size is a flag
    // error.
    let (code, out, _) = execute(&["-C", dir, "--json", "run", "thing", "--body-limit", "1m"]);
    assert!(code == 0 && !out.contains("body_truncated"), "within limit: code={code}");
    let (code, _, errb) = execute(&["-C", dir, "run", "thing", "--body-limit", "lots"]);
    assert!(code == 2 && errb.contains("--body-limit"), "bad size: code={code} err={errb}");
}

#[test]
fn parse_size_reads_suffixes() {
    for (input, want) in [
        ("", 0),
        ("512", 512),
        ("4k", 4096),
        ("4K", 4096),
        ("4kb", 4096),
        ("1m", 1 << 20),
        ("2MB", 2 << 20),
    ] {
        assert_eq!(parse_size(input), Ok(want), "{input:?}");
    }
    for bad in ["0", "-1", "k", "4g", "lots", "9223372036854775807k", "99999999999999999999"] {
        assert!(parse_size(bad).is_err(), "{bad:?} should be refused");
    }
}

/// A history entry keeps one value per header, so the count and index
/// forms are refused with a reason rather than answering wrongly.
#[test]
fn select_refuses_multi_value_header_selectors() {
    let (tmp, _, _srv) = select_project("history: 1\n");
    let dir = tmp.path().to_str().unwrap();
    let (code, _, errb) = execute(&["-C", dir, "run", "thing"]);
    assert_eq!(code, 0, "run: err={errb}");
    for sel in ["header.content-type.#", "header.content-type[0]", "headers.x[-1]"] {
        let (code, _, errb) = execute(&["-C", dir, "select", "thing", sel]);
        assert!(
            code == 2 && errb.contains("one value per header"),
            "{sel}: code={code} err={errb}"
        );
    }
}

/// The truncation note offers select only when the history took the
/// response; a binary body keeps its raw prefix under the limit.
#[test]
fn body_limit_note_and_binary_prefix() {
    let srv = server(|r| {
        if r.path == "/bin" {
            let mut body = vec![0xff, 0xfe, 0x00, 0x01];
            body.extend_from_slice("z".repeat(300).as_bytes());
            return Response::new(200)
                .with_header("Content-Type", "application/octet-stream")
                .with_body(body);
        }
        Response::json(200, format!(r#"{{"big": "{}"}}"#, "x".repeat(500)))
    });
    let tmp = tmp();
    let dir = tmp.path().to_str().unwrap();
    must_write(
        join(dir, "api.http"),
        "### thing\n# @name thing\nGET {{baseUrl}}/thing\n\n### bin\n# @name bin\nGET {{baseUrl}}/bin\n\n### anon\nGET {{baseUrl}}/thing\n",
    );
    must_write(
        join(dir, "http-client.env.json"),
        &format!(r#"{{"dev":{{"baseUrl":"{}"}}}}"#, srv.url()),
    );
    // No history: the note says what to set.
    let (code, out, _) = execute(&[
        "-C", dir, "--env", "dev", "run", "thing", "--body-limit", "100", "--no-color",
    ]);
    assert!(
        code == 0 && out.contains("the rest is not kept: set history: N") && !out.contains("reads the rest"),
        "without history: code={code}\n{out}"
    );
    must_write(join(dir, "apic.yaml"), "env: dev\nhistory: 2\n");
    let (code, out, _) = execute(&["-C", dir, "run", "thing", "--body-limit", "100", "--no-color"]);
    assert!(
        code == 0 && out.contains("apic select thing body.$.<path> reads the rest"),
        "with history: code={code}\n{out}"
    );
    // An unnamed request is never recorded.
    let (code, out, _) = execute(&["-C", dir, "run", "api.http#3", "--body-limit", "100", "--no-color"]);
    assert!(code == 0 && !out.contains("reads the rest"), "unnamed: code={code}\n{out}");
    // Bytes that are not text: the first 4 as they are, not rune-trimmed
    // to nothing; the JSON carries their base64.
    let mut a = crate::App::new();
    let (bytes, errs) = (crate::Buffer::new(), crate::Buffer::new());
    a.stdout = bytes.output();
    a.stderr = errs.output();
    let code = a.execute(
        &apic_httpc::CancelToken::new(),
        &["-C", dir, "run", "bin", "--body-limit", "4", "--body-only"],
    );
    assert!(
        code == 0 && bytes.bytes() == b"\xff\xfe\x00\x01\n",
        "binary body-only: code={code} out={:?}",
        bytes.bytes()
    );
    let (code, out, _) = execute(&["-C", dir, "--json", "run", "bin", "--body-limit", "4"]);
    assert!(
        code == 0
            && out.contains(r#""body":"//4AAQ==""#)
            && out.contains(r#""body_encoding":"base64""#)
            && out.contains(r#""body_truncated":true"#),
        "binary json: code={code} {out}"
    );
}

/// An entry past a history that has some is a bad argument, not "history
/// is off"; a redacted entry is refused with its reason; a data run says
/// why nothing was kept.
#[test]
fn select_errors_say_why() {
    let (tmp, _, _srv) = select_project("history: 3\n");
    let dir = tmp.path().to_str().unwrap();
    for _ in 0..2 {
        let (code, _, errb) = execute(&["-C", dir, "run", "thing"]);
        assert_eq!(code, 0, "run: err={errb}");
    }
    // History switched off afterwards: the two entries stay readable, and
    // a third is out of range, not missing history.
    must_write(join(dir, "apic.yaml"), "env: dev\n");
    let (code, out, _) = execute(&["-C", dir, "select", "thing", "status", "--entry", "2"]);
    assert!(code == 0 && out == "200\n", "entry 2 with history off: code={code} out={out:?}");
    let (code, _, errb) = execute(&["-C", dir, "select", "thing", "status", "--entry", "3"]);
    assert!(
        code == 2 && !errb.contains("history is off") && errb.contains("no #3"),
        "entry 3: code={code} err={errb}"
    );
    must_write(join(dir, "apic.yaml"), "env: dev\nhistory: 3\n");
    let (code, _, errb) = execute(&["-C", dir, "run", "thing", "--redact"]);
    assert_eq!(code, 0, "redacted run: err={errb}");
    let (code, _, errb) = execute(&["-C", dir, "select", "thing", "body.$.call"]);
    assert!(code == 2 && errb.contains("--redact run"), "redacted entry: code={code} err={errb}");
    let rows = join(dir, "rows.csv");
    must_write(&rows, "n\n1\n");
    let (code, out, errb) = execute(&[
        "-C", dir, "run", "thing", "--data", &rows, "--body-limit", "100", "--no-color",
    ]);
    assert!(
        code == 0 && out.contains("a data run keeps no history"),
        "data run note: code={code} err={errb}\n{out}"
    );
}
