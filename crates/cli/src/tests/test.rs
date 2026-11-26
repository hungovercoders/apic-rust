use std::io::Write;
use std::path::Path;

use super::{execute, join, must_read, must_write, s, tmp};
use crate::test::{real_path, LazyFile};

const PING_FEATURE: &str = "Feature: p\n  Scenario: s\n    When I run \"ping\"\n";

#[test]
fn test_command_exit_codes() {
    let tmp = tmp();
    let dir = s(&tmp);
    must_write(join(&dir, "api.http"), "### a\n# @name ping\nGET {{baseUrl}}/ping\n");
    must_write(join(&dir, "http-client.env.json"), r#"{"dev":{"baseUrl":"http://127.0.0.1:1"}}"#);
    must_write(join(&dir, "features/p.feature"), PING_FEATURE);

    let (code, _, stderr) = execute(&["test", "-C", &dir, "--env", "dev", "--no-color"]);
    assert!(
        code == 3 && stderr.contains("request failed"),
        "unreachable server should exit 3: code={code} stderr={stderr}"
    );
    let (code, _, stderr) = execute(&["test", "-C", &dir, "--env", "nope"]);
    assert!(
        code == 2 && stderr.contains(r#"environment "nope""#),
        "unknown env should exit 2: code={code} stderr={stderr}"
    );
    let empty = super::tmp();
    let (code, out, _) = execute(&["test", "--steps", "--json", "-C", &s(&empty)]);
    assert!(
        code == 0 && out.contains(r#""pattern""#),
        "--steps --json outside a project: code={code} out={out}"
    );
    // --output must never truncate a feature file, and is not created
    // when the run fails early.
    let feature = join(&dir, "features/p.feature");
    let (code, _, stderr) = execute(&["test", "-C", &dir, "--env", "dev", "--output", &feature]);
    assert!(
        code == 2 && stderr.contains("would overwrite"),
        "output onto a feature: code={code} stderr={stderr}"
    );
    assert!(must_read(&feature).contains("Feature: p"), "feature file was damaged");
    // Environment files, the project config and the session are inputs
    // too.
    for name in ["http-client.env.json", "apic.yaml", ".env", ".apic/session.json"] {
        let (code, _, stderr) = execute(&["test", "-C", &dir, "--env", "dev", "--output", &join(&dir, name)]);
        assert!(
            code == 2 && stderr.contains("would overwrite"),
            "output onto {name}: code={code} stderr={stderr}"
        );
    }
    // A request body file is an input as well.
    must_write(join(&dir, "payload.json"), r#"{"a":1}"#);
    must_write(
        join(&dir, "post.http"),
        "### b\n# @name post\nPOST {{baseUrl}}/p\nContent-Type: application/json\n\n< ./payload.json\n",
    );
    let (code, _, stderr) = execute(&[
        "test", "-C", &dir, "--env", "dev", "--output", &join(&dir, "payload.json"),
    ]);
    assert!(
        code == 2 && stderr.contains("would overwrite"),
        "output onto a body file: code={code} stderr={stderr}"
    );
    // An explicitly selected feature under a dot directory, hard-linked
    // to the output.
    must_write(
        join(&dir, ".hidden/h.feature"),
        "Feature: h\n  Scenario: s\n    When I run \"ping\"\n",
    );
    let linked = join(&dir, "linked.xml");
    if std::fs::hard_link(join(&dir, ".hidden/h.feature"), &linked).is_ok() {
        let (code, _, stderr) = execute(&[
            "test", "-C", &dir, "--env", "dev", "--output", &linked, ".hidden/h.feature",
        ]);
        assert!(
            code == 2 && stderr.contains("would overwrite"),
            "hard-linked output onto a selected feature: code={code} stderr={stderr}"
        );
        std::fs::remove_file(&linked).unwrap();
    }
    // Outside the project the name heuristics do not apply.
    let elsewhere = super::tmp();
    let outside = join(elsewhere.path(), "report.http");
    let (code, _, stderr) = execute(&["test", "-C", &dir, "--env", "dev", "--output", &outside]);
    assert!(
        code == 3 && !stderr.contains("would overwrite"),
        "output outside the project: code={code} stderr={stderr}"
    );
    assert!(Path::new(&outside).exists(), "report outside the project must be written");
    // A symlink alias of a feature is refused too.
    #[cfg(unix)]
    {
        let alias = join(&dir, "report.xml");
        if std::os::unix::fs::symlink(&feature, &alias).is_ok() {
            let (code, _, stderr) = execute(&["test", "-C", &dir, "--env", "dev", "--output", &alias]);
            assert!(
                code == 2 && stderr.contains("would overwrite"),
                "symlinked output: code={code} stderr={stderr}"
            );
            std::fs::remove_file(&alias).unwrap();
        }
    }
    let (code, _, stderr) = execute(&[
        "test", "-C", &dir, "--env", "dev", "--use-session", "--no-session",
    ]);
    assert!(
        code == 2 && stderr.contains("cannot be combined"),
        "conflicting session flags: code={code} stderr={stderr}"
    );
    // A report written to a file never contains colour escapes, whatever
    // the flags.
    let pretty = join(&dir, "pretty.txt");
    let _ = execute(&["test", "-C", &dir, "--env", "dev", "--format", "pretty", "--output", &pretty]);
    let data = must_read(&pretty);
    assert!(!data.is_empty() && !data.contains("\x1b["), "file report must be plain text: {data:?}");
    let report = join(&dir, "report.xml");
    let (code, _, _) = execute(&["test", "-C", &dir, "--env", "nope", "--output", &report]);
    assert_eq!(code, 2);
    assert!(
        !Path::new(&report).exists(),
        "report file should not be created when the run never starts"
    );
}

#[test]
fn lazy_file_reports_write_errors_on_close() {
    let dir = tmp();
    let mut lf = LazyFile::new(&join(dir.path(), "missing/report.xml"));
    assert!(lf.write(b"x").is_err(), "writing into a missing directory must fail");
    assert!(lf.close().is_err(), "close must report the earlier write failure");
    let mut ok = LazyFile::new(&join(dir.path(), "report.xml"));
    ok.write_all(b"x").unwrap();
    ok.close().unwrap();
}

#[test]
fn output_onto_request_file_outside_root_is_refused() {
    let base = tmp();
    let dir = join(base.path(), "proj");
    let api = join(base.path(), "api");
    must_write(join(&dir, "apic.yaml"), "dir: ../api\n");
    must_write(join(&dir, "http-client.env.json"), r#"{"dev":{"baseUrl":"http://127.0.0.1:1"}}"#);
    must_write(join(&api, "x.http"), "### a\n# @name ping\nGET {{baseUrl}}/ping\n");
    must_write(join(&dir, "features/p.feature"), PING_FEATURE);
    let alias = join(&dir, "report.xml");
    if std::fs::hard_link(join(&api, "x.http"), &alias).is_err() {
        return; // hard links not supported here
    }
    let (code, _, errb) = execute(&["test", "-C", &dir, "--env", "dev", "--output", &alias]);
    assert!(code == 2 && errb.contains("would overwrite"), "code={code} stderr={errb}");
    assert!(must_read(join(&api, "x.http")).contains("@name ping"), "request file was damaged");
}

#[test]
fn real_path_resolves_missing_leaves() {
    let dir = tmp();
    let real = std::fs::canonicalize(dir.path()).unwrap();
    let got = real_path(&join(dir.path(), "missing/deeper/report.xml"));
    assert_eq!(got, join(&real, "missing/deeper/report.xml"));
    assert_eq!(real_path(&s(&dir)), real.to_str().unwrap());
}

#[test]
fn output_aliasing_project_config_from_outside_is_refused() {
    let tmp = tmp();
    let dir = s(&tmp);
    must_write(join(&dir, "apic.yaml"), "env: dev\n");
    must_write(join(&dir, "api.http"), "### a\n# @name ping\nGET {{baseUrl}}/ping\n");
    must_write(join(&dir, "http-client.env.json"), r#"{"dev":{"baseUrl":"http://127.0.0.1:1"}}"#);
    must_write(join(&dir, "features/p.feature"), PING_FEATURE);
    let elsewhere = super::tmp();
    let alias = join(elsewhere.path(), "report.xml");
    if std::fs::hard_link(join(&dir, "apic.yaml"), &alias).is_err() {
        return; // hard links not supported here
    }
    let (code, _, errb) = execute(&["test", "-C", &dir, "--output", &alias]);
    assert!(code == 2 && errb.contains("would overwrite"), "code={code} stderr={errb}");
    assert!(must_read(join(&dir, "apic.yaml")).contains("env: dev"), "apic.yaml was damaged");
}
