use std::path::Path;

use super::{execute, join, must_read, must_write, s, tmp};
use crate::ui::start_demo;

#[test]
fn ui_refuses_json_and_non_tty() {
    let tmp = tmp();
    let dir = s(&tmp);
    must_write(join(&dir, "api.http"), "GET http://example.com\n");
    let (code, _, stderr) = execute(&["ui", "--json", "-C", &dir]);
    assert!(
        code == 2 && stderr.contains("--json"),
        "code={code} stderr={stderr}"
    );
    let (code, _, stderr) = execute(&["ui", "-C", &dir]);
    assert!(
        code == 2 && stderr.contains("interactive terminal"),
        "code={code} stderr={stderr}"
    );
}

#[test]
fn start_demo_serves_the_example_project() {
    let demo = start_demo().unwrap_or_else(|e| panic!("start the demo: {e:?}"));
    let root = demo.root().to_string();
    let p = apic_project::load(&root).unwrap();
    assert_eq!(p.files.len(), 4, "demo project should have 4 files");
    let env = must_read(join(&root, "http-client.env.json"));
    let url = env
        .split("\"baseUrl\": \"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .unwrap()
        .trim()
        .to_string();
    let resp = apic_httpc::Client::new()
        .send(&apic_httpc::Request::get(&format!("{url}/todos")))
        .unwrap();
    assert_eq!(resp.status_code, 401, "demo API should answer");
    demo.stop();
    assert!(
        !Path::new(&root).exists(),
        "stop should remove the temp project"
    );
    let addr = url
        .trim_start_matches("http://")
        .replace("localhost", "127.0.0.1");
    assert!(
        std::net::TcpStream::connect(&addr).is_err(),
        "stop should close the listener"
    );
}

#[test]
fn init_scaffolds_a_valid_project() {
    let tmp = tmp();
    let dir = s(&tmp);
    let (code, _, errb) = execute(&[
        "init",
        &dir,
        "--base-url",
        "http://localhost:1",
        "--env",
        "test",
    ]);
    assert_eq!(code, 0, "stderr={errb}");
    for f in [
        "apic.yaml",
        "http-client.env.json",
        "http-client.private.env.json",
        "api.http",
        "features/smoke.feature",
        ".gitignore",
    ] {
        assert!(Path::new(&join(&dir, f)).exists(), "{f} not written");
    }
    let (code, out, _) = execute(&["validate", "-C", &dir, "--json"]);
    assert!(
        code == 0 && out.contains(r#""ok": true"#),
        "scaffold should validate: code={code} out={out}"
    );
    let (code, out, _) = execute(&["init", &dir, "--json"]);
    assert!(
        code == 0 && out.contains(r#""skipped""#),
        "second init should skip existing files: code={code} out={out}"
    );
    let (code, _, _) = execute(&["init", &dir, "--env", ""]);
    assert_eq!(code, 2, "empty env should be a usage error");
}

#[test]
fn version_json_and_list_filter() {
    let (code, out, _) = execute(&["version", "--json"]);
    assert!(
        code == 0 && out.contains(r#""version""#) && out.contains(r#""os""#),
        "code={code} out={out}"
    );
    let tmp = tmp();
    let dir = s(&tmp);
    must_write(
        join(&dir, "a.http"),
        "### One\n# @name alpha\nGET http://example.com/a\n\n### Two\n# @name beta\nGET http://example.com/b\n",
    );
    let (code, out, _) = execute(&["list", "bet", "-C", &dir]);
    assert!(
        code == 0 && !out.contains("alpha") && out.contains("beta"),
        "code={code} out={out}"
    );
    let (code, out, _) = execute(&["list", "zzz", "-C", &dir, "--json"]);
    assert!(
        code == 0 && out.contains(r#""requests": []"#),
        "code={code} out={out}"
    );
    let (code, out, _) = execute(&["curl", "alpha", "-C", &dir, "--json"]);
    assert!(
        code == 0 && out.contains(r#""command": "curl"#),
        "code={code} out={out}"
    );
}
