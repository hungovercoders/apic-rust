use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::OwnedFd;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::input_tests::{read_async, wait_read};
use super::program_tests::empty_model;
use crate::input::{new_key_source, ReadError};
use crate::program::{ExecMsg, Mailbox, Program, ALT_SCREEN_ON, CLEAR_SCREEN, HIDE_CURSOR};
use crate::Msg;

/// Stands in for the terminal: the read end is what the UI and the editor
/// read, the write end is the keyboard.
pub(crate) fn terminal_pipe() -> (Arc<File>, File) {
    let (pr, pw) = std::io::pipe().expect("pipe");
    (
        Arc::new(File::from(OwnedFd::from(pr))),
        File::from(OwnedFd::from(pw)),
    )
}

fn want_key(mail: &Mailbox, want: &str) {
    match mail.key(Duration::from_secs(2)) {
        Some(k) => assert_eq!(k.to_string(), want, "the key that arrived"),
        None => panic!("no key arrived, want {want:?}"),
    }
}

// The poll source returns from a pending read on cancel without touching
// the terminal, and reads again after resume.
#[test]
fn poll_source_cancel_leaves_input_unread() {
    let (pr, mut pw) = terminal_pipe();
    let src = new_key_source(pr.clone()).expect("a key source");
    let pending = read_async(&src);
    src.cancel();
    assert_eq!(
        wait_read(&pending),
        Err(ReadError::Cancelled),
        "cancel should end the pending read"
    );
    src.cancel(); // a second cancel must not poison the next read
    pw.write_all(b"hello").expect("write");
    // Nothing is reading for the UI now, so the bytes are still in the
    // terminal for whoever reads it next.
    let mut buf = [0u8; 8];
    let n = (&*pr).read(&mut buf).expect("read");
    assert_eq!(&buf[..n], b"hello", "the input should still be there");
    src.resume();
    let after = read_async(&src);
    pw.write_all(b"k").expect("write");
    assert_eq!(wait_read(&after), Ok("k".to_string()), "after resume");
}

// stop_keys leaves the terminal with no read outstanding, and start_keys
// picks keys up again.
#[test]
fn stop_keys_leaves_the_terminal_alone() {
    let (pr, mut pw) = terminal_pipe();
    let (_dir, mut m) = empty_model();
    let mut out = Vec::new();
    let src = new_key_source(pr.clone()).expect("a key source");
    let mut p = Program::new(&mut m, pr.clone(), &mut out, src);
    p.start_keys();
    pw.write_all(b"j").expect("write");
    want_key(&p.mail, "j");
    p.stop_keys();
    pw.write_all(b"hello").expect("write");
    let mut buf = [0u8; 8];
    let n = (&*pr).read(&mut buf).expect("read");
    assert_eq!(
        &buf[..n],
        b"hello",
        "typed after stop_keys, the bytes should still be in the terminal"
    );
    p.start_keys();
    pw.write_all(b"k").expect("write");
    want_key(&p.mail, "k");
    p.stop_keys();
    p.done.cancel();
}

/// The program's output: when the editor says it is ready, it types a
/// line at the terminal, so the line can only arrive once exec has stopped
/// the key reader.
struct ReadyWriter {
    buf: Arc<Mutex<Vec<u8>>>,
    keyboard: File,
    typed: bool,
}

impl Write for ReadyWriter {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        let mut buf = self.buf.lock().expect("the buffer");
        buf.extend_from_slice(b);
        if !self.typed && String::from_utf8_lossy(&buf).contains("ready") {
            self.typed = true;
            let _ = self.keyboard.write_all(b"hello\n");
        }
        Ok(b.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

// exec hands the terminal to the editor: every byte typed while it runs
// reaches the editor, none reaches the UI, and keys work again afterwards.
#[test]
fn exec_leaves_the_terminal_to_the_editor() {
    let (pr, mut pw) = terminal_pipe();
    let (_dir, mut m) = empty_model();
    let screen = Arc::new(Mutex::new(Vec::new()));
    let mut out = ReadyWriter {
        buf: screen.clone(),
        keyboard: pw.try_clone().expect("clone the keyboard"),
        typed: false,
    };
    let text = || String::from_utf8_lossy(&screen.lock().expect("the buffer")).into_owned();
    let src = new_key_source(pr.clone()).expect("a key source");
    let mut p = Program::new(&mut m, pr, &mut out, src);
    p.capture_child = true;
    p.start_keys();
    pw.write_all(b"j").expect("write");
    want_key(&p.mail, "j");

    let mut editor = Command::new("sh");
    editor.args([
        "-c",
        r#"printf ready; IFS= read -r line; printf 'editor got %s' "$line""#,
    ]);
    p.exec(ExecMsg {
        cmd: editor,
        done: Some(Box::new(Msg::EditorDone)),
    });
    match p.mail.msg(Duration::from_secs(2)) {
        Some(Msg::EditorDone(None)) => {}
        other => panic!("exec should report the editor finishing, got {other:?}"),
    }
    assert!(
        text().contains("editor got hello"),
        "the editor should have read the line typed while it ran:\n{}",
        text()
    );
    if let Some(k) = p.mail.key(Duration::ZERO) {
        panic!("the UI should not have seen a key while the editor ran, got {k}");
    }
    pw.write_all(b"q").expect("write");
    want_key(&p.mail, "q");
    assert!(
        text().ends_with(&format!("{ALT_SCREEN_ON}{HIDE_CURSOR}{CLEAR_SCREEN}")),
        "exec should put the UI back on the alternate screen:\n{:?}",
        text()
    );
    p.stop_keys();
    p.done.cancel();
}
