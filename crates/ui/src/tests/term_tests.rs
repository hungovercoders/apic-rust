use std::io::Write;
use std::sync::mpsc;
use std::time::Duration;

use apic_httpc::CancelToken;

use crate::term::{decode_key, read_keys};
use crate::viewport::{Spinner, Viewport};
use crate::Key;

#[test]
fn decode_keys() {
    let cases: [(&[u8], &str, usize); 15] = [
        (b"j", "j", 1),
        (b"\r", "enter", 1),
        (b"\n", "enter", 1),
        (b"\t", "tab", 1),
        (b"\x7f", "backspace", 1),
        (b"\x03", "ctrl+c", 1),
        (b"\x15", "ctrl+u", 1),
        (b"\x1b[A", "up", 3),
        (b"\x1b[B", "down", 3),
        (b"\x1bOC", "right", 3),
        (b"\x1b[Z", "shift+tab", 3),
        (b"\x1b[5~", "pgup", 4),
        (b"\x1b[6~", "pgdown", 4),
        (b"\x1b[H", "home", 3),
        ("é".as_bytes(), "é", 2),
    ];
    for (input, want, want_used) in cases {
        let (k, used, ok) = decode_key(input);
        assert!(
            ok && k.to_string() == want && used == want_used,
            "decode_key({input:?}) = {k},{used},{ok}; want {want},{want_used}"
        );
    }
    // An unknown CSI sequence (a mouse report) is consumed, not shown.
    let (k, used, ok) = decode_key(b"\x1b[<0;1;1M");
    assert!(!ok && used == 9, "mouse report: {k},{used},{ok}");
    // Alt and a key arrives as ESC then the key: neither a binding nor an
    // escape, which would cancel a run.
    let (k, used, ok) = decode_key(b"\x1bj");
    assert!(!ok && used == 2, "alt+j: {k},{used},{ok}");
    // ESC then a control character is still the escape key on its own.
    let (k, used, ok) = decode_key(b"\x1b\x1b");
    assert!(
        ok && used == 1 && k.to_string() == "esc",
        "esc esc: {k},{used},{ok}"
    );
    // Incomplete sequences ask for more bytes; read_keys resolves a lone ESC.
    for input in [&b"\x1b"[..], b"\x1b[", b"\x1b[5"] {
        let (_, used, ok) = decode_key(input);
        assert!(
            !ok && used == 0,
            "decode_key({input:?}) should consume nothing, got {used},{ok}"
        );
    }
    // A rune split across reads waits for the rest; bytes that are no
    // rune at all are passed over.
    let (_, used, ok) = decode_key(&"é".as_bytes()[..1]);
    assert!(!ok && used == 0, "half a rune: {used},{ok}");
    let (_, used, ok) = decode_key(b"\xff\xff\xff\xff");
    assert!(!ok && used == 1, "not a rune: {used},{ok}");
}

/// Starts `read_keys` over a pipe and returns the pipe's write end, the
/// keys it decodes, and the token that ends it.
fn reading() -> (std::io::PipeWriter, mpsc::Receiver<Key>, CancelToken) {
    let (pr, pw) = std::io::pipe().expect("pipe");
    let (tx, keys) = mpsc::channel();
    let done = CancelToken::new();
    let token = done.clone();
    std::thread::spawn(move || read_keys(pr, |k| tx.send(k).is_ok(), &token));
    (pw, keys, done)
}

#[test]
fn read_keys_splits_across_reads() {
    let (mut pw, keys, done) = reading();
    std::thread::spawn(move || {
        let _ = pw.write_all(b"ab\x1b[");
        std::thread::sleep(Duration::from_millis(10));
        let _ = pw.write_all(b"A\r");
    });
    let mut got = Vec::new();
    for _ in 0..4 {
        match keys.recv_timeout(Duration::from_secs(2)) {
            Ok(k) => got.push(k.to_string()),
            Err(_) => panic!("timed out after {got:?}"),
        }
    }
    assert_eq!(got.join(","), "a,b,up,enter");
    done.cancel();
}

#[test]
fn read_keys_resolves_lone_escape() {
    let (mut pw, keys, done) = reading();
    pw.write_all(&[0x1b]).expect("write");
    match keys.recv_timeout(Duration::from_secs(2)) {
        Ok(k) => assert_eq!(k.to_string(), "esc"),
        Err(_) => panic!("a lone escape should arrive after the grace period"),
    }
    done.cancel();
}

#[test]
fn viewport() {
    let mut v = Viewport {
        width: 10,
        height: 3,
        ..Viewport::default()
    };
    v.set_content("1\n2\n3\n4\n5\n6");
    assert_eq!(v.view(), "1\n2\n3", "top");
    v.half_page_down();
    assert_eq!(v.view(), "2\n3\n4", "after half page");
    v.scroll(50);
    assert!(
        v.view() == "4\n5\n6" && v.at_bottom(),
        "clamped to the end: {:?}",
        v.view()
    );
    v.goto_top();
    assert_eq!(v.view(), "1\n2\n3", "goto_top");
    // Short content is padded to the window height.
    v.set_content("only");
    assert_eq!(v.view(), "only\n\n", "padding");
}

#[test]
fn spinner_cycles() {
    let mut s = Spinner::new();
    let first = s.view();
    for _ in 0..s.frames.len() {
        s.tick();
    }
    assert_eq!(s.view(), first, "spinner should wrap around");
}
