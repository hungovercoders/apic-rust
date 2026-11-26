use apic_httpc::CancelToken;
use apic_httpd::{Response, TestServer};
use serde_json::Value;
use tempfile::TempDir;

use super::{execute, execute_with, join, must_write, server, tmp};

/// Serves /echo/<id>, answering with the id and the X-Prev header it was
/// sent, and 404 for the id "bad". The request captures the id as prev,
/// so X-Prev shows whether a capture crossed iterations.
fn data_project() -> (TempDir, TestServer) {
    let srv = server(|r| {
        let id = r.path.strip_prefix("/echo/").unwrap_or(&r.path);
        let status = if id == "bad" { 404 } else { 200 };
        let body = serde_json::json!({"id": id, "prev": r.header("X-Prev").unwrap_or("")});
        Response::json(status, format!("{body}\n"))
    });
    let dir = tmp();
    must_write(
        join(dir.path(), "api.http"),
        "### echo\n# @name echo\n# @assert status == 200\n# @capture prev = body.$.id\nGET {{baseUrl}}/echo/{{id}}\nX-Prev: {{prev}}\n",
    );
    must_write(
        join(dir.path(), "http-client.env.json"),
        &format!(r#"{{"dev":{{"baseUrl":"{}","prev":"none"}}}}"#, srv.url()),
    );
    (dir, srv)
}

pub(crate) fn json_lines(out: &str) -> Vec<Value> {
    serde_json::Deserializer::from_str(out)
        .into_iter::<Value>()
        .map(|o| o.unwrap_or_else(|e| panic!("{e} in {out}")))
        .collect()
}

#[test]
fn run_data() {
    let (tmp, _srv) = data_project();
    let dir = tmp.path().to_str().unwrap();
    let rows = join(dir, "rows.csv");
    must_write(&rows, "id,note\n1,first\n2,second\n3,third\n");
    let args = |extra: &[&str]| {
        let mut all = vec!["run", "echo", "-C", dir, "--env", "dev", "--no-color", "--data", rows.as_str()];
        all.extend_from_slice(extra);
        execute(&all)
    };

    let (code, out, errb) = args(&[]);
    assert!(
        code == 0
            && out.contains("iteration 1/3 · id=1 note=first")
            && out.contains("iteration 3/3")
            && out.contains("3 of 3 iterations: 3 passed"),
        "text: code={code} out={out} err={errb}"
    );
    // Iterations are isolated: no capture crosses, and the session file
    // is not written.
    let (code, out, _) = args(&["--json"]);
    let objs = json_lines(&out);
    assert!(code == 0 && objs.len() == 3, "json: code={code} {out}");
    for (i, o) in objs.iter().enumerate() {
        let it = &o["iteration"];
        let body = &o["response"]["body"];
        assert!(
            it["index"] == i + 1 && it["total"] == 3 && it["row"]["id"] == body["id"] && body["prev"] == "none",
            "iteration {}: {it} {body}",
            i + 1
        );
    }
    assert!(
        !std::path::Path::new(&join(dir, ".apic/session.json")).exists(),
        "isolated iterations must not write the session"
    );
    // Shared: each iteration sees the one before.
    let (code, out, _) = args(&["--json", "--data-share-session"]);
    let objs = json_lines(&out);
    assert!(code == 0 && objs[1]["response"]["body"]["prev"] == "1", "shared: {out}");
    assert!(
        std::path::Path::new(&join(dir, ".apic/session.json")).exists(),
        "a shared session is written as usual"
    );

    // A failing row stops the run, unless --keep-going.
    must_write(&rows, "id\n1\nbad\n3\n");
    let (code, out, _) = args(&[]);
    assert!(
        code == 1 && !out.contains("iteration 3/3") && out.contains("2 of 3 iterations: 1 failed, 1 passed"),
        "stop: code={code} {out}"
    );
    let (code, out, _) = args(&["--keep-going"]);
    assert!(
        code == 1 && out.contains("3 of 3 iterations: 1 failed, 2 passed"),
        "keep going: code={code} {out}"
    );

    // JSON rows from stdin; --redact masks the row.
    let (code, stdout, stderr) = execute_with(
        &CancelToken::new(),
        Some(r#"[{"id": 42}]"#),
        &["run", "echo", "-C", dir, "--env", "dev", "--data", "-", "--json", "--redact"],
    );
    assert_eq!(code, 0, "stdin: {stdout} {stderr}");
    let it = &json_lines(&stdout)[0]["iteration"];
    assert_eq!(it["row"]["id"], "***", "redacted row: {it}");

    let out_json = join(dir, "out.json");
    let (code, _, errb) = args(&["--output", &out_json]);
    assert!(code == 2 && errb.contains("--"), "--output: code={code} {errb}");
    let missing = join(dir, "missing.csv");
    let (code, _, errb) = execute(&["run", "echo", "-C", dir, "--data", &missing]);
    assert!(code == 2 && errb.contains("--"), "missing: code={code} {errb}");
}
