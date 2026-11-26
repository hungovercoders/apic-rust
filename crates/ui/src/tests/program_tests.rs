use std::fs::File;
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use apic_httpc::CancelToken;
use apic_runner::{shared_session, Options, Runner};
use tempfile::TempDir;

use super::{ascii, kp, new_fixture};
use crate::input::new_key_source;
use crate::program::{
    after, async_cmd, batch, guard, quit, Program, ALT_SCREEN_OFF, ALT_SCREEN_ON, CLEAR_SCREEN,
    CURSOR_HOME, ERASE_BELOW, ERASE_LINE, HIDE_CURSOR, SHOW_CURSOR,
};
use crate::{Cmd, Config, Model, Msg};

/// The program's screen: what it has written so far.
#[derive(Clone, Default)]
pub(crate) struct Screen(Arc<Mutex<Vec<u8>>>);

impl Screen {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().expect("the screen")).into_owned()
    }

    fn reset(&self) {
        self.0.lock().expect("the screen").clear();
    }
}

impl Write for Screen {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("the screen").extend_from_slice(b);
        Ok(b.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// A pipe standing in for the terminal: the read end is the program's
/// input, the write end its keyboard.
fn terminal() -> (Arc<File>, File) {
    let (pr, pw) = std::io::pipe().expect("pipe");
    #[cfg(unix)]
    {
        use std::os::fd::OwnedFd;
        (
            Arc::new(File::from(OwnedFd::from(pr))),
            File::from(OwnedFd::from(pw)),
        )
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::OwnedHandle;
        (
            Arc::new(File::from(OwnedHandle::from(pr))),
            File::from(OwnedHandle::from(pw)),
        )
    }
}

/// Builds a program around `m` with a pipe standing in for the terminal:
/// the returned file is its keyboard, the screen its output. The loop,
/// render, dispatch and resize all work without a terminal.
fn program_over<'a>(m: &'a mut Model, screen: &'a mut Screen) -> (Program<'a>, File) {
    let (pr, pw) = terminal();
    let src = new_key_source(pr.clone()).expect("a key source");
    (Program::new(m, pr, screen, src), pw)
}

/// A model with an empty project behind it.
pub(crate) fn empty_model() -> (TempDir, Model) {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = apic_project::load(dir.path()).expect("load");
    let opts = Options {
        session: Some(shared_session(apic_session::Store::new_memory())),
        ..Options::default()
    };
    let r = Arc::new(Runner::new(p, opts).expect("runner"));
    let mut cfg = Config::new(r);
    cfg.root = dir.path().to_str().expect("utf-8 path").to_string();
    (dir, Model::new(cfg))
}

fn cmd(f: impl FnOnce() -> Option<Msg> + Send + 'static) -> Option<Cmd> {
    Some(Box::new(f))
}

// render paints every line with an erase to its end and the rows below
// erased, so a shorter frame leaves no debris, and paints nothing at all
// when the frame has not changed.
#[test]
fn render_erases_and_skips_unchanged_frames() {
    ascii(|| {
        let mut f = new_fixture(Options::default());
        let mut screen = Screen::default();
        let out = screen.clone();
        let (mut p, _keyboard) = program_over(&mut f.m, &mut screen);
        p.m.update(Msg::Size {
            width: 80,
            height: 20,
        });
        p.render();
        let painted = out.text();
        assert!(
            painted.starts_with(CURSOR_HOME)
                && painted.ends_with(&format!("{ERASE_LINE}{ERASE_BELOW}")),
            "a frame starts at the top and ends by erasing below:\n{painted:?}"
        );
        let body = painted
            .strip_prefix(CURSOR_HOME)
            .and_then(|s| s.strip_suffix(ERASE_BELOW))
            .expect("the frame");
        let rows: Vec<&str> = body.split("\r\n").collect();
        assert_eq!(rows.len(), 20, "a 20-row terminal gets 20 rows");
        for (i, row) in rows.iter().enumerate() {
            assert!(
                row.ends_with(ERASE_LINE),
                "row {} is not erased to its end: {row:?}",
                i + 1
            );
        }
        out.reset();
        p.render();
        assert!(
            out.text().is_empty(),
            "an unchanged frame should paint nothing, got {:?}",
            out.text()
        );
        p.m.update(Msg::Key(kp("j")));
        p.render();
        assert!(!out.text().is_empty(), "a moved cursor is a new frame");
    });
}

// resize falls back to 80x24 when the terminal cannot say its size, and
// reports a change only once.
#[test]
fn resize_falls_back_and_reports_changes() {
    ascii(|| {
        let mut f = new_fixture(Options::default());
        let mut screen = Screen::default();
        let (mut p, _keyboard) = program_over(&mut f.m, &mut screen);
        assert!(
            p.resize() && p.m.width == 80 && p.m.height == 24,
            "a pipe has no size, so the first resize should report 80x24, got {}x{}",
            p.m.width,
            p.m.height
        );
        assert!(!p.resize(), "the same size again is not a change");
    });
}

// dispatch routes each kind of command the model returns.
#[test]
fn dispatch_routes_commands() {
    ascii(|| {
        let mut f = new_fixture(Options::default());
        let mut screen = Screen::default();
        let (mut p, _keyboard) = program_over(&mut f.m, &mut screen);
        assert!(
            !p.dispatch(None) && !p.dispatch(cmd(|| None)),
            "nothing to do is not a quit"
        );
        assert!(p.dispatch(Some(Box::new(quit))), "quit ends the loop");
        assert!(
            p.dispatch(cmd(|| Some(Msg::Cmd(Box::new(quit))))),
            "a command that returns a command is followed"
        );
        assert!(
            p.dispatch(Some(batch(vec![Box::new(|| None), Box::new(quit)]))),
            "a batch runs each command, and a quit inside it quits"
        );
        assert!(
            !p.dispatch(Some(async_cmd(|| Msg::ClearStatus(7)))),
            "async work does not quit"
        );
        match p.mail.msg(Duration::from_secs(2)) {
            Some(Msg::ClearStatus(7)) => {}
            other => panic!("async: got {other:?}"),
        }
        assert!(
            !p.dispatch(Some(after(Duration::from_millis(10), Msg::ClearStatus(8)))),
            "a delay does not quit"
        );
        if let Some(got) = p.mail.msg(Duration::ZERO) {
            panic!("a delayed message arrived at once: {got:?}");
        }
        match p.mail.msg(Duration::from_secs(2)) {
            Some(Msg::ClearStatus(8)) => {}
            other => panic!("delay: got {other:?}"),
        }
        // A command that returns a plain message hands it to the model.
        assert!(
            !p.dispatch(cmd(|| Some(Msg::Size {
                width: 90,
                height: 30
            }))),
            "a plain message does not quit"
        );
        assert!(
            p.m.width == 90 && p.m.height == 30,
            "the model should have seen the message, size is {}x{}",
            p.m.width,
            p.m.height
        );
        p.done.cancel();
    });
}

// A delayed message is dropped when the program ends before it is due.
#[test]
fn delayed_message_dies_with_the_program() {
    ascii(|| {
        let mut f = new_fixture(Options::default());
        let mut screen = Screen::default();
        let (mut p, _keyboard) = program_over(&mut f.m, &mut screen);
        p.dispatch(Some(after(Duration::from_millis(20), Msg::ClearStatus(1))));
        p.done.cancel();
        if let Some(got) = p.mail.msg(Duration::from_millis(100)) {
            panic!("got {got:?} after the program ended");
        }
    });
}

/// Cancels the token it returns after five seconds, so a loop that should
/// have ended on its own fails its test rather than hanging it.
fn watchdog() -> CancelToken {
    let token = CancelToken::new();
    let t = token.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(5));
        t.cancel();
    });
    token
}

// The loop enters the alternate screen, quits on q from the terminal, and
// returns when it is cancelled.
#[test]
fn loop_quits_on_key_and_on_cancel() {
    ascii(|| {
        let mut f = new_fixture(Options::default());
        let mut screen = Screen::default();
        let out = screen.clone();
        let (mut p, mut keyboard) = program_over(&mut f.m, &mut screen);
        keyboard.write_all(b"q").expect("write");
        let cancel = watchdog();
        p.run_loop(&cancel);
        assert!(!cancel.is_cancelled(), "q should end the loop");
        p.stop_keys();
        assert!(
            out.text()
                .starts_with(&format!("{ALT_SCREEN_ON}{HIDE_CURSOR}{CLEAR_SCREEN}")),
            "the loop should open the alternate screen first:\n{:?}",
            out.text()
        );
        assert!(
            out.text().contains("auth.http"),
            "the loop should have painted the project"
        );
        drop(p);

        let mut f2 = new_fixture(Options::default());
        let mut screen2 = Screen::default();
        let (mut p2, _keyboard2) = program_over(&mut f2.m, &mut screen2);
        let cancel = CancelToken::new();
        let t = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            t.cancel();
        });
        let started = std::time::Instant::now();
        p2.run_loop(&cancel);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "cancelling should end the loop"
        );
        p2.stop_keys();
    });
}

// A panic on the request thread reaches the loop as a message, which
// raises it again only once the terminal has been put back, so the crash
// prints on a usable terminal and says where the worker panicked.
#[test]
fn worker_panic_restores_the_terminal() {
    ascii(|| {
        let (_dir, mut m) = empty_model();
        let mut screen = Screen::default();
        let out = screen.clone();
        let (mut p, _keyboard) = program_over(&mut m, &mut screen);
        assert!(
            !p.dispatch(Some(async_cmd(|| panic!("boom")))),
            "async work should not quit"
        );
        let cancel = watchdog();
        // What run does: the loop under guard, then the terminal put back.
        let outcome = guard(|| p.run_loop(&cancel));
        p.restore();
        p.stop_keys();
        let Err(pm) = outcome else {
            panic!("the loop should re-raise the worker's panic");
        };
        let text = pm.to_string();
        assert!(
            text.contains("boom") && text.contains("panicked at") && text.contains("program_tests"),
            "the panic should carry the value and where the worker raised it:\n{text}"
        );
        assert!(
            out.text()
                .ends_with(&format!("{SHOW_CURSOR}{ALT_SCREEN_OFF}")),
            "the terminal should be restored before the panic:\n{:?}",
            out.text()
        );
    });
}
