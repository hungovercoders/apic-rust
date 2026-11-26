//! Keys: what the UI reacts to, and decoding them from the bytes a
//! terminal sends.

use std::fmt;
use std::io::Read;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;

use apic_httpc::CancelToken;

/// Names the keys the UI reacts to. Anything printable arrives as
/// [`KeyType::Rune`] with the rune set.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum KeyType {
    #[default]
    Rune,
    Enter,
    Esc,
    Tab,
    ShiftTab,
    Backspace,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PgUp,
    PgDown,
    CtrlC,
    CtrlD,
    CtrlU,
}

/// One decoded keypress.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Key {
    pub kind: KeyType,
    pub rune: char,
}

impl Key {
    /// A key that is not a printable character.
    pub fn new(kind: KeyType) -> Key {
        Key { kind, rune: '\0' }
    }

    /// A printable keypress, for tests and bindings.
    pub fn rune(r: char) -> Key {
        Key {
            kind: KeyType::Rune,
            rune: r,
        }
    }
}

/// The name used in key bindings: "j", "enter", "ctrl+c", "up".
impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self.kind {
            KeyType::Rune => return write!(f, "{}", self.rune),
            KeyType::Enter => "enter",
            KeyType::Esc => "esc",
            KeyType::Tab => "tab",
            KeyType::ShiftTab => "shift+tab",
            KeyType::Backspace => "backspace",
            KeyType::Up => "up",
            KeyType::Down => "down",
            KeyType::Left => "left",
            KeyType::Right => "right",
            KeyType::Home => "home",
            KeyType::End => "end",
            KeyType::PgUp => "pgup",
            KeyType::PgDown => "pgdown",
            KeyType::CtrlC => "ctrl+c",
            KeyType::CtrlD => "ctrl+d",
            KeyType::CtrlU => "ctrl+u",
        };
        f.write_str(name)
    }
}

fn key(kind: KeyType) -> Key {
    Key::new(kind)
}

/// Decodes the first key in `b`. It returns the key, how many bytes it
/// consumed, and false when `b` holds no complete key.
pub(crate) fn decode_key(b: &[u8]) -> (Key, usize, bool) {
    let Some(&first) = b.first() else {
        return (Key::default(), 0, false);
    };
    match first {
        0x03 => return (key(KeyType::CtrlC), 1, true),
        0x04 => return (key(KeyType::CtrlD), 1, true),
        0x15 => return (key(KeyType::CtrlU), 1, true),
        b'\r' | b'\n' => return (key(KeyType::Enter), 1, true),
        b'\t' => return (key(KeyType::Tab), 1, true),
        0x08 | 0x7f => return (key(KeyType::Backspace), 1, true),
        0x1b => return decode_escape(b),
        _ => {}
    }
    if first < 0x20 {
        // An unhandled control character: skip it rather than showing a glyph.
        return (Key::default(), 1, false);
    }
    match first_char(b) {
        Some((r, size)) => (Key::rune(r), size, true),
        None if b.len() < 4 => (Key::default(), 0, false), // a multi-byte rune split across reads
        None => (Key::default(), 1, false),
    }
}

/// The first character of `b` and its length in bytes, or `None` when
/// `b` does not start with a whole, valid one.
fn first_char(b: &[u8]) -> Option<(char, usize)> {
    let head = &b[..b.len().min(4)];
    let valid = match std::str::from_utf8(head) {
        Ok(s) => s,
        Err(e) => std::str::from_utf8(&head[..e.valid_up_to()]).unwrap_or(""),
    };
    valid.chars().next().map(|c| (c, c.len_utf8()))
}

/// Decodes an escape sequence. A sequence that is not complete yet
/// consumes nothing: `read_keys` waits briefly for the rest, and treats a
/// lone ESC that never grows as the escape key.
fn decode_escape(b: &[u8]) -> (Key, usize, bool) {
    let none = Key::default();
    if b.len() == 1 {
        return (none, 0, false);
    }
    if b[1] == b'[' || b[1] == b'O' {
        if b.len() < 3 {
            return (none, 0, false);
        }
        let kind = match b[2] {
            b'A' => Some(KeyType::Up),
            b'B' => Some(KeyType::Down),
            b'C' => Some(KeyType::Right),
            b'D' => Some(KeyType::Left),
            b'H' => Some(KeyType::Home),
            b'F' => Some(KeyType::End),
            b'Z' => Some(KeyType::ShiftTab),
            _ => None,
        };
        if let Some(kind) = kind {
            return (key(kind), 3, true);
        }
        // CSI <number> ~ and mouse reports, which the UI ignores.
        for i in 2..b.len() {
            if (0x40..=0x7e).contains(&b[i]) {
                if b[i] == b'~' {
                    let kind = match &b[2..i] {
                        b"1" | b"7" => Some(KeyType::Home),
                        b"4" | b"8" => Some(KeyType::End),
                        b"5" => Some(KeyType::PgUp),
                        b"6" => Some(KeyType::PgDown),
                        _ => None,
                    };
                    if let Some(kind) = kind {
                        return (key(kind), i + 1, true);
                    }
                }
                return (none, i + 1, false);
            }
        }
        return (none, 0, false); // incomplete sequence
    }
    if (0x20..=0x7e).contains(&b[1]) {
        // Alt and a key, which most terminals send as ESC then the key:
        // not a binding, and not the escape key either, which would
        // cancel a run.
        return (none, 2, false);
    }
    // ESC followed by something else: treat the ESC alone.
    (key(KeyType::Esc), 1, true)
}

/// How long `read_keys` waits for the rest of an escape sequence before
/// deciding the ESC was the escape key.
const ESCAPE_GRACE: Duration = Duration::from_millis(50);

/// How often `read_keys` looks at `done` while nothing is being typed.
const DONE_POLL: Duration = Duration::from_millis(100);

/// Decodes keys from `r` until it fails, handing them to `emit`, which
/// says whether to go on. Reads happen on their own thread so a sequence
/// split across reads can be completed, while a lone ESC still arrives
/// promptly.
pub(crate) fn read_keys<R>(mut r: R, mut emit: impl FnMut(Key) -> bool, done: &CancelToken)
where
    R: Read + Send + 'static,
{
    let (tx, chunks) = mpsc::channel::<Vec<u8>>();
    let reader_done = done.clone();
    std::thread::spawn(move || {
        let mut buf = [0u8; 256];
        loop {
            match r.read(&mut buf) {
                Ok(0) | Err(_) => return,
                Ok(n) => {
                    if reader_done.is_cancelled() || tx.send(buf[..n].to_vec()).is_err() {
                        return;
                    }
                }
            }
        }
    });

    let mut pending: Vec<u8> = Vec::new();
    // Emits every complete key in pending. With force set, an incomplete
    // sequence is resolved as a bare escape key. It reports whether the
    // reader should keep going.
    let mut flush = |pending: &mut Vec<u8>, force: bool| -> bool {
        while !pending.is_empty() {
            let (mut k, mut used, mut ok) = decode_key(pending);
            if used == 0 {
                if !force {
                    return true;
                }
                (k, used, ok) = (key(KeyType::Esc), 1, true);
            }
            pending.drain(..used);
            if !ok {
                continue;
            }
            if done.is_cancelled() || !emit(k) {
                return false;
            }
        }
        true
    };

    loop {
        if done.is_cancelled() {
            return;
        }
        let wait = if pending.is_empty() {
            DONE_POLL
        } else {
            ESCAPE_GRACE
        };
        match chunks.recv_timeout(wait) {
            Ok(b) => {
                pending.extend_from_slice(&b);
                if !flush(&mut pending, false) {
                    return;
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                if !pending.is_empty() && !flush(&mut pending, true) {
                    return;
                }
            }
            Err(RecvTimeoutError::Disconnected) => {
                flush(&mut pending, true);
                return;
            }
        }
    }
}
