//! Regular expressions with the RE2 behaviour apic's files were written
//! against: `\d`, `\w`, `\s` and `\b` are ASCII, `\Q...\E` quotes literal
//! text, and inside a character class `[`, `&` and `~` are ordinary
//! characters.

use regex::Regex;

/// Compiles `pattern`. The error is one line naming what is wrong and the
/// pattern, as `error parsing regexp: unclosed group: `(``.
pub fn compile_regex(pattern: &str) -> Result<Regex, String> {
    Regex::new(&translate(pattern)).map_err(|e| {
        let text = e.to_string();
        let reason = text.lines().last().unwrap_or("").trim();
        let reason = reason.strip_prefix("error: ").unwrap_or(reason);
        format!("error parsing regexp: {reason}: `{pattern}`")
    })
}

/// Rewrites the constructs that would otherwise be read differently.
fn translate(pattern: &str) -> String {
    let chars: Vec<char> = pattern.chars().collect();
    let mut out = String::with_capacity(pattern.len() + 8);
    let mut in_class = false;
    let mut class_start = 0; // index in out where the class body begins
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' {
            let Some(&e) = chars.get(i + 1) else {
                out.push(c);
                break;
            };
            i += 2;
            match e {
                'd' => out.push_str("[0-9]"),
                'D' => out.push_str("[^0-9]"),
                'w' => out.push_str("[0-9A-Za-z_]"),
                'W' => out.push_str("[^0-9A-Za-z_]"),
                's' => out.push_str("[\\t\\n\\f\\r ]"),
                'S' => out.push_str("[^\\t\\n\\f\\r ]"),
                'b' if !in_class => out.push_str("(?-u:\\b)"),
                'B' if !in_class => out.push_str("(?-u:\\B)"),
                'Q' if !in_class => {
                    let mut literal = String::new();
                    while i < chars.len() {
                        if chars[i] == '\\' && chars.get(i + 1) == Some(&'E') {
                            i += 2;
                            break;
                        }
                        literal.push(chars[i]);
                        i += 1;
                    }
                    out.push_str(&regex::escape(&literal));
                }
                e => {
                    out.push('\\');
                    out.push(e);
                }
            }
            continue;
        }
        if !in_class {
            if c == '[' {
                in_class = true;
                out.push(c);
                if chars.get(i + 1) == Some(&'^') {
                    out.push('^');
                    i += 1;
                }
                class_start = out.len();
                i += 1;
                continue;
            }
            out.push(c);
            i += 1;
            continue;
        }
        match c {
            ']' if out.len() > class_start => {
                in_class = false;
                out.push(c);
            }
            '[' if chars.get(i + 1) == Some(&':') => {
                // A POSIX class such as [:alpha:] is copied whole.
                let rest: String = chars[i..].iter().collect();
                match rest.find(":]") {
                    Some(end) => {
                        let n = rest[..end + 2].chars().count();
                        out.push_str(&rest[..end + 2]);
                        i += n;
                        continue;
                    }
                    None => out.push_str("\\["),
                }
            }
            '[' | '&' | '~' => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_as_written() {
        let cases = [
            (r"^\d+$", "123", true),
            (r"^\d+$", "١٢٣", false),
            (r"^\w+$", "héllo", false),
            (r"^[\w-]+$", "a-b_c", true),
            (r"^[^\d]+$", "abc", true),
            (r"a\sb", "a\u{a0}b", false),
            (r"\bé", "é", false),
            (r"^\Qa.b\E+$", "a.bbb", true),
            (r"^\Qa.b\E$", "axb", false),
            (r"^[[a]+$", "[a[", true),
            (r"^[a&&b]+$", "a&b", true),
            (r"^[[:alpha:]]+$", "abc", true),
            (r"^[]a]+$", "]a]", true),
            (r"(?i)^abc$", "ABC", true),
            (r"^a.*e$", "alice", true),
            (r"[bc]an", "banana", true),
        ];
        for (pattern, text, want) in cases {
            let re = compile_regex(pattern).unwrap_or_else(|e| panic!("{pattern}: {e}"));
            assert_eq!(re.is_match(text), want, "{pattern} on {text}");
        }
    }

    #[test]
    fn errors_are_one_line() {
        let e = compile_regex("(").unwrap_err();
        assert!(
            e.starts_with("error parsing regexp: ") && e.ends_with(": `(`") && !e.contains('\n'),
            "{e}"
        );
    }
}
