//! ANSI colours for the reports, and the writer that takes them out again
//! when the destination is not a terminal.

use std::io::Write;

const ESC: char = '\x1b';

/// A function that colours a piece of text.
pub(crate) type ColorFn = fn(&str) -> String;

fn colorize(s: &str, code: u8) -> String {
    format!("{ESC}[{code}m{s}{ESC}[0m")
}

fn bold(s: String) -> String {
    s.replacen("\x1b[", "\x1b[1;", 1)
}

pub(crate) fn red(s: &str) -> String {
    colorize(s, 31)
}

pub(crate) fn green(s: &str) -> String {
    colorize(s, 32)
}

pub(crate) fn yellow(s: &str) -> String {
    colorize(s, 33)
}

pub(crate) fn cyan(s: &str) -> String {
    colorize(s, 36)
}

pub(crate) fn red_bold(s: &str) -> String {
    bold(colorize(s, 31))
}

pub(crate) fn black_bold(s: &str) -> String {
    bold(colorize(s, 30))
}

pub(crate) fn cyan_bold(s: &str) -> String {
    bold(colorize(s, 36))
}

pub(crate) fn white_bold(s: &str) -> String {
    bold(colorize(s, 37))
}

/// Removes colour sequences (`ESC [ ... letter`) from one piece of output.
/// An escape character followed by anything but `[` is dropped with the
/// character after it, and a sequence cut short by the end of the piece is
/// dropped too.
pub(crate) fn uncolored(data: &str) -> String {
    let mut out = String::with_capacity(data.len());
    let mut chars = data.chars();
    'outer: while let Some(c1) = chars.next() {
        if c1 != ESC {
            out.push(c1);
            continue;
        }
        match chars.next() {
            None => break,
            Some('[') => {}
            Some(_) => continue,
        }
        loop {
            match chars.next() {
                None => break 'outer,
                Some(c) if c.is_ascii_alphabetic() || c == '@' => break,
                Some(_) => {}
            }
        }
    }
    out
}

/// Where a formatter prints: the report destination, with colours taken
/// out when they are not wanted. Write errors are not reported here; a
/// destination that needs them keeps the first one itself.
pub(crate) struct Out<'a> {
    w: &'a mut dyn Write,
    no_colors: bool,
}

impl<'a> Out<'a> {
    pub(crate) fn new(w: &'a mut dyn Write, no_colors: bool) -> Out<'a> {
        Out { w, no_colors }
    }

    /// Writes one piece of output.
    pub(crate) fn print(&mut self, s: &str) {
        if self.no_colors {
            let _ = self.w.write_all(uncolored(s).as_bytes());
        } else {
            let _ = self.w.write_all(s.as_bytes());
        }
    }

    /// Writes one piece of output and a newline.
    pub(crate) fn println(&mut self, s: &str) {
        let mut line = String::with_capacity(s.len() + 1);
        line.push_str(s);
        line.push('\n');
        self.print(&line);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours_and_bold() {
        assert_eq!(red("x"), "\x1b[31mx\x1b[0m");
        assert_eq!(black_bold("x"), "\x1b[1;30mx\x1b[0m");
        assert_eq!(uncolored(&format!("a {} b", cyan_bold("c"))), "a c b");
    }

    #[test]
    fn stray_escapes_are_dropped() {
        assert_eq!(uncolored("a\x1bXb"), "ab");
        assert_eq!(uncolored("a\x1b"), "a");
        assert_eq!(uncolored("a\x1b[31"), "a");
    }
}
