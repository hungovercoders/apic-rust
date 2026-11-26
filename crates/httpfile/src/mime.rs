//! Reads a media type and its parameters out of a header value such as
//! `multipart/form-data; boundary=x` or `form-data; name="a";
//! filename*=UTF-8''r%C3%A9sum%C3%A9.pdf`, following RFC 1521 and the
//! parameter extensions of RFC 2231.

use std::collections::BTreeMap;

fn is_tspecial(c: u8) -> bool {
    b"()<>@,;:\\\"/[]?=".contains(&c)
}

fn is_token_char(c: char) -> bool {
    let n = c as u32;
    n > 0x20 && n < 0x7f && !is_tspecial(n as u8)
}

/// Splits off the leading token.
fn consume_token(v: &str) -> (&str, &str) {
    match v.find(|c| !is_token_char(c)) {
        None => (v, ""),
        Some(0) => ("", v),
        Some(i) => v.split_at(i),
    }
}

/// Splits off a parameter value: a token or a quoted string. `None` means
/// there is no value to read.
fn consume_value(v: &str) -> Option<(String, &str)> {
    if v.is_empty() {
        return None;
    }
    let b = v.as_bytes();
    if b[0] != b'"' {
        let (token, rest) = consume_token(v);
        return (!token.is_empty()).then(|| (token.to_string(), rest));
    }
    let mut buf = Vec::new();
    let mut i = 1;
    while i < b.len() {
        let c = b[i];
        if c == b'"' {
            let value = String::from_utf8_lossy(&buf).into_owned();
            return Some((value, &v[i + 1..]));
        }
        // A backslash escapes only the special characters: some clients
        // send a full file path without escaping its backslashes, and
        // "C:\dev\foo.txt" should keep them.
        if c == b'\\' && i + 1 < b.len() && is_tspecial(b[i + 1]) {
            buf.push(b[i + 1]);
            i += 2;
            continue;
        }
        if c == b'\r' || c == b'\n' {
            return None;
        }
        buf.push(c);
        i += 1;
    }
    None // no closing quote
}

/// Reads one `; name=value` parameter and returns it with what follows.
fn consume_param(v: &str) -> Option<(String, String, &str)> {
    let rest = v.trim_start().strip_prefix(';')?;
    let (param, rest) = consume_token(rest.trim_start());
    if param.is_empty() {
        return None;
    }
    let rest = rest.trim_start().strip_prefix('=')?;
    let (value, rest) = consume_value(rest.trim_start())?;
    Some((param.to_lowercase(), value, rest))
}

fn check_media_type(s: &str) -> Result<(), String> {
    let (typ, rest) = consume_token(s);
    if typ.is_empty() {
        return Err("mime: no media type".to_string());
    }
    if rest.is_empty() {
        return Ok(());
    }
    let Some(rest) = rest.strip_prefix('/') else {
        return Err("mime: expected slash after first token".to_string());
    };
    let (subtype, rest) = consume_token(rest);
    if subtype.is_empty() {
        return Err("mime: expected token after slash".to_string());
    }
    if !rest.is_empty() {
        return Err("mime: unexpected content after media subtype".to_string());
    }
    Ok(())
}

fn percent_unescape(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hex = s.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    Some(String::from_utf8_lossy(&out).into_owned())
}

/// Decodes an RFC 2231 extended value, `charset'language'percent-encoded`.
fn decode_2231(v: &str) -> Option<String> {
    let mut parts = v.splitn(3, '\'');
    let (charset, _language, encoded) = (parts.next()?, parts.next()?, parts.next()?);
    let charset = charset.to_lowercase();
    if charset != "us-ascii" && charset != "utf-8" {
        return None;
    }
    percent_unescape(encoded)
}

/// Parses a media type value and its parameters. The media type comes
/// back lower-cased and trimmed, the parameter names lower-cased;
/// continuations and encoded values (`name*0`, `name*`) are joined and
/// decoded. The error is the reason as text (`mime: no media type`,
/// `mime: invalid media parameter`, ...).
pub fn parse_media_type(v: &str) -> Result<(String, BTreeMap<String, String>), String> {
    let base = v.split(';').next().unwrap_or("");
    let media_type = base.to_lowercase().trim().to_string();
    check_media_type(&media_type)?;

    let mut params = BTreeMap::new();
    // Base name -> piece name -> value, for the parameters with a `*`.
    let mut continuation: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();

    let mut v = &v[base.len()..];
    loop {
        v = v.trim_start();
        if v.is_empty() {
            break;
        }
        let Some((key, value, rest)) = consume_param(v) else {
            if v.trim() == ";" {
                break; // a trailing semicolon is not an error
            }
            return Err("mime: invalid media parameter".to_string());
        };
        let pieces = match key.split_once('*') {
            Some((base_name, _)) => continuation.entry(base_name.to_string()).or_default(),
            None => &mut params,
        };
        if pieces.get(&key).is_some_and(|old| *old != value) {
            // Repeating a parameter is tolerated only when it says the same.
            return Err("mime: duplicate parameter name".to_string());
        }
        pieces.insert(key, value);
        v = rest;
    }

    for (key, pieces) in &continuation {
        if let Some(single) = pieces.get(&format!("{key}*")) {
            if let Some(decoded) = decode_2231(single) {
                params.insert(key.clone(), decoded);
            }
            continue;
        }
        let mut buf = String::new();
        let mut found = false;
        for n in 0.. {
            let simple = format!("{key}*{n}");
            if let Some(piece) = pieces.get(&simple) {
                found = true;
                buf.push_str(piece);
                continue;
            }
            let Some(piece) = pieces.get(&format!("{simple}*")) else {
                break;
            };
            found = true;
            if n == 0 {
                if let Some(decoded) = decode_2231(piece) {
                    buf.push_str(&decoded);
                }
            } else if let Some(decoded) = percent_unescape(piece) {
                buf.push_str(&decoded);
            }
        }
        if found {
            params.insert(key.clone(), buf);
        }
    }
    Ok((media_type, params))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn get(v: &str, key: &str) -> Option<String> {
        parse_media_type(v).ok()?.1.get(key).cloned()
    }

    #[test]
    fn media_types() {
        let (mt, p) =
            parse_media_type("Multipart/Form-Data; Boundary=abc; charset=\"utf-8\";").unwrap();
        assert_eq!(mt, "multipart/form-data");
        assert_eq!(p["boundary"], "abc");
        assert_eq!(p["charset"], "utf-8");
        assert_eq!(parse_media_type("form-data").unwrap().0, "form-data");
        assert_eq!(
            get(
                "form-data; name=\"a b\"; filename=\"r\u{e9}sum\u{e9} \\\"x\\\".pdf\"",
                "filename"
            )
            .unwrap(),
            "r\u{e9}sum\u{e9} \"x\".pdf"
        );
        assert_eq!(
            get("form-data; filename=\"C:\\dev\\a.txt\"", "filename").unwrap(),
            "C:\\dev\\a.txt"
        );
        assert_eq!(
            get("a/b; filename*=UTF-8''r%C3%A9.pdf", "filename").unwrap(),
            "r\u{e9}.pdf"
        );
        assert_eq!(get("a/b; x*0=\"ab\"; x*1=cd", "x").unwrap(), "abcd");
        for bad in [
            "",
            "a/",
            "a/b/c",
            "a b",
            "a/b; x",
            "a/b; x=\"open",
            "a/b; x=1; x=2",
        ] {
            assert!(parse_media_type(bad).is_err(), "{bad:?}");
        }
        assert!(parse_media_type("a/b; x=1; x=1").is_ok());
    }
}
