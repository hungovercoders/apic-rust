/// Returns `s` as a double-quoted string literal: `"` and `\` are escaped,
/// the common control characters are written as `\n`, `\t` and so on, and
/// anything else that does not print is written as `\x1b`, `\u200b` or
/// `\U0010ffff`.
pub fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{07}' => out.push_str("\\a"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{0b}' => out.push_str("\\v"),
            c if printable(c) => out.push(c),
            c if (c as u32) < 0x80 => out.push_str(&format!("\\x{:02x}", c as u32)),
            c if (c as u32) < 0x10000 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push_str(&format!("\\U{:08x}", c as u32)),
        }
    }
    out.push('"');
    out
}

/// A close approximation of "is a graphic character or an ASCII space".
fn printable(c: char) -> bool {
    if c == ' ' {
        return true;
    }
    if c.is_control() || c.is_whitespace() {
        return false;
    }
    !matches!(c as u32,
        0xAD | 0x200B..=0x200F | 0x202A..=0x202E | 0x2060..=0x206F | 0xFEFF | 0xFFF9..=0xFFFB
        | 0xE000..=0xF8FF | 0xFFFE | 0xFFFF | 0xF0000..=0x10FFFF)
}

#[cfg(test)]
mod tests {
    use super::quote;

    #[test]
    fn quotes() {
        assert_eq!(quote("plain"), "\"plain\"");
        assert_eq!(quote("a\tb\n"), "\"a\\tb\\n\"");
        assert_eq!(quote("say \"hi\" \\"), "\"say \\\"hi\\\" \\\\\"");
        assert_eq!(quote("\u{1b}[0m"), "\"\\x1b[0m\"");
        assert_eq!(quote("caf\u{e9} \u{2713}"), "\"caf\u{e9} \u{2713}\"");
        assert_eq!(quote("\u{200b}"), "\"\\u200b\"");
    }
}
