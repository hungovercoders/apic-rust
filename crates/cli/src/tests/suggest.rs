use apic_runner::{Code, ErrorInfo};
use serde::Deserialize;
use tempfile::TempDir;

use super::{execute, join, s, tmp};

/// A project with a request to misspell and an environment to misspell.
fn suggest_project() -> TempDir {
    let dir = tmp();
    for (name, body) in [
        (
            "api.http",
            "# @name login\nGET http://localhost/login\n\n### \n# @name get-user\nGET http://localhost/me\n",
        ),
        ("http-client.env.json", r#"{"dev": {"a": "1"}, "staging": {"a": "2"}}"#),
    ] {
        std::fs::write(join(dir.path(), name), body).unwrap();
    }
    dir
}

#[derive(Deserialize)]
struct Wrapper {
    error: ErrorInfo,
}

pub(crate) fn json_error(errb: &str) -> ErrorInfo {
    serde_json::from_str::<Wrapper>(errb)
        .unwrap_or_else(|e| panic!("stderr is not one JSON error object: {e}\n{errb}"))
        .error
}

#[test]
fn unknown_names_suggest_the_closest() {
    let tmp = suggest_project();
    let dir = s(&tmp);
    let abs = format!("{}#1", join(tmp.path(), "apo.http"));
    // The arguments, the code, what the human message holds and the
    // suggestions of the --json object.
    let cases: Vec<(Vec<&str>, Code, &str, Vec<&str>)> = vec![
        (
            vec!["run", "get-usr"],
            Code::UnknownRequest,
            r#"no request named "get-usr"; did you mean get-user? (run "#,
            vec!["get-user"],
        ),
        (
            vec!["run", "Get_User"],
            Code::UnknownRequest,
            "did you mean get-user?",
            vec!["get-user"],
        ),
        (
            vec!["describe", "logn"],
            Code::UnknownRequest,
            "did you mean login?",
            vec!["login"],
        ),
        (
            vec!["run", "apo.http"],
            Code::UnknownRequest,
            r#"no request named "apo.http"; did you mean api.http?"#,
            vec!["api.http"],
        ),
        (
            vec!["run", "apo.http#1"],
            Code::UnknownRequest,
            r#"no .http file "apo.http" in "#,
            vec!["api.http"],
        ),
        (
            vec!["run", "apo.http#1"],
            Code::UnknownRequest,
            "; did you mean api.http?",
            vec!["api.http"],
        ),
        (
            vec!["run", "api.http#get-usr"],
            Code::UnknownRequest,
            r#"no request named "get-usr" in api.http; did you mean get-user?"#,
            vec!["get-user"],
        ),
        (
            vec!["fmt", "api.http#get-usr"],
            Code::UnknownRequest,
            r#"no request named "get-usr"; did you mean get-user?"#,
            vec!["get-user"],
        ),
        (
            vec!["run", "login", "--env", "stagin"],
            Code::Environment,
            r#"environment "stagin" not found in http-client.env.json; did you mean staging? (have: dev, staging)"#,
            vec!["staging"],
        ),
        // An absolute path is compared by its path from the root.
        (
            vec!["run", abs.as_str()],
            Code::UnknownRequest,
            "; did you mean api.http?",
            vec!["api.http"],
        ),
        // Nothing close: what there is, and no suggestions key.
        (
            vec!["run", "zzzzzz"],
            Code::UnknownRequest,
            r#"no request named "zzzzzz"; known: get-user, login (run "#,
            vec![],
        ),
        (
            vec!["run", "zzz.http#1"],
            Code::UnknownRequest,
            r#"no .http file "zzz.http" in "#,
            vec![],
        ),
        (
            vec!["run", "zzz.http#1"],
            Code::UnknownRequest,
            "; files: api.http (run ",
            vec![],
        ),
        (
            vec!["run", "api.http#zzz"],
            Code::UnknownRequest,
            r#"no request named "zzz" in api.http; it has: login, get-user"#,
            vec![],
        ),
        (
            vec!["run", "login", "--env", "zzz"],
            Code::Environment,
            r#"environment "zzz" not found in http-client.env.json (have: dev, staging)"#,
            vec![],
        ),
    ];
    for (case_args, want_code, message, suggest) in cases {
        let mut args = vec!["-C", dir.as_str(), "--no-session"];
        args.extend_from_slice(&case_args);
        let (code, _, errb) = execute(&args);
        assert!(
            code == 2 && errb.contains(message),
            "{case_args:?} human: code={code}, want the message to contain {message:?}:\n{errb}"
        );
        args.push("--json");
        let (code, _, errb) = execute(&args);
        let info = json_error(&errb);
        assert!(
            code == 2 && info.code == want_code,
            "{case_args:?} json: code={code} {}, want 2 {want_code}: {}",
            info.code,
            info.message
        );
        assert_eq!(info.suggestions, suggest, "{case_args:?} suggestions");
        if suggest.is_empty() {
            assert!(
                !errb.contains("suggestions"),
                "an error with nothing to suggest should omit the key:\n{errb}"
            );
        }
    }
}

#[test]
fn human_errors_carry_the_hint_and_code_off_a_terminal() {
    let tmp = suggest_project();
    let dir = s(&tmp);
    std::fs::write(
        join(tmp.path(), "var.http"),
        "GET http://localhost/{{token}}\n",
    )
    .unwrap();
    // A missing variable's message does not name a command, so the
    // catalogue's hint follows it, then the code line; neither is coloured
    // because the test's stderr is a buffer, not a terminal.
    let (_, _, errb) = execute(&["-C", &dir, "--no-session", "run", "var.http"]);
    let e = apic_runner::lookup(Code::MissingVariable).unwrap();
    for want in [
        "error: ".to_string(),
        format!("\n  hint: {}\n", e.hint),
        "\n  E101 missing variable · see https://apic.sh/errors/#e101\n".to_string(),
    ] {
        assert!(errb.contains(&want), "missing {want:?} in:\n{errb}");
    }
    assert!(
        !errb.contains("\x1b["),
        "escape codes off a terminal:\n{errb}"
    );
    // An unknown request's message already says to run apic list, so the
    // hint, which says the same, is left out; the code line stays.
    let (_, _, errb) = execute(&["-C", &dir, "--no-session", "run", "get-usr"]);
    assert!(
        !errb.contains("hint:")
            && errb.contains("\n  E201 unknown request · see https://apic.sh/errors/#e201\n"),
        "want the code line and no hint when the message names a command:\n{errb}"
    );
    // A message that merely mentions apic is not one that names a command.
    std::fs::write(
        join(tmp.path(), "h2.http"),
        "GET http://localhost/ HTTP/2\n",
    )
    .unwrap();
    let (_, _, errb) = execute(&["-C", &dir, "--no-session", "run", "h2.http"]);
    assert!(
        errb.contains("apic does not send") && errb.contains("\n  hint: "),
        "want the hint under a message that mentions apic without naming a command:\n{errb}"
    );
}
