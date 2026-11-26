use apic_httpc::CancelToken;

use super::{execute, execute_with, join, must_read, must_write, s, tmp};

/// Pins that `apic init` ignores each file apic itself treats as a
/// secret source. .env was missing, so a project scaffolded by apic would
/// happily commit it.
#[test]
fn init_gitignores_every_credential_file() {
    let tmp = tmp();
    let dir = s(&tmp);
    let (code, _, stderr) = execute(&["init", &dir]);
    assert_eq!(code, 0, "stderr={stderr}");
    let got = must_read(join(&dir, ".gitignore"));
    for want in ["http-client.private.env.json", ".env", ".apic/"] {
        assert!(
            got.contains(want),
            "generated .gitignore is missing {want:?}:\n{got}"
        );
    }
}

/// Pins the modeline that gives editors completion and validation for
/// apic.yaml, and that the file still parses as config.
#[test]
fn init_points_at_the_schema() {
    let tmp = tmp();
    let dir = s(&tmp);
    let (code, _, stderr) = execute(&["init", &dir, "--env", "qa"]);
    assert_eq!(code, 0, "stderr={stderr}");
    let data = must_read(join(&dir, "apic.yaml"));
    assert!(
        data.starts_with(
            "# yaml-language-server: $schema=https://apic.sh/schemas/apic.schema.json\n"
        ),
        "apic.yaml should start with the schema modeline:\n{data}"
    );
    let p = apic_project::load(&dir).unwrap();
    assert_eq!(p.config.env, "qa");
}

#[test]
fn import_detects_postman_collections() {
    let tmp = tmp();
    let dir = s(&tmp);
    let col = join(&dir, "c.postman_collection.json");
    must_write(
        &col,
        r#"{"info": {"name": "c", "schema": "https://schema.getpostman.com/json/collection/v2.1.0/collection.json"}, "item": [{"name": "ping", "request": "https://x/ping", "event": [{"listen": "test", "script": {"exec": ["pm.response.to.have.status(200);", "pm.foo();"]}}]}]}"#,
    );
    let env = join(&dir, "dev.postman_environment.json");
    must_write(
        &env,
        r#"{"name": "dev", "values": [{"key": "a", "value": "1", "enabled": true}], "_postman_variable_scope": "environment"}"#,
    );
    let exec = |args: &[&str]| {
        let (code, stdout, stderr) = execute(args);
        (stdout + &stderr, code)
    };
    let out_dir = join(&dir, "out");
    let (out, code) = exec(&["import", &col, "-o", &out_dir, "--postman-env", &env]);
    assert!(
        code == 0
            && out.contains(&format!("wrote {}", join(&out_dir, "c.http")))
            && out.contains("note  ping: pm.foo();:")
            && out.contains("1 request(s) generated"),
        "import:\n{out}"
    );
    let got = must_read(join(&out_dir, "apic.yaml"));
    assert!(got.contains("env: dev\n"), "apic.yaml = {got:?}");
    let (out, code) = exec(&["--json", "import", &col, "-o", &join(&dir, "out2")]);
    assert!(
        code == 0 && out.contains(r#""unsupported""#) && out.contains(r#""requests": 1"#),
        "import --json:\n{out}"
    );
    let (out, code) = exec(&["import", &env, "-o", &dir]);
    assert!(
        code == 2 && out.contains("is a Postman environment"),
        "an environment file alone: {code} {out}"
    );
    let spec = join(&dir, "openapi.yaml");
    must_write(
        &spec,
        "openapi: 3.0.0\ninfo: {title: t, version: '1'}\npaths: {}\n",
    );
    let (out, code) = exec(&["import", &spec, "--postman-env", &env, "-o", &dir]);
    assert!(
        code == 2 && out.contains("--postman-env goes with"),
        "postman env with OpenAPI: {code} {out}"
    );
}

#[test]
fn import_curl_appends_a_request() {
    let tmp = tmp();
    let dir = s(&tmp);
    must_write(
        join(&dir, "http-client.env.json"),
        r#"{"dev": {"baseUrl": "https://api.example.com"}}"#,
    );
    must_write(
        join(&dir, "todos.http"),
        "### existing\n# @name post-todos\nPOST {{baseUrl}}/todos\n",
    );
    let exec = |stdin: &str, args: &[&str]| {
        let all = [&["-C", dir.as_str(), "--env", "dev"], args].concat();
        let (code, out, err) = execute_with(&CancelToken::new(), Some(stdin), &all);
        (out, err, code)
    };
    let cmd = r#"curl -X POST https://api.example.com/todos -H "Content-Type: application/json" -d '{"title":"x"}' --bogus"#;
    let (out, err_out, code) = exec("", &["import", "--curl", cmd, "--into", "todos.http"]);
    assert!(
        code == 0
            && out == "added post-todos-2 to todos.http\n"
            && err_out.contains("note: unknown flag --bogus"),
        "code={code} out={out:?} err={err_out:?}"
    );
    let got = must_read(join(&dir, "todos.http"));
    let want = "### existing\n# @name post-todos\nPOST {{baseUrl}}/todos\n\n### POST /todos\n# @name post-todos-2\n# @assert status == 200\nPOST {{baseUrl}}/todos\nContent-Type: application/json\n\n{\"title\":\"x\"}\n";
    assert_eq!(got, want);
    // Printed without --into, read from stdin, named explicitly, as JSON.
    let (out, _, code) = exec(
        "curl https://api.example.com/health",
        &["--json", "import", "--curl", "-", "--name", "ping"],
    );
    assert!(
        code == 0
            && out.contains(r#""name": "ping""#)
            && out.contains("GET {{baseUrl}}/health")
            && out.contains(r#""warnings": []"#),
        "stdin + json: code={code} out={out}"
    );
    let (out, _, code) = exec("", &["import", "--curl", "curl -H 'a: b'"]);
    assert!(
        code == 2 && out.is_empty(),
        "no URL: code={code} out={out:?}"
    );
    let (_, err_out, code) = exec("", &["import", "openapi.yaml", "--into", "x.http"]);
    assert!(
        code == 2 && err_out.contains("--into and --name go with --curl"),
        "--into without --curl: code={code} err={err_out:?}"
    );
}

#[test]
fn import_curl_body_that_splits_the_block_goes_to_a_side_file() {
    let tmp = tmp();
    let dir = s(&tmp);
    let (code, stdout, stderr) = execute(&[
        "-C",
        &dir,
        "import",
        "--curl",
        "curl -d '### heading\ntext' https://x/notes",
        "--into",
        "notes.http",
    ]);
    assert_eq!(code, 0, "{stdout}{stderr}");
    let got = must_read(join(&dir, "notes.http"));
    assert!(
        got.contains("\n< ./post-notes.body.txt\n") && !got.contains("### heading"),
        "file:\n{got}"
    );
    assert_eq!(
        must_read(join(&dir, "post-notes.body.txt")),
        "### heading\ntext"
    );
    let p = apic_project::load(&dir).unwrap();
    assert!(
        p.requests().len() == 1 && p.validate().is_empty(),
        "project: {} {:?}",
        p.requests().len(),
        p.validate()
    );
    let (code, _, stderr) = execute(&[
        "-C",
        &dir,
        "import",
        "--curl",
        "curl -d '< 5 items' https://x/notes",
    ]);
    assert!(
        code == 0 && stderr.contains("use --into"),
        "without --into: code={code} err={stderr}"
    );
}
