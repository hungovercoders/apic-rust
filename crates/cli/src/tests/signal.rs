use std::path::Path;
use std::time::{Duration, Instant};

use apic_httpc::CancelToken;
use apic_runner::Code;

use super::suggest::json_error;
use super::{cancel_after, execute, execute_with, free_port, join, s, tmp, HangServer};

/// An interrupt cancels the token the commands run under, instead of
/// ending the process; after it, handling is back to the default.
#[cfg(unix)]
#[test]
fn interrupt_cancels_the_context() {
    let guard = crate::signal_context();
    crate::signal::interrupt_self();
    assert!(
        guard.token().wait_timeout(Duration::from_secs(5)),
        "the interrupt did not cancel the token"
    );
}

/// The demo server stops when its token is cancelled (Ctrl-C), and exits
/// 0: being told to stop is not an error.
#[test]
fn demo_stops_when_cancelled() {
    let dir = tmp();
    let port = free_port();
    let cancel = CancelToken::new();
    {
        let cancel = cancel.clone();
        std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline {
                if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            cancel.cancel();
        });
    }
    let (code, out, errb) = execute_with(
        &cancel,
        None,
        &["demo", "--out", &s(&dir), "--port", &port.to_string()],
    );
    assert!(
        code == 0 && out.contains("demo api listening on") && errb.is_empty(),
        "code={code}\nstdout:\n{out}\nstderr:\n{errb}"
    );
}

/// A port already in use is E214, the code whose catalogue entry names
/// this command, not the uncoded E200. It fails before anything is
/// written or announced: no startup object or "listening" line on stdout,
/// and no project on disk.
#[test]
fn demo_port_in_use_is_a_server_error() {
    let ln = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = ln.local_addr().unwrap().port().to_string();
    let tmp = tmp();
    let dir = join(tmp.path(), "apic-demo");
    let (code, out, errb) = execute(&["--json", "demo", "--out", &dir, "--port", &port]);
    let got = json_error(&errb);
    assert!(
        code == 2 && got.code == Code::Server,
        "code={code} {}: {}",
        got.code,
        got.message
    );
    assert!(
        out.is_empty(),
        "stdout should be empty when the port is taken:\n{out}"
    );
    assert!(
        !Path::new(&dir).exists(),
        "the project should not be written when the port is taken"
    );
    let (code, out, _) = execute(&["demo", "--out", &dir, "--port", &port]);
    assert!(
        code == 2 && !out.contains("listening"),
        "text mode: code={code} stdout:\n{out}"
    );
}

/// --keep-going carries a flow past a failed request, not past Ctrl-C:
/// once the token is cancelled, the remaining requests and data rows are
/// not attempted, so the output holds what completed plus the one
/// interrupted, not a cancellation per remaining request.
#[test]
fn cancellation_stops_a_keep_going_flow() {
    let srv = HangServer::new();
    let url = srv.url();
    let tmp = tmp();
    let dir = s(&tmp);
    std::fs::write(
        join(&dir, "a.http"),
        format!("# @name one\nGET {url}/{{{{row}}}}\n\n###\n# @name two\nGET {url}/\n\n###\n# @name three\nGET {url}/\n"),
    )
    .unwrap();
    let rows = join(&dir, "rows.csv");
    std::fs::write(&rows, "row\nx\ny\nz\n").unwrap();
    let cases: [&[&str]; 2] = [
        &["run", "a.http", "--keep-going", "--json", "--var", "row=x"],
        &["run", "a.http", "--keep-going", "--json", "--data", &rows],
    ];
    for args in cases {
        let cancel = CancelToken::new();
        cancel_after(&cancel, Duration::from_millis(100));
        let all = [&["-C", dir.as_str(), "--no-session"], args].concat();
        let (code, out, errb) = execute_with(&cancel, None, &all);
        let lines: Vec<&str> = out.trim().split('\n').collect();
        assert!(
            code == 3 && lines.len() == 1 && lines[0].contains(r#""E305""#),
            "{args:?}: code={code}, {} result lines, want 3 and one E305 result:\n{out}\n{errb}",
            lines.len()
        );
        assert!(
            errb.contains(r#""code":"E305""#),
            "stderr should end with the E305 object:\n{errb}"
        );
    }
}
