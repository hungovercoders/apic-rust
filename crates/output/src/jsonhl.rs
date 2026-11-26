use crate::theme::Theme;

/// Colours the keys, strings, numbers and literals of an already-indented,
/// valid JSON document. It works line by line (JSON strings never span
/// lines) and passes anything unexpected through untouched, so a document
/// that is not JSON comes back unchanged.
pub fn highlight_json(t: &Theme, s: &str) -> String {
    s.split('\n')
        .map(|line| highlight_line(t, line))
        .collect::<Vec<_>>()
        .join("\n")
}

fn highlight_line(t: &Theme, line: &str) -> String {
    let b = line.as_bytes();
    let mut out = String::with_capacity(line.len());
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c == b'"' {
            let Some(end) = closing_quote(b, i) else {
                out.push_str(&line[i..]);
                return out;
            };
            let tok = &line[i..=end];
            if line[end + 1..].trim_start_matches(' ').starts_with(':') {
                out.push_str(&t.key.render(tok));
            } else {
                out.push_str(&t.str.render(tok));
            }
            i = end + 1;
        } else if c == b'-' || c.is_ascii_digit() {
            let mut j = i;
            while j < b.len() && b"0123456789eE+-.".contains(&b[j]) {
                j += 1;
            }
            out.push_str(&t.num.render(&line[i..j]));
            i = j;
        } else if c == b't' || c == b'f' || c == b'n' {
            let mut j = i;
            while j < b.len() && b[j].is_ascii_lowercase() {
                j += 1;
            }
            out.push_str(&t.lit.render(&line[i..j]));
            i = j;
        } else {
            // Copy up to the next byte that starts a token; text outside
            // ASCII goes through whole.
            let mut j = i + 1;
            while j < b.len() && !line.is_char_boundary(j) {
                j += 1;
            }
            out.push_str(&line[i..j]);
            i = j;
        }
    }
    out
}

/// Returns the index of the quote that closes the string opening at
/// `line[start]`, honouring backslash escapes.
fn closing_quote(line: &[u8], start: usize) -> Option<usize> {
    let mut i = start + 1;
    while i < line.len() {
        match line[i] {
            b'\\' => i += 1,
            b'"' => return Some(i),
            _ => {}
        }
        i += 1;
    }
    None
}
