use std::path::Path;

use serde_json::Value;

use super::{execute, join, must_read, must_write, s, tmp};

/// Returns the parsed .mcp.json (or another file).
fn read_mcp_config(path: &str) -> Value {
    let data = must_read(path);
    serde_json::from_str(&data).unwrap_or_else(|e| panic!("{path}: {e}\n{data}"))
}

/// Returns the args of the apic entry in a parsed configuration.
fn server_args(doc: &Value) -> String {
    let entry = &doc["mcpServers"]["api"];
    assert_eq!(entry["command"], "apic", "no apic entry: {doc}");
    entry["args"]
        .as_array()
        .map(|args| {
            args.iter()
                .map(|a| a.as_str().unwrap_or(""))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default()
}

fn count(v: &Value, key: &str) -> usize {
    v[key].as_array().map_or(usize::MAX, Vec::len)
}

#[test]
fn mcp_install_writes_the_project_entry() {
    let tmp = tmp();
    let dir = s(&tmp);
    let (code, out, errb) = execute(&["-C", &dir, "mcp", "install", &dir, "--no-color"]);
    assert_eq!(code, 0, "err={errb}");
    let path = join(&dir, ".mcp.json");
    assert!(
        out.contains(&format!("wrote {path}")),
        "output lacks wrote {path}:\n{out}"
    );
    assert_eq!(
        server_args(&read_mcp_config(&path)),
        "mcp",
        "just mcp when the project is the directory"
    );
    // The project below the directory the agent starts in, and an
    // environment, go into the entry as flags, the path in forward
    // slashes.
    let api = join(&dir, "api");
    std::fs::create_dir(&api).unwrap();
    let (code, out, _) = execute(&["--json", "-C", &api, "--env", "dev", "mcp", "install", &dir]);
    let got: Value =
        serde_json::from_str(&out).unwrap_or_else(|e| panic!("code={code} {e}: {out}"));
    assert_eq!(code, 0);
    assert!(
        count(&got, "written") == 1 && count(&got, "unchanged") == 0,
        "second install: {got}"
    );
    assert_eq!(
        server_args(&read_mcp_config(&path)),
        "mcp --dir api --env dev"
    );
    // The same entry again is unchanged.
    let (code, out, _) = execute(&["--json", "-C", &api, "--env", "dev", "mcp", "install", &dir]);
    let got: Value = serde_json::from_str(&out).unwrap();
    assert!(
        code == 0 && count(&got, "written") == 0 && count(&got, "unchanged") == 1,
        "third install: code={code} {got}"
    );
}

#[test]
fn mcp_install_merges_and_refuses_what_it_cannot_read() {
    let tmp = tmp();
    let dir = s(&tmp);
    let path = join(&dir, ".mcp.json");
    must_write(
        &path,
        r#"{"mcpServers": {"other": {"command": "x"}}, "note": "<kept>"}"#,
    );
    let (code, _, errb) = execute(&["-C", &dir, "mcp", "install", &dir]);
    assert_eq!(code, 0, "err={errb}");
    let doc = read_mcp_config(&path);
    assert!(
        doc["mcpServers"]["other"].is_object() && doc["note"] == "<kept>",
        "merge lost what the file held: {doc}"
    );
    assert!(
        !must_read(&path).contains(r"\u003c"),
        "HTML escaped: {}",
        must_read(&path)
    );
    server_args(&doc);

    // --to picks the file, under the directory; a subdirectory is made,
    // and an absolute path is reported as given.
    let (code, _, errb) = execute(&[
        "-C",
        &dir,
        "mcp",
        "install",
        &dir,
        "--to",
        ".cursor/mcp.json",
    ]);
    assert_eq!(code, 0, "err={errb}");
    server_args(&read_mcp_config(&join(&dir, ".cursor/mcp.json")));
    let abs = join(&dir, "tools/mcp.json");
    let (code, out, errb) = execute(&[
        "-C",
        &dir,
        "mcp",
        "install",
        &dir,
        "--to",
        &abs,
        "--no-color",
    ]);
    assert!(
        code == 0 && out.contains(&format!("wrote {abs}\n")),
        "absolute --to: code={code} err={errb} out={out}"
    );
    let (code, _, errb) = execute(&[
        "-C",
        &dir,
        "mcp",
        "install",
        &dir,
        "--to",
        "../outside.json",
    ]);
    assert!(
        code == 2 && errb.contains("E210"),
        "outside the directory: code={code} err={errb}"
    );

    // An empty file is an empty document; one that is not JSON, or whose
    // mcpServers is not an object, is reported and left as it is.
    must_write(&path, "");
    let (code, _, errb) = execute(&["-C", &dir, "mcp", "install", &dir]);
    assert_eq!(code, 0, "empty file: err={errb}");
    server_args(&read_mcp_config(&path));
    for bad in ["not json", "null", r#"{"mcpServers": ["x"]}"#] {
        must_write(&path, bad);
        let (code, _, errb) = execute(&["-C", &dir, "mcp", "install", &dir]);
        assert!(
            code == 2 && errb.contains("E210") && errb.contains("not a"),
            "{bad}: code={code} err={errb}"
        );
        assert_eq!(must_read(&path), bad, "{bad}: the file was replaced");
    }
    // A directory that does not exist is a mistyped project.
    let (code, _, errb) = execute(&["mcp", "install", &join(&dir, "missing")]);
    assert!(
        code == 2 && errb.contains("E210"),
        "missing dir: code={code} err={errb}"
    );
}

#[test]
fn init_registers_the_server_unless_told_not_to() {
    let tmp = tmp();
    let dir = s(&tmp);
    let path = join(&dir, ".mcp.json");
    let (code, out, errb) = execute(&["init", &dir, "--no-color"]);
    assert!(
        code == 0 && out.contains(&format!("wrote {path}")),
        "code={code} err={errb} out={out}"
    );
    assert_eq!(server_args(&read_mcp_config(&path)), "mcp");
    // A second init finds the entry there, so it is neither written nor
    // "kept". An entry the team has changed is kept unless --force, like
    // every other file.
    let (code, out, _) = execute(&["--json", "init", &dir]);
    assert!(
        code == 0 && !out.contains(".mcp.json"),
        "code={code} out={out}"
    );
    let (code, _, errb) = execute(&[
        "-C",
        &join(&dir, "api"),
        "--env",
        "dev",
        "mcp",
        "install",
        &dir,
    ]);
    assert_eq!(code, 0, "err={errb}");
    let (code, out, _) = execute(&["init", &dir, "--no-color"]);
    assert!(
        code == 0 && out.contains(&format!("kept  {path}")),
        "code={code} out={out}"
    );
    assert_eq!(
        server_args(&read_mcp_config(&path)),
        "mcp --dir api --env dev",
        "init replaced the entry without --force"
    );
    let (code, out, _) = execute(&["init", &dir, "--force", "--no-color"]);
    assert!(
        code == 0 && out.contains(&format!("wrote {path}")),
        "code={code} out={out}"
    );
    assert_eq!(
        server_args(&read_mcp_config(&path)),
        "mcp",
        "--force should replace the entry"
    );
    let bare = super::tmp();
    let (code, _, errb) = execute(&["init", &s(&bare), "--no-mcp"]);
    assert_eq!(code, 0, "err={errb}");
    assert!(
        !Path::new(&join(bare.path(), ".mcp.json")).exists(),
        "--no-mcp should leave .mcp.json out"
    );
}

/// Off a terminal, a plain error ends with the way to get it as JSON:
/// the reader is more likely a script or an agent than a person.
#[test]
fn error_off_terminal_points_at_json() {
    let tmp = tmp();
    let dir = s(&tmp);
    let (code, _, errb) = execute(&["-C", &dir, "run", "nope"]);
    assert!(
        code == 2 && errb.contains("\n  --json prints this error as one"),
        "code={code} err={errb}"
    );
    let (code, _, errb) = execute(&["--json", "-C", &dir, "run", "nope"]);
    assert!(
        code == 2 && !errb.contains("--json prints") && errb.starts_with(r#"{"error":"#),
        "under --json the object stands alone: code={code} err={errb}"
    );
    // Not on the one error --json cannot help with: apic ui refuses it.
    let (code, _, errb) = execute(&["-C", &dir, "ui"]);
    assert!(
        code == 2 && errb.contains("E213") && !errb.contains("--json prints"),
        "apic ui: code={code} err={errb}"
    );
}
