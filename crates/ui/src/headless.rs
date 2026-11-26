//! Drives a model without a terminal or an event loop: it feeds keys in
//! and resolves the work they schedule inline. Tests use it, and so does
//! the docs screenshot generator, which is why the screenshots show what
//! the UI really draws rather than a hand-written approximation.

use crate::program::{Cmd, Msg};
use crate::term::{Key, KeyType};
use crate::ui::Model;

/// Bounds a command chain so a bug cannot hang the caller. A flow over
/// the whole project is the longest legitimate chain, and it is two
/// commands per request.
const SETTLE_LIMIT: usize = 1000;

/// Turns a binding name ("j", "enter", "ctrl+d", "shift+tab") into a
/// keypress. Anything else is taken as the literal rune, so "?" and "/"
/// work as written.
pub fn parse_key(name: &str) -> Key {
    const NAMED: [KeyType; 16] = [
        KeyType::Enter,
        KeyType::Esc,
        KeyType::Tab,
        KeyType::ShiftTab,
        KeyType::Backspace,
        KeyType::Up,
        KeyType::Down,
        KeyType::Left,
        KeyType::Right,
        KeyType::Home,
        KeyType::End,
        KeyType::PgUp,
        KeyType::PgDown,
        KeyType::CtrlC,
        KeyType::CtrlD,
        KeyType::CtrlU,
    ];
    for kind in NAMED {
        if Key::new(kind).to_string() == name {
            return Key::new(kind);
        }
    }
    match name.chars().next() {
        Some(r) => Key::rune(r),
        None => Key::default(),
    }
}

/// The callback `press_watch` runs as background work lands.
type Each<'a> = Option<&'a mut dyn FnMut(&mut Model)>;

impl Model {
    /// Tells the model how big the terminal is. The program loop does
    /// this from the real terminal; callers without one say so themselves.
    pub fn resize(&mut self, width: i32, height: i32) {
        self.update(Msg::Size { width, height });
    }

    /// Feeds one key and settles everything it starts, so a run sent by
    /// enter has landed by the time `press` returns.
    pub fn press(&mut self, name: &str) {
        let cmd = self.update(Msg::Key(parse_key(name)));
        self.settle(cmd);
    }

    /// Presses several keys in order, splitting a space-separated list
    /// for brevity: "j enter 3".
    pub fn press_all(&mut self, keys: &str) {
        for k in keys.split_whitespace() {
            self.press(k);
        }
    }

    /// `press` with a callback after every piece of background work the
    /// key started has landed: for `f`, once per request of the flow, so a
    /// caller can capture the screen as rows light up one by one. The
    /// callback also runs once at the end, after the last message.
    pub fn press_watch(&mut self, name: &str, mut each: impl FnMut(&mut Model)) {
        let cmd = self.update(Msg::Key(parse_key(name)));
        self.settle_watch(cmd, &mut Some(&mut each));
        each(self);
    }

    /// Resolves a command chain inline. Work the event loop would do in
    /// the background (a request, the next step of a flow) runs here and
    /// its message goes straight back to the model. Anything that needs
    /// the real program (a timer, the editor, quitting) ends the chain
    /// instead.
    pub(crate) fn settle(&mut self, cmd: Option<Cmd>) {
        self.settle_watch(cmd, &mut None);
    }

    fn settle_watch(&mut self, mut cmd: Option<Cmd>, each: &mut Each<'_>) {
        for _ in 0..SETTLE_LIMIT {
            let Some(run) = cmd.take() else {
                return;
            };
            match run() {
                None | Some(Msg::Quit | Msg::Delay(..) | Msg::Exec(_)) => return,
                Some(Msg::Async(work)) => {
                    cmd = self.update(work());
                    if let Some(each) = each {
                        each(self);
                    }
                }
                Some(Msg::Cmd(next)) => cmd = Some(next),
                Some(Msg::Batch(mut cmds)) => {
                    let Some(last) = cmds.pop() else {
                        return;
                    };
                    for c in cmds {
                        self.settle_watch(Some(c), each);
                    }
                    cmd = Some(last);
                }
                Some(msg) => cmd = self.update(msg),
            }
        }
    }
}
