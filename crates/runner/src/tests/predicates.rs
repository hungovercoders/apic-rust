use apic_httpd::{Request as HttpRequest, Response as HttpResponse, TestServer};

use super::*;

fn no_session() -> Options {
    Options {
        no_session: true,
        ..Options::default()
    }
}

/// A request asserts its response's shape: types, emptiness, length and a
/// JSON Schema read relative to the .http file.
#[test]
fn shape_assertions() {
    let srv = TestServer::new(|_: &HttpRequest| {
        HttpResponse::json(
            200,
            r#"{"id": 7, "name": "alice", "roles": ["admin"], "meta": {}}"#,
        )
    });
    let api = r#"
### good
# @name good
# @assert body.$.id isInteger
# @assert body.$.name isString
# @assert body.$.roles length == 1
# @assert body.$.meta isEmpty
# @assert body.$.roles not isEmpty
# @assert body.$ matchesSchema ../schemas/user.json
GET SRV/u

### bad
# @name bad
# @assert body.$.id isString
# @assert body.$ matchesSchema ../schemas/strict.json
GET SRV/u

### outside
# @name outside
# @assert body.$ matchesSchema ../../elsewhere.json
GET SRV/u
"#
    .replace("SRV", &srv.url());
    let dir = write_project(&[
        (
            "schemas/user.json",
            r#"{"type": "object", "required": ["id", "name"], "properties": {"id": {"type": "integer"}, "name": {"type": "string"}}}"#,
        ),
        (
            "schemas/strict.json",
            r#"{"type": "object", "properties": {"id": {"type": "string"}}}"#,
        ),
        ("api/users.http", &api),
    ]);
    let r = new_runner(dir.path(), no_session());
    let res = run_ok(&r, "good");
    assert!(res.ok, "good: {:?}", res.asserts);
    let res = run_ok(&r, "bad");
    assert!(
        !res.ok
            && res.asserts.len() == 2
            && res.asserts[0].actual == "number"
            && !res.asserts[1].pass
            && res.asserts[1].actual.contains("id"),
        "bad: {:?}",
        res.asserts
    );
    let res = run_ok(&r, "outside");
    assert!(
        !res.ok && res.asserts[0].error.contains("outside project root"),
        "outside: {:?}",
        res.asserts
    );
    // validate: the missing and the escaping schema, at the path's span.
    let diags = r.project.validate();
    let codes: Vec<&str> = diags.iter().map(|d| d.code.as_str()).collect();
    assert_eq!(codes, ["missing-schema-file"]);
    let d = &diags[0];
    assert!(
        d.line == 20 && d.column == 32 && d.end_column == 52,
        "span: {d:?}"
    );
}

/// validate resolves a schema path the way the runner reads it: an
/// absolute path inside the project is fine, and a symlink out of it is
/// refused by both.
#[test]
fn schema_paths_agree_with_validate() {
    let srv = TestServer::new(|_: &HttpRequest| HttpResponse::new(200).with_body(r#"{"id": 7}"#));
    let (dir, outside) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let schema = r#"{"type": "object", "required": ["id"]}"#;
    std::fs::write(dir.path().join("user.json"), schema).unwrap();
    std::fs::write(outside.path().join("user.json"), schema).unwrap();
    #[cfg(unix)]
    let linked = std::os::unix::fs::symlink(
        outside.path().join("user.json"),
        dir.path().join("linked.json"),
    )
    .is_ok();
    #[cfg(not(unix))]
    let linked = false;
    let mut text = format!(
        "### abs\n# @name abs\n# @assert body.$ matchesSchema {}\nGET {}\n",
        dir.path().join("user.json").display(),
        srv.url()
    );
    if linked {
        text.push_str(&format!(
            "\n### linked\n# @name linked\n# @assert body.$ matchesSchema linked.json\nGET {}\n",
            srv.url()
        ));
    }
    std::fs::write(dir.path().join("api.http"), text).unwrap();
    let r = new_runner(dir.path(), no_session());
    let res = run_ok(&r, "abs");
    assert!(res.ok, "abs: {:?}", res.asserts);
    let got: Vec<String> = r
        .project
        .validate()
        .iter()
        .map(|d| format!("{}: {}", d.code, d.message))
        .collect();
    if !linked {
        assert!(got.is_empty(), "validate: {got:?}");
        return;
    }
    let res = run_ok(&r, "linked");
    assert!(
        !res.ok && res.asserts[0].error.contains("outside project root"),
        "linked: {:?}",
        res.asserts
    );
    assert!(
        got.len() == 1
            && got[0].contains(
                "missing-schema-file: schema file linked.json resolves outside the project root"
            ),
        "validate: {got:?}"
    );
}
