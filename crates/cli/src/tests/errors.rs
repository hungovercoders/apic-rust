use std::collections::BTreeMap;
use std::time::Duration;

use apic_httpc::CancelToken;
use apic_httpd::{Response, TestServer};
use apic_runner::Code;

use super::suggest::json_error;
use super::{cancel_after, execute_with, free_port, join, s, server, tmp, HangServer};

/// What a case starts before it runs; the URL replaces {{url}} in its
/// files.
enum Setup {
    None,
    /// A server that never answers.
    Hang,
    /// A server that answers with a hundred bytes.
    Ok,
    /// A server that hangs up without answering: not a refused
    /// connection, not a timeout, just a failed request.
    HangUp,
    /// An address nothing listens on.
    Closed,
    /// An https server with a certificate nothing trusts.
    Tls,
}

/// Makes apic fail with one catalogue code: files for a project, then the
/// arguments (--json is added).
struct ErrorCase {
    files: Vec<(&'static str, &'static str)>,
    args: Vec<&'static str>,
    setup: Setup,
    /// When set, the command is cancelled after this long.
    cancel: Option<Duration>,
}

fn case(files: &[(&'static str, &'static str)], args: &[&'static str]) -> ErrorCase {
    ErrorCase {
        files: files.to_vec(),
        args: args.to_vec(),
        setup: Setup::None,
        cancel: None,
    }
}

fn get(args: &[&'static str], setup: Setup) -> ErrorCase {
    ErrorCase {
        setup,
        ..case(&[("a.http", "GET {{url}}/\n")], args)
    }
}

fn error_cases() -> BTreeMap<&'static str, ErrorCase> {
    let mut cases = BTreeMap::new();
    let mut add = |code: Code, c: ErrorCase| {
        cases.insert(code.as_str(), c);
    };
    add(
        Code::MissingVariable,
        case(
            &[("a.http", "GET http://localhost/{{token}}\n")],
            &["run", "a.http"],
        ),
    );
    add(
        Code::Build,
        case(
            &[("a.http", "GET http://localhost/{{$randomInt 5 1}}\n")],
            &["run", "a.http"],
        ),
    );
    add(
        Code::InvalidFile,
        case(
            &[("a.http", "# @capture nonsense\nGET http://localhost/\n")],
            &["run", "a.http"],
        ),
    );
    add(
        Code::Directive,
        case(
            &[("a.http", "# @timeout soon\nGET http://localhost/\n")],
            &["run", "a.http"],
        ),
    );
    add(
        Code::Auth,
        case(
            &[("a.http", "# @auth nonsense\nGET http://localhost/\n")],
            &["run", "a.http"],
        ),
    );
    add(
        Code::BodyFile,
        case(
            &[("a.http", "POST http://localhost/\n\n< missing.json\n")],
            &["run", "a.http"],
        ),
    );
    add(
        Code::Ref,
        case(
            &[("a.http", "# @ref nope\nGET http://localhost/{{token}}\n")],
            &["run", "a.http"],
        ),
    );
    add(Code::UnknownRequest, get(&["run", "nope"], Setup::None));
    add(
        Code::Ambiguous,
        case(
            &[(
                "a.http",
                "GET http://localhost/1\n\n###\nGET http://localhost/2\n",
            )],
            &["describe", "a.http"],
        ),
    );
    add(Code::Flag, get(&["run", "--bogus"], Setup::None));
    add(
        Code::Environment,
        get(&["run", "a.http", "--env", "nope"], Setup::None),
    );
    add(
        Code::Project,
        case(
            &[
                ("apic.yaml", "timeout: [\n"),
                ("a.http", "GET http://localhost/\n"),
            ],
            &["run", "a.http"],
        ),
    );
    add(
        Code::TlsConfig,
        case(
            &[
                ("ca.pem", "not a certificate\n"),
                ("a.http", "GET https://localhost/\n"),
            ],
            &["run", "a.http", "--cacert", "{{dir}}/ca.pem"],
        ),
    );
    add(
        Code::Proxy,
        get(
            &[
                "run",
                "a.http",
                "--var",
                "url=http://localhost",
                "--proxy",
                "ftp://proxy",
            ],
            Setup::None,
        ),
    );
    add(
        Code::Session,
        get(&["session", "--no-session"], Setup::None),
    );
    add(
        Code::Data,
        case(
            &[
                ("a.http", "GET http://localhost/{{id}}\n"),
                ("rows.csv", ""),
            ],
            &["run", "a.http", "--data", "{{dir}}/rows.csv"],
        ),
    );
    add(
        Code::File,
        get(&["fmt", "{{dir}}/missing.http"], Setup::None),
    );
    add(
        Code::Import,
        case(
            &[("spec.json", "{\"not\": \"a spec\"}")],
            &["import", "{{dir}}/spec.json", "--out", "{{dir}}/out"],
        ),
    );
    add(Code::Features, get(&["test"], Setup::None));
    add(Code::Terminal, get(&["ui"], Setup::None));
    add(
        Code::Server,
        get(&["mcp", "--http", "256.0.0.1:1"], Setup::None),
    );
    add(
        Code::Mock,
        case(
            &[("spec.json", "{\"not\": \"a spec\"}")],
            &["mock", "{{dir}}/spec.json"],
        ),
    );
    add(Code::Transport, get(&["run", "a.http"], Setup::HangUp));
    add(Code::Connect, get(&["run", "a.http"], Setup::Closed));
    add(
        Code::Timeout,
        get(&["run", "a.http", "--timeout", "50ms"], Setup::Hang),
    );
    add(Code::Tls, get(&["run", "a.http"], Setup::Tls));
    add(
        Code::Protocol,
        ErrorCase {
            setup: Setup::Ok,
            ..case(
                &[
                    ("apic.yaml", "maxBodyBytes: 10\n"),
                    ("a.http", "GET {{url}}/\n"),
                ],
                &["run", "a.http"],
            )
        },
    );
    add(
        Code::Cancelled,
        ErrorCase {
            cancel: Some(Duration::from_millis(100)),
            ..get(&["run", "a.http"], Setup::Hang)
        },
    );
    cases
}

/// Starts what a case needs: the URL for its files, and what must stay
/// alive while the command runs.
fn start(setup: &Setup) -> (String, Option<Box<dyn std::any::Any>>) {
    match setup {
        Setup::None => ("http://localhost".to_string(), None),
        Setup::Hang => {
            let srv = HangServer::new();
            (srv.url(), Some(Box::new(srv)))
        }
        Setup::Ok => {
            let srv = server(|_| Response::text(200, "x".repeat(100)));
            (srv.url(), Some(Box::new(srv)))
        }
        Setup::HangUp => {
            let srv = server(|_| Response::hang_up());
            (srv.url(), Some(Box::new(srv)))
        }
        Setup::Closed => (format!("http://127.0.0.1:{}", free_port()), None),
        Setup::Tls => {
            let srv = TestServer::new_tls(|_: &apic_httpd::Request| Response::not_found());
            (srv.url(), Some(Box::new(srv)))
        }
    }
}

/// Runs a real command for each code in the catalogue and checks the
/// --json error object on stderr. A new code without a case here fails
/// the test.
#[test]
fn every_code_is_produced() {
    let cases = error_cases();
    for e in apic_runner::CATALOGUE {
        if e.code == Code::Other {
            // Every error apic makes itself has a code; E200 is the
            // fallback for one that slipped through.
            let got = apic_runner::code_of(&apic_runner::Error::Other("anything".to_string()));
            assert_eq!(got, Code::Other, "an uncoded error");
            continue;
        }
        let Some(c) = cases.get(e.code.as_str()) else {
            panic!(
                "{} ({}): no test produces it; add a case to error_cases",
                e.code, e.title
            );
        };
        let tmp = tmp();
        let dir = s(&tmp);
        let (url, _running) = start(&c.setup);
        for (name, body) in &c.files {
            std::fs::write(join(&dir, name), body.replace("{{url}}", &url)).unwrap();
        }
        let cancel = CancelToken::new();
        if let Some(after) = c.cancel {
            cancel_after(&cancel, after);
        }
        let mut args = vec!["-C".to_string(), dir.clone(), "--json".to_string()];
        args.extend(c.args.iter().map(|a| a.replace("{{dir}}", &dir)));
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let (exit, _, errb) = execute_with(&cancel, None, &args);
        let got = json_error(&errb);
        assert!(
            got.code == e.code && exit == e.exit && got.exit == e.exit,
            "want {} exit {}, got {} exit {exit} (object says {}): {}",
            e.code,
            e.exit,
            got.code,
            got.exit,
            got.message
        );
        assert!(
            got.title == e.title
                && got.hint == e.hint
                && got.url == e.code.url()
                && !got.message.is_empty(),
            "incomplete error object: {got:?}"
        );
    }
}
