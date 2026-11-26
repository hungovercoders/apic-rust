use apic_gocompat::quote;

use crate::mime::parse_media_type;
use crate::parse::{span_from, RE_HEADER};
use crate::{Header, Request};

/// One part of a multipart/form-data body, as written in the file: its
/// headers, then either literal content or a `< file` reference.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Part {
    pub headers: Vec<Header>,
    /// From Content-Disposition.
    pub name: String,
    /// From Content-Disposition, when the part is a file.
    pub filename: String,
    /// The part's own Content-Type header, when present.
    pub content_type: String,
    /// Literal content (a template), empty when `file` is set.
    pub body: String,
    /// `< path`: the file whose bytes are the content.
    pub file: String,
    /// `<@ path`: substitute `{{vars}}` inside the file too.
    pub file_templated: bool,
    /// Line of the `< path` reference.
    pub file_line: usize,
    /// Column where the path starts on that line.
    pub file_column: usize,
    /// Line of the delimiter that opens the part.
    pub line: usize,
}

/// A parsed multipart/form-data body.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Multipart {
    pub boundary: String,
    pub parts: Vec<Part>,
}

impl Multipart {
    /// Counts the parts whose content comes from a file.
    pub fn files(&self) -> usize {
        self.parts.iter().filter(|p| !p.file.is_empty()).count()
    }
}

/// A `< path` or `<@ path` reference in a request: the whole body, or one
/// part of a multipart body.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileRef {
    pub path: String,
    pub templated: bool,
    pub line: usize,
    pub column: usize,
}

fn trim_blanks_end(s: &str) -> &str {
    s.trim_end_matches([' ', '\t'])
}

/// Reads a `< path` or `<@ path` line: the path and whether it is the
/// templated form.
pub(crate) fn file_reference(t: &str) -> Option<(String, bool)> {
    if !t.starts_with("<@ ") && !t.starts_with("< ") {
        return None;
    }
    let templated = t.starts_with("<@");
    let rest = t.strip_prefix("<@").unwrap_or(t);
    let rest = rest.strip_prefix('<').unwrap_or(rest);
    Some((rest.trim().to_string(), templated))
}

impl Request {
    /// Returns the value of the first header with the given name,
    /// case-insensitively.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|h| h.name.eq_ignore_ascii_case(name))
            .map(|h| h.value.as_str())
    }

    /// Reports whether the request declares a multipart/form-data
    /// Content-Type. The body then holds parts (see [`Multipart`]) rather
    /// than one template.
    pub fn is_multipart(&self) -> bool {
        self.header("Content-Type")
            .and_then(|ct| parse_media_type(ct).ok())
            .is_some_and(|(mt, _)| mt.eq_ignore_ascii_case("multipart/form-data"))
    }

    /// Parses the body of a multipart/form-data request into its parts.
    /// It returns `Ok(None)` for a request that is not multipart, or whose
    /// whole body is a `< file`; an error names what is wrong with the
    /// body.
    pub fn multipart(&self) -> Result<Option<Multipart>, String> {
        if !self.is_multipart() || !self.body_file.is_empty() {
            return Ok(None);
        }
        let ct = self.header("Content-Type").unwrap_or("");
        let (_, params) =
            parse_media_type(ct).map_err(|e| format!("Content-Type {}: {e}", quote(ct)))?;
        let boundary = params.get("boundary").cloned().unwrap_or_default();
        if boundary.is_empty() {
            return Err("Content-Type multipart/form-data needs a boundary parameter, e.g. `multipart/form-data; boundary=WebAppBoundary`".to_string());
        }
        if self.body.trim().is_empty() {
            return Err(format!(
                "multipart body is empty: write the parts between `--{boundary}` delimiters"
            ));
        }
        let delim = format!("--{boundary}");
        let closing = format!("--{boundary}--");
        let lines: Vec<&str> = self.body.split('\n').collect();
        let mut m = Multipart {
            boundary,
            parts: Vec::new(),
        };
        let mut i = 0;
        while i < lines.len() && trim_blanks_end(lines[i]) != delim {
            if trim_blanks_end(lines[i]) == closing {
                break;
            }
            i += 1;
        }
        if i == lines.len() {
            return Err(format!("multipart body has no `{delim}` delimiter"));
        }
        let not_closed =
            || format!("multipart body is not closed: expected `{closing}` at the end");
        let mut closed = false;
        while i < lines.len() {
            let t = trim_blanks_end(lines[i]);
            if t == closing {
                closed = true;
                break;
            }
            if t != delim {
                return Err(format!(
                    "multipart body: line {}: expected `{delim}`",
                    self.body_line + i
                ));
            }
            let mut part = Part {
                line: self.body_line + i,
                ..Part::default()
            };
            i += 1;
            while i < lines.len() {
                let t = lines[i].trim();
                if t.is_empty() {
                    i += 1;
                    break;
                }
                let Some(mh) = RE_HEADER.captures(t) else {
                    return Err(format!(
                        "multipart body: line {}: expected a part header (`Name: value`) or a blank line before the content, got {}",
                        self.body_line + i,
                        quote(t)
                    ));
                };
                part.headers.push(Header {
                    name: mh[1].to_string(),
                    value: mh[2].to_string(),
                });
                i += 1;
            }
            let start = i;
            while i < lines.len() {
                let t = trim_blanks_end(lines[i]);
                if t == delim || t == closing {
                    break;
                }
                i += 1;
            }
            if i == lines.len() {
                return Err(not_closed());
            }
            let content = &lines[start..i];
            if content.len() == 1 {
                if let Some((path, templated)) = file_reference(content[0].trim()) {
                    part.file_templated = templated;
                    part.file = path;
                    part.file_line = self.body_line + start;
                    part.file_column = span_from(content[0], &part.file, content[0].find('<')).0;
                }
            }
            if part.file.is_empty() {
                part.body = content.join("\n");
            }
            for h in &part.headers {
                if h.name.eq_ignore_ascii_case("Content-Disposition") {
                    if let Ok((_, p)) = parse_media_type(&h.value) {
                        part.name = p.get("name").cloned().unwrap_or_default();
                        part.filename = p.get("filename").cloned().unwrap_or_default();
                    }
                } else if h.name.eq_ignore_ascii_case("Content-Type") {
                    part.content_type = h.value.clone();
                }
            }
            m.parts.push(part);
        }
        if !closed {
            return Err(not_closed());
        }
        if m.parts.is_empty() {
            return Err(format!(
                "multipart body has no parts between `{delim}` and `{closing}`"
            ));
        }
        Ok(Some(m))
    }

    /// Returns every file the request's body reads: the whole-body
    /// `< file`, or the `< file` parts of a multipart body. A multipart
    /// body that does not parse contributes nothing; validate reports it.
    pub fn body_files(&self) -> Vec<FileRef> {
        if !self.body_file.is_empty() {
            return vec![FileRef {
                path: self.body_file.clone(),
                templated: self.body_file_templated,
                line: self.body_file_line,
                column: self.body_file_column,
            }];
        }
        let Ok(Some(m)) = self.multipart() else {
            return Vec::new();
        };
        m.parts
            .into_iter()
            .filter(|p| !p.file.is_empty())
            .map(|p| FileRef {
                path: p.file,
                templated: p.file_templated,
                line: p.file_line,
                column: p.file_column,
            })
            .collect()
    }
}

/// Quotes a Content-Disposition parameter value the way RFC 2616
/// quoted-strings are written: only backslash and the double quote are
/// escaped, so a non-ASCII name or filename survives being read back
/// (`\u` escapes would be read as literal letters).
pub fn quote_param(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        if c == '\\' || c == '"' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}
