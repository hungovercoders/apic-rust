use std::time::{Duration, Instant};

use apic_httpc::CancelToken;
use serde_json::Value;
use tempfile::TempDir;

use super::{execute, free_port, join, tmp};
use crate::{App, Buffer};

fn write_mock_spec() -> (TempDir, String) {
    let dir = tmp();
    let spec = join(dir.path(), "todos.yaml");
    std::fs::write(
        &spec,
        r#"
openapi: 3.0.3
info: {title: Todos, version: "1"}
paths:
  /todos:
    get:
      responses:
        "200":
          description: ok
          content:
            application/json:
              example: [{id: "1", title: "Buy milk", done: false}]
    post:
      responses:
        "201": {description: created}
"#,
    )
    .unwrap();
    (dir, spec)
}

#[test]
fn mock_rejects_zero_port() {
    let (_dir, spec) = write_mock_spec();
    let (code, _, stderr) = execute(&["mock", &spec, "--port", "0"]);
    assert_eq!(code, 2, "stderr={stderr}");
    assert!(
        stderr.contains("--port must be between 1 and 65535"),
        "unexpected stderr: {stderr}"
    );
}

#[test]
fn mock_missing_file_is_e210() {
    let dir = tmp();
    let (code, _, stderr) = execute(&["mock", &join(dir.path(), "missing.yaml")]);
    assert_eq!(code, 2, "stderr={stderr}");
    assert!(stderr.contains("E210"), "unexpected stderr: {stderr}");
}

/// The command prints its startup line, serves the document until its
/// token is cancelled, and exits 0.
#[test]
fn mock_serves_until_cancelled() {
    let (_dir, spec) = write_mock_spec();
    let port = free_port();
    let (stdout, stderr) = (Buffer::new(), Buffer::new());
    let cancel = CancelToken::new();
    let done = {
        let (stdout, stderr, cancel, spec) =
            (stdout.clone(), stderr.clone(), cancel.clone(), spec.clone());
        std::thread::spawn(move || {
            let mut app = App::new();
            app.stdout = stdout.output();
            app.stderr = stderr.output();
            app.execute(
                &cancel,
                &["mock", &spec, "--port", &port.to_string(), "--json"],
            )
        })
    };

    let url = format!("http://127.0.0.1:{port}");
    let client = apic_httpc::Client::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    let res = loop {
        match client.send(&apic_httpc::Request::get(&format!("{url}/todos"))) {
            Ok(res) => break res,
            Err(e) if Instant::now() > deadline => {
                cancel.cancel();
                panic!("mock did not come up: {e} (stderr {})", stderr.string());
            }
            Err(_) => std::thread::sleep(Duration::from_millis(50)),
        }
    };
    let body = String::from_utf8_lossy(&res.body).into_owned();
    assert!(
        res.status_code == 200 && body.contains(r#""title": "Buy milk""#),
        "GET /todos = {} {body}",
        res.status_code
    );
    cancel.cancel();
    let code = done.join().expect("the command thread");
    assert_eq!(code, 0, "stderr {}", stderr.string());

    let startup: Value = serde_json::from_str(&stdout.string())
        .unwrap_or_else(|e| panic!("stdout {:?}: {e}", stdout.string()));
    let routes = startup["routes"].as_array().unwrap();
    assert!(
        startup["spec"] == spec.as_str()
            && startup["url"] == format!("http://localhost:{port}")
            && startup["listening"] == true
            && routes.len() == 2
            && routes[1]["status"] == 201,
        "{startup}"
    );
}
