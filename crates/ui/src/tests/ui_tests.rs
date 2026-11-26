use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use apic_gocompat::{quote, Time};
use apic_httpc::CancelToken;
use apic_output::style;
use apic_runner::{shared_session, Options, Runner};

use super::{ascii, kp, new_fixture, write, Fixture};
use crate::editor::editor_command;
use crate::ui::short_duration;
use crate::{Cmd, Config, Key, KeyType, Model, Msg};

fn args(cmd: &std::process::Command) -> String {
    cmd.get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(" ")
}

/// A model over a project of one file, with nothing to rebuild it.
fn small_model(dir: &Path, width: i32, height: i32) -> Model {
    let p = apic_project::load(dir).expect("load");
    let opts = Options {
        session: Some(shared_session(apic_session::Store::new_memory())),
        ..Options::default()
    };
    let r = Arc::new(Runner::new(p, opts).expect("runner"));
    let mut cfg = Config::new(r);
    cfg.root = dir.to_str().expect("utf-8 path").to_string();
    let mut m = Model::new(cfg);
    m.update(Msg::Size { width, height });
    m
}

#[test]
fn init_is_inert_and_list_is_grouped() {
    ascii(|| {
        let mut f = new_fixture(Options::default());
        let v = f.view();
        for want in [
            "auth.http",
            "todos.http",
            "POST",
            "login",
            "whoami",
            "list-todos",
            "1 preview",
            "env=local",
            "▸",
        ] {
            assert!(v.contains(want), "missing {want:?} in view:\n{v}");
        }
        assert!(
            !v.contains("\x1b["),
            "view should have no escape codes under the Ascii profile"
        );
        assert_eq!(f.selected_id(), "login", "first request should be selected");
    });
}

#[test]
fn too_small() {
    ascii(|| {
        let mut f = new_fixture(Options::default());
        f.size(40, 10);
        assert!(
            f.view().contains("terminal too small"),
            "got {:?}",
            f.view()
        );
    });
}

#[test]
fn navigation_skips_file_headings() {
    ascii(|| {
        let mut f = new_fixture(Options::default());
        f.press(&["j", "j", "j", "j"]);
        assert_eq!(f.selected_id(), "api-key", "after 4 downs");
        f.press(&["j"]);
        assert_eq!(
            f.selected_id(),
            "health",
            "moving past a file heading should land on the next request"
        );
        f.press(&["k"]);
        assert_eq!(f.selected_id(), "api-key", "up");
        f.press(&["G"]);
        assert_eq!(f.selected_id(), "redirect-raw", "G");
        f.press(&["g"]);
        assert_eq!(f.selected_id(), "login", "g");
    });
}

#[test]
fn filter() {
    ascii(|| {
        let mut f = new_fixture(Options::default());
        f.press(&["/", "t", "o", "d", "o", "enter"]);
        let v = f.view();
        assert!(
            !v.contains("POST    login") && v.contains("list-todos") && v.contains("/todo"),
            "filter should hide login and keep todos:\n{v}"
        );
        assert_eq!(
            f.selected_id(),
            "list-todos",
            "cursor should move to the first match"
        );
        f.press(&["esc"]);
        assert!(
            f.view().contains("POST    login"),
            "esc should clear the filter"
        );
    });
}

// The arrows move through the matches while the filter prompt is open,
// enter keeps that position, and more text puts the cursor back on the
// first match.
#[test]
fn filter_arrows_move_while_typing() {
    ascii(|| {
        let mut f = new_fixture(Options::default());
        f.press(&["/", "t", "o", "d", "o", "down"]);
        assert!(
            f.m.filtering && f.selected_id() == "list-open-todos",
            "down should move to the second match and keep the prompt open, got {} filtering={}",
            f.selected_id(),
            f.m.filtering
        );
        f.press(&["tab", "pgdown", "left", "home"]);
        assert_eq!(
            f.selected_id(),
            "list-open-todos",
            "keys the prompt has no use for should leave the cursor alone"
        );
        f.press(&["enter"]);
        assert!(
            !f.m.filtering && f.selected_id() == "list-open-todos",
            "enter should keep the cursor where the arrows left it, got {}",
            f.selected_id()
        );
        f.press(&["/", "down", "down", "s"]);
        assert_eq!(
            f.selected_id(),
            "list-todos",
            "more text should return to the first match"
        );
        f.press(&["up"]);
        assert_eq!(
            f.selected_id(),
            "list-todos",
            "up at the first match stays put"
        );
    });
}

#[test]
fn run_selected_and_busy() {
    ascii(|| {
        let mut f = new_fixture(Options::default());
        let cmd = f.press(&["enter"]);
        assert!(f.m.running() && cmd.is_some(), "enter should start a run");
        let second = f.press(&["enter"]);
        assert!(
            second.is_some() && f.m.status().contains("busy"),
            "a second enter while running should report busy, status={:?}",
            f.m.status()
        );
        f.drain(cmd);
        assert!(!f.m.running(), "run should have finished");
        let sel = f.m.selected().expect("a selection");
        let res = f.m.result(&sel).expect("a result");
        assert!(
            res.ok && res.response.as_ref().is_some_and(|r| r.status == 200),
            "login should pass: {res:?}"
        );
        let v = f.view();
        for want in [
            "200 OK",
            "access_token",
            "mock-token",
            "✓ status == 200",
            "↳ token = mock-token",
            "last: ✓ login",
        ] {
            assert!(v.contains(want), "missing {want:?} in view:\n{v}");
        }
        f.press(&["3"]);
        let v = f.view();
        assert!(
            v.contains("3 checks") && v.contains("✓ status == 200"),
            "checks tab:\n{v}"
        );
        f.press(&["4"]);
        let v = f.view();
        assert!(
            v.contains("↳ token = mock-token"),
            "session tab should show the capture:\n{v}"
        );
    });
}

#[test]
fn flow_runs_whole_file_live() {
    ascii(|| {
        let mut f = new_fixture(Options::default());
        f.press(&["j"]); // whoami
        let cmd = f.press(&["f"]);
        assert!(f.m.running(), "f should start a flow");
        // Resolve just the first step and check the list shows it before
        // the rest run.
        let first = f.step(cmd);
        let v = f.view();
        assert!(
            v.contains("✓ POST    login") && v.contains("running whoami (2/5)"),
            "after the first step the list should show login passed and whoami running:\n{v}"
        );
        f.drain(first);
        assert!(!f.m.running(), "flow should finish");
        let v = f.view();
        assert!(v.contains("last: 5 passed"), "summary missing:\n{v}");
    });
}

// Pins the headless hook the animated screenshot uses: running a file with
// f calls back once per request as it lands, then once more at the end.
#[test]
fn press_watch_yields_a_frame_per_request() {
    ascii(|| {
        let mut f = new_fixture(Options::default());
        let mut views = Vec::new();
        f.m.press_watch("f", |m| views.push(m.view()));
        // auth.http has five requests: five landings plus the final call.
        assert_eq!(views.len(), 6, "frames");
        assert!(
            views[0].contains("✓ POST    login") && !views[0].contains("last: 5 passed"),
            "first frame should show only login landed:\n{}",
            views[0]
        );
        assert!(
            views[5].contains("last: 5 passed") && !f.m.running(),
            "last frame should show the finished flow:\n{}",
            views[5]
        );
    });
}

/// The request named in the tab strip, which is what the right pane is
/// about.
fn pane_request(v: &str) -> String {
    let strip = v.split('\n').next().unwrap_or("");
    let Some(at) = strip.find("5 history") else {
        return String::new();
    };
    strip[at + "5 history".len()..]
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_string()
}

// A flow started under a filter that hides some of its requests clears the
// filter, and the cursor follows each step, so the pane and the cursor
// never name different requests.
#[test]
fn flow_keeps_pane_and_cursor_together() {
    ascii(|| {
        let mut f = new_fixture(Options::default());
        f.press(&["/"]);
        f.type_text("list-todos");
        f.press(&["enter"]);
        assert_eq!(
            f.selected_id(),
            "list-todos",
            "filter should land on list-todos"
        );
        let cmd = f.press(&["f"]);
        assert!(
            f.m.filter.is_empty() && f.m.status().contains("filter cleared"),
            "a flow over hidden requests should clear the filter, filter={:?} status={:?}",
            f.m.filter,
            f.m.status()
        );
        let got = f.selected_id();
        let pane = pane_request(&f.view());
        assert!(
            got == "list-todos" && pane == got,
            "before the first step: cursor={got} pane={pane}"
        );
        f.drain(cmd);
        let got = f.selected_id();
        let pane = pane_request(&f.view());
        assert!(
            got == "redirect-raw" && pane == got,
            "after the flow: cursor={got} pane={pane}"
        );
        // The same without a filter: a run of everything from the last row
        // moves the cursor to the first request as it starts.
        f.press(&["G"]);
        let cmd = f.press(&["a"]);
        let got = f.selected_id();
        let pane = pane_request(&f.view());
        assert!(
            got == "login" && pane == got,
            "a should start at the first request: cursor={got} pane={pane}"
        );
        f.drain(cmd);
        // A filter that shows every request of the file stays.
        f.press(&["/"]);
        f.type_text("auth");
        f.press(&["enter"]);
        let cmd = f.press(&["f"]);
        f.drain(cmd);
        assert_eq!(
            f.m.filter, "auth",
            "a filter that hides nothing in the file should stay"
        );
    });
}

#[test]
fn run_all_reports_failures() {
    ascii(|| {
        let mut f = new_fixture(Options::default());
        let cmd = f.press(&["a"]);
        f.drain(cmd);
        let v = f.view();
        assert!(
            v.contains("1 failed, 23 passed") && v.contains("✗ GET     not-found"),
            "expected the deliberate failure in the summary and list:\n{v}"
        );
    });
}

#[test]
fn stale_result_dropped() {
    ascii(|| {
        let mut f = new_fixture(Options::default());
        let cmd = f.press(&["enter"]).expect("a run");
        f.press(&["esc"]);
        assert!(
            !f.m.running() && f.m.status().contains("cancelled"),
            "esc should cancel, status={:?}",
            f.m.status()
        );
        let Some(Msg::Async(work)) = cmd() else {
            panic!("a run should be scheduled off the loop");
        };
        f.m.update(work());
        let sel = f.m.selected().expect("a selection");
        assert!(
            f.m.result(&sel).is_none(),
            "a result from a cancelled run must not be recorded"
        );
    });
}

#[test]
fn session_clear_needs_confirm() {
    ascii(|| {
        let mut f = new_fixture(Options::default());
        let cmd = f.press(&["enter"]);
        f.drain(cmd);
        f.press(&["4", "x"]);
        let v = f.view();
        assert!(
            v.contains(r#"clear the session for "local"? (y/n)"#),
            "expected a confirmation:\n{v}"
        );
        f.press(&["n"]);
        assert!(f.view().contains("↳ token"), "n should keep the session");
        f.press(&["x", "y"]);
        let v = f.view();
        assert!(
            !v.contains("↳ token") && v.contains("nothing captured yet"),
            "y should clear the session:\n{v}"
        );
    });
}

#[test]
fn headers_and_curl_toggles() {
    ascii(|| {
        let mut f = new_fixture(Options::default());
        let cmd = f.press(&["enter"]);
        f.drain(cmd);
        f.press(&["H"]);
        let v = f.view();
        assert!(
            v.contains("content-type: application/json")
                && v.contains("Content-Type: application/json"),
            "H should show request and response headers:\n{v}"
        );
        f.press(&["c"]);
        let v = f.view();
        assert!(
            v.contains("curl -sS") && v.contains("/auth/login'"),
            "c should show the curl command:\n{v}"
        );
        // c again moves on through the languages, then hides the code.
        f.press(&["c"]);
        let v = f.view();
        assert!(
            v.contains("http --ignore-stdin")
                && v.contains("httpie · 2/6")
                && v.contains("c shows powershell"),
            "c again should show HTTPie:\n{v}"
        );
        f.press(&["c", "c", "c", "c"]);
        let v = f.view();
        assert!(
            v.contains("package main") && v.contains("c hides this"),
            "the sixth c should show Go:\n{v}"
        );
        f.press(&["c"]);
        let v = f.view();
        assert!(
            !v.contains("package main") && !v.contains("curl -sS"),
            "c after the last language should hide the code"
        );
        f.press(&["c", "esc"]);
        assert!(!f.view().contains("curl -sS"), "esc should hide the code");
    });
}

fn rules(v: &str) -> (Option<usize>, Option<usize>, Option<usize>) {
    (
        v.find("── request"),
        v.find("── response"),
        v.find("── checks"),
    )
}

// The response tab shows what was sent above what came back, each under
// its own rule, so the two cannot be mistaken for one block.
#[test]
fn response_tab_divides_request_from_response() {
    ascii(|| {
        let mut f = new_fixture(Options::default());
        let v = f.view();
        assert!(
            !v.contains("── request"),
            "the preview tab has no rules:\n{v}"
        );
        let cmd = f.press(&["enter"]);
        f.drain(cmd);
        let v = f.view();
        let (Some(req), Some(resp), Some(checks)) = rules(&v) else {
            panic!("want request, response and checks rules:\n{v}");
        };
        assert!(
            req < resp && resp < checks,
            "want the rules in that order:\n{v}"
        );
        let line = v.find("/auth/login").expect("the request line");
        let status = v.find("200 OK").expect("the status");
        assert!(
            line > req && line < resp && status > resp && status < checks,
            "the request line belongs under request and the status under response:\n{v}"
        );
        // The checks tab is divided the same way.
        f.press(&["3"]);
        let v = f.view();
        let (Some(req), Some(resp), Some(checks)) = rules(&v) else {
            panic!("the checks tab wants the same three rules:\n{v}");
        };
        assert!(
            req < resp && resp < checks,
            "the checks tab wants the same three rules:\n{v}"
        );
    });
}

// The panes are parted by a divider with a blank column either side, and
// the list keeps its status codes in one column however long each request
// took.
#[test]
fn panes_have_a_gutter_and_aligned_statuses() {
    ascii(|| {
        let mut f = new_fixture(Options::default());
        let cmd = f.press(&["a"]);
        f.drain(cmd);
        let mut col: Option<usize> = None;
        let view = style::strip(&f.view());
        for line in view.split('\n') {
            let Some(at) = line.find('│') else {
                continue; // the status bar
            };
            assert!(
                line[..at].ends_with(' ') && line[at..].starts_with("│ "),
                "want a blank column either side of the divider: {line:?}"
            );
            let Some(code) = line[..at].find(" 200 ") else {
                continue;
            };
            let code = style::string_width(&line[..code]); // a column, not a byte offset
            if let Some(col) = col {
                assert_eq!(code, col, "status codes out of line in {line:?}");
            }
            col = Some(code);
        }
        assert!(col.is_some(), "no row showed a status:\n{}", f.view());
    });
}

#[test]
fn env_cycle_rebuilds_runner() {
    ascii(|| {
        let mut f = new_fixture(Options::default());
        let cmd = f.press(&["enter"]);
        f.drain(cmd);
        f.press(&["e"]);
        assert!(
            f.m.env() == "other" && *f.rebuilt.borrow() == ["other"],
            "env={} rebuilt={:?}",
            f.m.env(),
            f.rebuilt.borrow()
        );
        let v = f.view();
        assert!(
            v.contains("env=other") && !v.contains("200 OK"),
            "status bar should show the new env and results should be cleared:\n{v}"
        );
        f.press(&["e"]);
        assert_eq!(f.m.env(), "local", "cycling wraps around");
        f.press(&["E"]);
        assert!(
            f.m.env() == "other" && f.rebuilt.borrow().len() == 3,
            "E should go back one, wrapping: env={} rebuilt={:?}",
            f.m.env(),
            f.rebuilt.borrow()
        );
    });
}

const EXTRA: &str = "### Extra\n# @name extra\nGET {{baseUrl}}/status/200\n";

#[test]
fn reload_picks_up_new_file() {
    ascii(|| {
        let mut f = new_fixture(Options::default());
        f.press(&["j", "j"]);
        write(&f.dir.path().join("extra.http"), EXTRA);
        f.press(&["r"]);
        let v = f.view();
        assert!(
            v.contains("extra.http") && v.contains("project reloaded"),
            "reload should list the new file:\n{v}"
        );
        assert_eq!(
            f.selected_id(),
            "basic-auth",
            "reload should keep the cursor on the same request"
        );
    });
}

#[test]
fn editor_command_per_editor() {
    ascii(|| {
        let cmd = editor_command("vim", "auth.http", 7).expect("vim");
        assert_eq!(args(&cmd), "+7 auth.http");
        let cmd = editor_command("code --wait", "auth.http", 7).expect("code");
        assert_eq!(args(&cmd), "--wait -g auth.http:7");
        // Without --wait, VS Code returns at once and the reload sees no edit.
        let cmd = editor_command("code", "auth.http", 7).expect("code");
        assert_eq!(args(&cmd), "--wait -g auth.http:7");
        let cmd = editor_command("cursor -w", "auth.http", 7).expect("cursor");
        assert_eq!(args(&cmd), "-w -g auth.http:7");
        assert!(
            editor_command("", "x", 1).is_err(),
            "empty editor should error"
        );

        let mut f = new_fixture(Options::default());
        f.press(&["j"]); // whoami, further down auth.http
        let cmd = f.press(&["o"]).expect("o should schedule something");
        let Some(Msg::Exec(e)) = cmd() else {
            panic!("o should ask the program to run the editor");
        };
        let line = f.m.selected().expect("a selection").line;
        let want = format!("+{line} {}", f.dir.path().join("auth.http").display());
        assert_eq!(args(&e.cmd), want, "editor args");
        let done = e.done.expect("a done callback");
        assert!(
            matches!(done(None), Msg::EditorDone(None)),
            "the editor finishing should come back as an editor-done message"
        );
        let run = f.press(&["enter"]);
        let busy = f.press(&["o"]).and_then(|cmd| cmd());
        assert!(
            !matches!(busy, Some(Msg::Exec(_))) && f.m.status().contains("busy"),
            "o during a run should say busy, status={:?}",
            f.m.status()
        );
        f.drain(run);
        f.m.cfg.editor = String::new();
        f.press(&["o"]);
        assert!(
            f.m.status().contains("$EDITOR"),
            "status={:?}",
            f.m.status()
        );
    });
}

#[test]
fn quit_cancels_inflight() {
    ascii(|| {
        let blocked = CancelToken::new();
        let gate = blocked.clone();
        let srv = apic_httpd::Server::bind("127.0.0.1:0", move |_req: &apic_httpd::Request| {
            gate.wait_timeout(Duration::from_secs(30));
            apic_httpd::Response::new(200)
        })
        .expect("bind");
        let dir = tempfile::tempdir().expect("tempdir");
        write(
            &dir.path().join("slow.http"),
            &format!("### Slow\n# @name slow\nGET {}/\n", srv.url()),
        );
        let mut m = small_model(dir.path(), 100, 30);
        let cmd = m.update(Msg::Key(kp("enter"))).expect("a run");
        let cancel = m.inflight.as_ref().expect("a run in flight").cancel.clone();
        let Some(Msg::Async(work)) = cmd() else {
            panic!("a run should be scheduled off the loop");
        };
        let worker = std::thread::spawn(work);
        let quit: Cmd = m
            .update(Msg::Key(kp("q")))
            .expect("q should return a command");
        assert!(matches!(quit(), Some(Msg::Quit)), "q should quit");
        assert!(
            cancel.is_cancelled(),
            "quit should cancel the in-flight request"
        );
        let _ = worker.join().expect("the request should end");
        blocked.cancel();
    });
}

// The spinner only moves while a run is in flight, so an idle UI does not
// redraw on every tick.
#[test]
fn tick_advances_spinner_only_while_running() {
    ascii(|| {
        let mut f = new_fixture(Options::default());
        let before = f.m.spin.view();
        f.m.update(Msg::Tick);
        assert_eq!(
            f.m.spin.view(),
            before,
            "an idle tick should not move the spinner"
        );
        let cmd = f.press(&["enter"]);
        f.m.update(Msg::Tick);
        assert_ne!(
            f.m.spin.view(),
            before,
            "a tick during a run should move the spinner"
        );
        f.drain(cmd);
    });
}

// The editor coming back reloads the project, or reports why it failed.
#[test]
fn editor_done_reloads_or_reports() {
    ascii(|| {
        let mut f = new_fixture(Options::default());
        f.m.press_all("j j");
        assert_eq!(f.selected_id(), "basic-auth", "press_all should move twice");
        let cmd =
            f.m.update(Msg::EditorDone(Some("exit status 1".to_string())));
        f.drain(cmd);
        assert!(
            f.m.status() == "editor: exit status 1" && f.rebuilt.borrow().is_empty(),
            "a failed editor should say so and not reload: status={:?} rebuilt={:?}",
            f.m.status(),
            f.rebuilt.borrow()
        );
        write(&f.dir.path().join("extra.http"), EXTRA);
        let cmd = f.m.update(Msg::EditorDone(None));
        f.drain(cmd);
        let v = f.view();
        assert!(
            v.contains("extra.http")
                && f.rebuilt.borrow().len() == 1
                && f.selected_id() == "basic-auth",
            "the editor closing should reload and keep the cursor: rebuilt={:?} selected={}\n{v}",
            f.rebuilt.borrow(),
            f.selected_id()
        );
    });
}

/// The message a status schedules to clear itself.
fn clearing(cmd: Option<Cmd>) -> Msg {
    match cmd.and_then(|cmd| cmd()) {
        Some(Msg::Delay(_, msg)) => *msg,
        other => panic!("a status should schedule its own clearing, got {other:?}"),
    }
}

// A status clears when its own timer fires, not when an older one does.
#[test]
fn status_clears_only_on_its_own_timer() {
    ascii(|| {
        let mut f = new_fixture(Options::default());
        let first = f.m.update(Msg::Key(kp("e"))); // env other
        let second = f.m.update(Msg::Key(kp("e"))); // back to local, a newer status
        assert_eq!(f.m.status(), "env local");
        f.m.update(clearing(first));
        assert_eq!(
            f.m.status(),
            "env local",
            "the first status's timer must not clear the second status"
        );
        f.m.update(clearing(second));
        assert_eq!(
            f.m.status(),
            "",
            "the second status's timer should clear it"
        );
    });
}

// The session tab counts a cached token's expiry down: the model says it
// is live, and each tick redraws it against the clock.
#[test]
fn session_tab_counts_token_expiry_down() {
    ascii(|| {
        let mut f = new_fixture(Options::default());
        let start = Time::from_civil(2026, 10, 13, 12, 0, 0, 0, 0);
        f.m.now = Box::new(move || start);
        let expires = start.add_nanos(90_000_000_000).format_rfc3339_nano();
        let token = format!(
            r#"{{"access_token":"at","expires_at":{}}}"#,
            quote(&expires)
        );
        f.m.runner
            .session
            .as_ref()
            .expect("a session")
            .lock()
            .expect("the session lock")
            .set("local", [("$oauth2:abc", token)]);
        f.m.refresh_session();
        assert!(!f.m.live(), "the preview tab has no clock");
        f.press(&["4"]);
        assert!(f.m.live(), "the session tab with a cached token is live");
        let v = f.view();
        assert!(
            v.contains("token, expires in 1m30s"),
            "expected the expiry at the start:\n{v}"
        );
        let later = start.add_nanos(30_000_000_000);
        f.m.now = Box::new(move || later);
        f.m.update(Msg::Tick);
        let v = f.view();
        assert!(
            v.contains("token, expires in 1m0s"),
            "a tick should redraw the countdown:\n{v}"
        );
        f.press(&["?"]);
        assert!(!f.m.live(), "the help overlay covers the clock");
    });
}

// The method column widens for a long method, so OPTIONS does not push its
// id out of line, and stays at its usual width otherwise.
#[test]
fn method_column_fits_the_longest_method() {
    ascii(|| {
        let mut f = new_fixture(Options::default());
        // The demo project has a GRAPHQL request, so its column is seven wide.
        let v = f.view();
        assert!(
            v.contains("POST    login") && v.contains("GET     whoami"),
            "the column should fit GRAPHQL:\n{v}"
        );
        f.press(&["/"]);
        f.type_text("graphql");
        f.press(&["enter"]);
        let v = f.view();
        assert!(
            v.contains(&format!("GRAPHQL {}", f.selected_id())),
            "GRAPHQL should sit in the same column as the rest:\n{v}"
        );
        // A project with nothing longer than DELETE keeps the usual six.
        let dir = tempfile::tempdir().expect("tempdir");
        write(
            &dir.path().join("a.http"),
            "### a\n# @name a\nGET http://127.0.0.1:1/\n\n### b\n# @name b\nDELETE http://127.0.0.1:1/\n",
        );
        let mut m = small_model(dir.path(), 100, 30);
        let v = m.view();
        assert!(
            v.contains("GET    a") && v.contains("DELETE b"),
            "the usual column is six wide:\n{v}"
        );
    });
}

fn top(v: &str) -> String {
    v.split('\n').next().unwrap_or("").to_string()
}

// A list taller than its pane says how many rows are above and below.
#[test]
fn list_counts_hidden_rows() {
    ascii(|| {
        let mut f = new_fixture(Options::default());
        f.size(80, 20); // 18 rows for the requests and their file headings
        let rows = f.m.visible().len();
        assert!(
            rows > 18,
            "the demo project should overflow 18 rows, has {rows}"
        );
        let hidden = rows - 18;
        let line = top(&f.view());
        assert!(
            line.contains(&format!("↓{hidden}")) && !line.contains('↑'),
            "at the top, {hidden} rows are below: {line:?}"
        );
        f.press(&["G"]);
        let line = top(&f.view());
        assert!(
            line.contains(&format!("↑{hidden}")) && !line.contains('↓'),
            "at the bottom, {hidden} rows are above: {line:?}"
        );
        f.size(80, 40);
        let line = top(&f.view());
        assert!(
            !line.contains('↑') && !line.contains('↓'),
            "everything fits, so nothing is hidden: {line:?}"
        );
        f.press(&["/"]);
        let line = top(&f.view());
        assert!(
            !line.contains('↑') && !line.contains('↓'),
            "the filter prompt is not decorated: {line:?}"
        );
    });
}

#[test]
fn help_overlay() {
    ascii(|| {
        let mut f = new_fixture(Options::default());
        f.press(&["?"]);
        let v = f.view();
        assert!(
            v.contains("run selected") && v.contains("apic is epic"),
            "help missing:\n{v}"
        );
        f.press(&["esc"]);
        assert!(!f.view().contains("run selected"), "esc should close help");
    });
}

#[test]
fn redact_masks_values() {
    ascii(|| {
        let mut f = new_fixture(Options {
            redact: true,
            ..Options::default()
        });
        let cmd = f.press(&["enter"]);
        f.drain(cmd);
        f.press(&["H"]);
        let v = f.view();
        assert!(
            !v.contains("s3cret") && v.contains("***"),
            "redact should mask the request body:\n{v}"
        );
        f.press(&["4"]);
        let v = f.view();
        assert!(
            !v.contains("mock-token"),
            "redact should mask session values:\n{v}"
        );
    });
}

#[test]
fn preview_shows_missing_variable_hint() {
    ascii(|| {
        let mut f = new_fixture(Options::default());
        f.press(&["j"]); // whoami needs {{token}}, and # @ref login supplies it
        let v = f.view();
        assert!(
            !v.contains("not ready") && v.contains("captured by login, which # @ref runs first"),
            "preview should say login runs first:\n{v}"
        );
        assert!(
            v.contains("● GET     whoami"),
            "list should mark whoami as ready:\n{v}"
        );
        f.press(&["/"]);
        f.type_text("get-job");
        f.press(&["enter"]); // get-job needs {{jobId}} from create-job, and nothing runs that for it
        let v = f.view();
        assert!(
            v.contains("not ready") && v.contains("captured by create-job"),
            "preview should explain the missing jobId:\n{v}"
        );
        assert!(
            v.contains("○ GET     get-job"),
            "list should mark get-job as not ready:\n{v}"
        );
    });
}

#[test]
fn run_stores_what_ref_ran_first() {
    ascii(|| {
        let mut f = new_fixture(Options::default());
        f.press(&["j"]); // whoami
        let cmd = f.press(&["enter"]);
        f.drain(cmd);
        let sel = f.m.selected().expect("a selection");
        let res = f.m.result(&sel).expect("a result");
        assert!(
            res.ok && res.deps.len() == 1,
            "whoami should pass after login ran first: {res:?}"
        );
        let v = f.view();
        for want in ["✓ POST    login", "✓ GET     whoami", "last: ✓ whoami"] {
            assert!(v.contains(want), "missing {want:?} in view:\n{v}");
        }
    });
}

#[test]
fn pane_scroll_survives_redraw() {
    ascii(|| {
        let mut f = new_fixture(Options::default());
        f.size(100, 24);
        let cmd = f.press(&["enter"]);
        f.drain(cmd);
        f.press(&["1"]); // the preview is taller than the pane
        f.view();
        assert!(
            f.m.vp.lines.len() as i32 > f.m.vp.height,
            "the preview should overflow a {}-line pane, it has {} lines",
            f.m.vp.height,
            f.m.vp.lines.len()
        );
        f.m.update(Msg::Key(Key::new(KeyType::CtrlD)));
        let want = f.m.vp.offset;
        assert_ne!(want, 0, "ctrl+d should scroll the pane down");
        f.view();
        assert_eq!(f.m.vp.offset, want, "redrawing reset the scroll offset");
        f.press(&["ctrl+u"]);
        assert_eq!(
            f.m.vp.offset, 0,
            "ctrl+u should scroll back up the half page"
        );
        f.press(&["pgdown", "pgup"]);
        assert_eq!(
            f.m.vp.offset, 0,
            "pgdown then pgup should land back at the top"
        );
        f.m.update(Msg::Key(Key::new(KeyType::CtrlD)));
        f.press(&["K"]);
        assert_eq!(f.m.vp.offset, want - 1, "K should scroll up a line");
        f.press(&["J"]);
        assert_eq!(f.m.vp.offset, want, "J should scroll down a line");
        f.press(&["2"]); // another tab is another subject: back to the top
        f.view();
        assert_eq!(
            f.m.vp.offset, 0,
            "switching tab should scroll back to the top"
        );
    });
}

#[test]
fn scroll_position_shows_where_the_pane_is() {
    ascii(|| {
        let mut f = new_fixture(Options::default());
        f.size(100, 24);
        let cmd = f.press(&["enter"]);
        f.drain(cmd);
        f.press(&["1"]);
        let v = f.view();
        assert!(
            v.contains("top ↓"),
            "a pane with more below should say so:\n{v}"
        );
        for _ in 0..20 {
            f.m.update(Msg::Key(Key::new(KeyType::CtrlD)));
        }
        let v = f.view();
        assert!(
            v.contains("↑ end"),
            "a pane scrolled to the bottom should say so:\n{v}"
        );
        f.size(100, 90); // now everything fits
        let v = f.view();
        assert!(
            !v.contains("top ↓") && !v.contains("↑ end"),
            "content that fits needs no scroll position:\n{v}"
        );
    });
}

#[test]
fn rows_carry_the_outcome_and_files_roll_up() {
    ascii(|| {
        let mut f = new_fixture(Options::default());
        f.size(120, 30);
        let v = f.view();
        assert!(
            v.contains("auth.http") && v.contains("todos.http"),
            "the list should be grouped by file:\n{v}"
        );
        let cmd = f.press(&["f"]); // run auth.http as a flow
        f.drain(cmd);
        let v = f.view();
        assert!(
            v.contains("200 "),
            "a request that ran should show its status:\n{v}"
        );
        assert!(
            v.contains("✓5"),
            "auth.http should roll up five passes:\n{v}"
        );
        assert!(
            v.contains("last: 5 passed"),
            "status bar should summarise the flow:\n{v}"
        );
    });
}

#[test]
fn status_bar_badges() {
    ascii(|| {
        let mut f = new_fixture(Options {
            redact: true,
            ..Options::default()
        });
        f.m.cfg.demo = true;
        let v = f.view();
        assert!(
            v.contains("demo") && v.contains("redact"),
            "status bar should badge demo and redact:\n{v}"
        );
    });
}

#[test]
fn short_durations() {
    for (ms, want) in [
        (120, "120ms"),
        (999, "999ms"),
        (1500, "1.5s"),
        (12_000, "12.0s"),
    ] {
        assert_eq!(short_duration(Duration::from_millis(ms)), want, "{ms} ms");
    }
}

// A # @disabled request is dimmed, kept out of a run of everything, and
// still sent by enter.
#[test]
fn disabled_stays_out_of_flows() {
    ascii(|| {
        let mut f = new_fixture(Options::default());
        let extra = "### up\n# @name up\n# @assert status == 200\nGET {{baseUrl}}/health\n\n### off\n# @name off\n# @disabled\nGET {{baseUrl}}/health\n";
        write(&f.dir.path().join("zz.http"), extra);
        let cmd = f.press(&["r"]);
        f.drain(cmd);
        let cmd = f.press(&["a"]);
        f.drain(cmd);
        let v = f.view();
        assert!(
            v.contains("1 failed, 24 passed")
                && v.contains("✓ GET     up")
                && v.contains("- GET     off"),
            "the run of everything should skip off:\n{v}"
        );
        f.press(&["G"]);
        let cmd = f.press(&["enter"]);
        f.drain(cmd);
        let sel = f.m.selected().expect("a selection");
        let res = f.m.result(&sel);
        assert!(
            sel.name == "off" && res.is_some_and(|r| r.ok),
            "enter on off should send it: {} {res:?}",
            sel.name
        );
    });
}

#[test]
fn history_tab_lists_runs_and_what_changed() {
    ascii(|| {
        let mut f = new_fixture(Options::default());
        // The demo project keeps a history; this starts from one that does
        // not, to see the tab say so.
        let cfg = f.dir.path().join("apic.yaml");
        let data = std::fs::read_to_string(&cfg).expect("read apic.yaml");
        let data = data
            .split('\n')
            .filter(|line| !line.contains("history"))
            .collect::<Vec<_>>()
            .join("\n");
        write(&cfg, &data);
        f.press(&["r"]);
        f.press(&["5"]);
        let v = f.view();
        assert!(
            v.contains("5 history") && v.contains("history is off"),
            "history tab before history is on:\n{v}"
        );
        write(&cfg, &format!("{data}\nhistory: 3\n"));
        f.press(&["r"]);
        let v = f.view();
        assert!(
            v.contains("keeps the last 3") && v.contains("nothing recorded yet"),
            "history tab after a reload:\n{v}"
        );
        let cmd = f.press(&["enter"]);
        f.drain(cmd);
        f.press(&["5"]);
        let v = f.view();
        assert!(
            v.contains("#1") && v.contains("200 OK") && v.contains("run it again"),
            "history tab after one run:\n{v}"
        );
        let cmd = f.press(&["enter"]);
        f.drain(cmd);
        f.press(&["5"]);
        let v = f.view();
        for want in ["#2", "what changed · #2 → #1"] {
            assert!(v.contains(want), "missing {want:?} after two runs:\n{v}");
        }
        // The demo API hands out the same token every time, so two logins
        // return the same body.
        assert!(
            v.contains("no changes in status or body"),
            "two identical logins should not differ:\n{v}"
        );
        f.press(&["e"]); // the other environment has no history of its own
        f.press(&["5"]);
        let v = f.view();
        assert!(
            v.contains("history · other") && v.contains("nothing recorded yet"),
            "history is per environment:\n{v}"
        );
    });
}
