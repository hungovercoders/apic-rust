use serde_json::Value;

use super::{execute, join, s, tmp};
use crate::{App, CommandSpec};

/// A directory with no request files is where a new user is most likely
/// to be standing: every command they might try there names apic init and
/// apic ui --demo.
#[test]
fn empty_directory_points_at_init_and_demo() {
    let tmp = tmp();
    let dir = s(&tmp);
    let (code, out, _) = execute(&["-C", &dir, "list"]);
    assert!(
        code == 0 && out.contains("no .http files found under") && out.contains("apic init") && out.contains("apic ui --demo"),
        "list: code={code}\n{out}"
    );
    let (code, out, _) = execute(&["-C", &dir, "list", "--json"]);
    let got: Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("list --json: code={code} err={e}\n{out}"));
    assert_eq!(code, 0);
    assert!(
        got["requests"].as_array().is_some_and(Vec::is_empty) && got["hint"].as_str().unwrap_or("").contains("apic init"),
        "list --json: {got}"
    );
    for args in [["run", "login"], ["describe", "login"], ["curl", "login"]] {
        let (code, _, errb) = execute(&[&["-C", dir.as_str(), "--no-session"], &args[..]].concat());
        assert!(
            code == 2 && errb.contains("no .http files found under") && errb.contains("apic init") && errb.contains("apic ui --demo"),
            "{args:?}: code={code}\n{errb}"
        );
    }
}

/// A project with files but no # @name keeps the message about naming a
/// request: init and demo are for an empty directory, not a bare one.
#[test]
fn unnamed_project_says_to_name_a_request() {
    let tmp = tmp();
    std::fs::write(join(tmp.path(), "a.http"), "GET http://localhost/\n").unwrap();
    let (_, _, errb) = execute(&["-C", &s(&tmp), "--no-session", "run", "login"]);
    assert!(
        errb.contains("add `# @name login` above a request line") && !errb.contains("apic init"),
        "got:\n{errb}"
    );
}

/// apic with no arguments is a first contact: the help it prints carries
/// examples, as does every command's help.
#[test]
fn every_command_has_examples() {
    let (code, out, _) = execute(&[]);
    assert!(
        code == 0 && out.contains("Examples:") && out.contains("apic ui --demo") && out.contains("apic init"),
        "apic with no args: code={code}\n{out}"
    );
    fn walk(c: &CommandSpec, path: &str) {
        if c.hidden || c.name == "help" || c.name == "completion" {
            return;
        }
        assert!(!c.example.trim().is_empty(), "{path} has no examples");
        for sub in &c.subcommands {
            walk(sub, &format!("{path} {}", sub.name));
        }
    }
    walk(App::spec(), "apic");
}
