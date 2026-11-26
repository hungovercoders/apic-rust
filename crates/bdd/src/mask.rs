//! Masks secret values inside the structured reports, value by value, so
//! a secret that happens to equal a status, element or attribute name
//! cannot change what the report means.

use apic_gocompat::indent_json;

use crate::fmt::{escape_attr, escape_text};
use crate::jsonmatch::{decode_json, to_json, Json};
use crate::state::Run;

/// The cucumber JSON fields whose string values carry structure (result
/// statuses, element types, step keywords, locations) rather than user
/// text; masking them would change what the report means.
const CUKE_STRUCTURAL: [&str; 6] = ["status", "type", "keyword", "uri", "location", "line"];

impl Run<'_> {
    /// Masks secrets inside the string values of a JSON document, leaving
    /// keys, numbers and structure untouched, so a numeric-looking secret
    /// cannot corrupt a cucumber report.
    pub(crate) fn mask_json(&self, data: &[u8]) -> Result<Vec<u8>, String> {
        let mut v = decode_json(data)?;
        self.mask_value(&mut v);
        let mut out = indent_json(&to_json(&v), "", "  ");
        out.push('\n');
        Ok(out.into_bytes())
    }

    fn mask_value(&self, v: &mut Json) {
        match v {
            Json::String(s) => *s = self.mask(s),
            Json::Array(items) => items.iter_mut().for_each(|item| self.mask_value(item)),
            Json::Object(members) => {
                for (k, val) in members.iter_mut() {
                    let structural = CUKE_STRUCTURAL.contains(&k.as_str());
                    if structural && matches!(val, Json::String(_)) {
                        continue;
                    }
                    self.mask_value(val);
                }
            }
            _ => {}
        }
    }

    /// Masks the text and the user-supplied attributes (name, message) of
    /// an XML report (JUnit) while leaving its structure untouched.
    pub(crate) fn mask_xml(&self, data: &[u8]) -> Result<Vec<u8>, String> {
        let src = std::str::from_utf8(data).map_err(|e| e.to_string())?;
        let mut out = String::with_capacity(src.len() + 1);
        let mut open: Vec<&str> = Vec::new();
        let mut rest = src;
        while !rest.is_empty() {
            if let Some(after) = rest.strip_prefix("<?") {
                let end = after
                    .find("?>")
                    .ok_or("unterminated processing instruction")?;
                out.push_str(&rest[..end + 4]);
                rest = &after[end + 2..];
            } else if let Some(after) = rest.strip_prefix("</") {
                let end = after.find('>').ok_or("unterminated end tag")?;
                let name = after[..end].trim();
                if open.pop() != Some(name) {
                    return Err(format!("unexpected end element </{name}>"));
                }
                out.push_str(&format!("</{name}>"));
                rest = &after[end + 1..];
            } else if rest.starts_with("<!") {
                return Err("unsupported markup".to_string());
            } else if let Some(after) = rest.strip_prefix('<') {
                let (name, attrs, self_closing, used) = start_tag(after)?;
                out.push('<');
                out.push_str(name);
                for (attr, value) in attrs {
                    // Only user text is masked; counts, timings, statuses
                    // and types keep the report machine-readable.
                    let value = if attr == "name" || attr == "message" {
                        self.mask(&value)
                    } else {
                        value
                    };
                    out.push_str(&format!(" {attr}=\"{}\"", escape_attr(&value)));
                }
                out.push('>');
                if self_closing {
                    out.push_str(&format!("</{name}>"));
                } else {
                    open.push(name);
                }
                rest = &after[used..];
            } else {
                let end = rest.find('<').unwrap_or(rest.len());
                let text = unescape(&rest[..end])?
                    .replace("\r\n", "\n")
                    .replace('\r', "\n");
                out.push_str(&escape_text(&self.mask(&text)));
                rest = &rest[end..];
            }
        }
        if !open.is_empty() {
            return Err("unexpected EOF".to_string());
        }
        out.push('\n');
        Ok(out.into_bytes())
    }
}

type StartTag<'a> = (&'a str, Vec<(&'a str, String)>, bool, usize);

/// Reads a start tag from just after its `<`: the name, the attributes
/// with their values unescaped, whether it closes itself, and how much
/// text it took.
fn start_tag(src: &str) -> Result<StartTag<'_>, String> {
    let name_end = src
        .find(|c: char| c.is_whitespace() || c == '>' || c == '/')
        .ok_or("unterminated start tag")?;
    let name = &src[..name_end];
    if name.is_empty() {
        return Err("expected element name after <".to_string());
    }
    let mut attrs = Vec::new();
    let mut pos = name_end;
    loop {
        let trimmed = src[pos..].trim_start();
        pos = src.len() - trimmed.len();
        if trimmed.starts_with("/>") {
            return Ok((name, attrs, true, pos + 2));
        }
        if trimmed.starts_with('>') {
            return Ok((name, attrs, false, pos + 1));
        }
        let eq = trimmed
            .find('=')
            .ok_or("attribute name without = in element")?;
        let attr = trimmed[..eq].trim();
        let value = trimmed[eq + 1..].trim_start();
        let quote = value.chars().next().filter(|c| *c == '"' || *c == '\'');
        let quote = quote.ok_or("unquoted or missing attribute value in element")?;
        let close = value[1..]
            .find(quote)
            .ok_or("unterminated attribute value")?;
        attrs.push((attr, unescape(&value[1..=close])?));
        pos = src.len() - value.len() + close + 2;
    }
}

/// Replaces character and entity references with what they stand for.
fn unescape(s: &str) -> Result<String, String> {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp + 1..];
        let end = after.find(';').ok_or("invalid character entity")?;
        let entity = &after[..end];
        let c = match entity {
            "lt" => '<',
            "gt" => '>',
            "amp" => '&',
            "apos" => '\'',
            "quot" => '"',
            _ => {
                let code = if let Some(hex) = entity.strip_prefix("#x") {
                    u32::from_str_radix(hex, 16).ok()
                } else if let Some(dec) = entity.strip_prefix('#') {
                    dec.parse().ok()
                } else {
                    None
                };
                code.and_then(char::from_u32)
                    .ok_or_else(|| format!("invalid character entity &{entity};"))?
            }
        };
        out.push(c);
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}
