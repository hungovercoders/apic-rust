/// Rewrites JSON text the way an encoder with HTML escaping on writes it:
/// `<`, `>` and `&` become `\u003c`, `\u003e` and `\u0026`, and U+2028 and
/// U+2029 become `\u2028` and `\u2029`. In valid JSON these characters only
/// occur inside strings, so the text keeps its meaning.
pub fn escape_html(json: &str) -> String {
    let mut out = String::with_capacity(json.len());
    for c in json.chars() {
        match c {
            '<' => out.push_str("\\u003c"),
            '>' => out.push_str("\\u003e"),
            '&' => out.push_str("\\u0026"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            c => out.push(c),
        }
    }
    out
}

/// Rewrites JSON text the way an encoder with HTML escaping off writes it:
/// only U+2028 and U+2029 become `\u2028` and `\u2029`.
pub fn escape_line_separators(json: &str) -> String {
    let mut out = String::with_capacity(json.len());
    for c in json.chars() {
        match c {
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes() {
        assert_eq!(
            escape_html("{\"a\":\"<b> & \u{2028}\"}"),
            "{\"a\":\"\\u003cb\\u003e \\u0026 \\u2028\"}"
        );
        assert_eq!(escape_line_separators("\"<\u{2029}>\""), "\"<\\u2029>\"");
    }
}
