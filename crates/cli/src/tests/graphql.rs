use super::{execute, join, must_write, tmp};

/// list keeps the GRAPHQL method the file wrote; curl and describe show
/// the POST with the JSON envelope that is sent.
#[test]
fn graphql_commands() {
    let tmp = tmp();
    let dir = tmp.path().to_str().unwrap();
    must_write(
        join(dir, "api.http"),
        "### Todos
# @name todos
GRAPHQL {{baseUrl}}/graphql

query Todos($done: Boolean) {
  todos(done: $done) { id }
}

{\"done\": true}
",
    );
    must_write(join(dir, "http-client.env.json"), r#"{"dev":{"baseUrl":"http://api.test"}}"#);
    let (code, out, errb) = execute(&["list", "-C", dir, "--env", "dev", "--no-color"]);
    assert!(code == 0 && out.contains("GRAPHQL"), "list: code={code} out={out} err={errb}");
    let (code, out, errb) = execute(&["curl", "todos", "-C", dir, "--env", "dev"]);
    assert_eq!(code, 0, "curl: out={out} err={errb}");
    // curl infers POST from the body, so no -X is needed.
    for want in [
        "http://api.test/graphql",
        "Content-Type: application/json",
        r#"--data-raw '{"query":"query Todos($done: Boolean) {\n  todos(done: $done) { id }\n}","variables":{"done":true}}'"#,
    ] {
        assert!(out.contains(want), "curl lacks {want:?}:\n{out}");
    }
    assert!(
        !out.contains("X-REQUEST-TYPE") && !out.contains("GRAPHQL"),
        "curl carries the editor's marker:\n{out}"
    );
    let (code, out, errb) = execute(&["describe", "todos", "-C", dir, "--env", "dev", "--json"]);
    assert!(
        code == 0 && out.contains(r#""method": "POST""#) && out.contains(r#""body": "{\"query\": \"query Todos"#),
        "describe: code={code} out={out} err={errb}"
    );
}
