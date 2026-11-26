//! Reading a result back from its JSON.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use apic_gocompat::{escape_html, json_string};
use apic_httpc::canonical_header_key;
use apic_httpfile::Header;
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use serde::Deserialize;

use crate::result::{Body, Resolved, Response, Result, BASE64};

#[derive(Deserialize, Default)]
#[serde(default)]
struct StoredRequest {
    name: String,
    file: String,
    line: usize,
    method: String,
    url: String,
    headers: BTreeMap<String, String>,
    body: String,
    auth: String,
    http_version: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct StoredAssert {
    expr: String,
    pass: bool,
    actual: String,
    expected: String,
    error: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Stored {
    ok: bool,
    request: StoredRequest,
    response: Option<Response>,
    captures: BTreeMap<String, String>,
    asserts: Vec<StoredAssert>,
    errors: Vec<String>,
    attempts: usize,
    saved_to: String,
}

/// Reads a result back from the JSON `apic run --json` prints, as the
/// response history stores it, so the renderers can show it as they show
/// a fresh one. What was masked when it was written stays masked; the
/// request's own name for each header is the one stored.
pub fn parse_result(data: &str) -> std::result::Result<Result, String> {
    let s: Stored = serde_json::from_str(data).map_err(|e| e.to_string())?;
    let q = s.request;
    let mut res = Result {
        ok: s.ok,
        request: Resolved {
            name: q.name,
            file: q.file,
            line: q.line,
            method: q.method,
            url: q.url,
            body: q.body,
            auth: q.auth,
            http_version: q.http_version,
            // Sorted by name: the map is.
            headers: q
                .headers
                .into_iter()
                .map(|(name, value)| Header { name, value })
                .collect(),
            ..Resolved::default()
        },
        captures: s.captures,
        asserts: s
            .asserts
            .into_iter()
            .map(|a| apic_assert::Result {
                expr: a.expr,
                pass: a.pass,
                actual: a.actual,
                expected: a.expected,
                error: a.error,
            })
            .collect(),
        errors: s.errors,
        attempts: s.attempts,
        saved_to: s.saved_to,
        ..Result::default()
    };
    if let Some(r) = s.response {
        let body = raw_body(&r)?;
        res.raw = Some(Arc::new(apic_selector::Response {
            status: r.status,
            status_text: r.status_text.clone(),
            headers: r
                .headers
                .iter()
                .map(|(k, v)| (canonical_header_key(k), v.clone()))
                .collect(),
            body,
            duration: Duration::from_millis(r.duration_ms.max(0) as u64),
        }));
        res.response = Some(r);
    }
    Ok(res)
}

/// Turns a response's body back into bytes: base64 decoded for a binary
/// body, the text for a text one, and compact JSON for a JSON one.
fn raw_body(r: &Response) -> std::result::Result<Vec<u8>, String> {
    match &r.body {
        Body::None => Ok(Vec::new()),
        Body::Text(b) if r.body_encoding == BASE64 => {
            STANDARD.decode(b.as_bytes()).map_err(|e| e.to_string())
        }
        Body::Text(b) => Ok(b.as_bytes().to_vec()),
        Body::Json(b) => Ok(escape_html(b).into_bytes()),
    }
}

/// Rewrites valid JSON text compactly with the members of every object
/// sorted by name, numbers kept as they were written and strings written
/// afresh: what reading a document into a tree and writing it back gives.
pub(crate) fn canonical_json(src: &str) -> String {
    let mut p = Canon {
        b: src.as_bytes(),
        i: 0,
    };
    let mut out = String::with_capacity(src.len());
    p.value(&mut out);
    out
}

struct Canon<'a> {
    b: &'a [u8],
    i: usize,
}

impl Canon<'_> {
    fn ws(&mut self) {
        while self.i < self.b.len() && matches!(self.b[self.i], b' ' | b'\t' | b'\r' | b'\n') {
            self.i += 1;
        }
    }

    /// The raw text of the string literal at the cursor, quotes included.
    fn string(&mut self) -> &str {
        let start = self.i;
        self.i += 1;
        while self.i < self.b.len() && self.b[self.i] != b'"' {
            if self.b[self.i] == b'\\' {
                self.i += 1;
            }
            self.i += 1;
        }
        self.i = (self.i + 1).min(self.b.len());
        std::str::from_utf8(&self.b[start..self.i]).unwrap_or("\"\"")
    }

    fn decoded(&mut self) -> String {
        serde_json::from_str(self.string()).unwrap_or_default()
    }

    fn value(&mut self, out: &mut String) {
        self.ws();
        match self.b.get(self.i) {
            Some(b'{') => {
                self.i += 1;
                let mut members: BTreeMap<String, String> = BTreeMap::new();
                loop {
                    self.ws();
                    match self.b.get(self.i) {
                        Some(b'"') => {
                            let key = self.decoded();
                            self.ws();
                            self.i += 1; // the colon
                            let mut v = String::new();
                            self.value(&mut v);
                            members.insert(key, v);
                        }
                        Some(b',') => self.i += 1,
                        _ => {
                            self.i += 1;
                            break;
                        }
                    }
                }
                out.push('{');
                for (n, (k, v)) in members.iter().enumerate() {
                    if n > 0 {
                        out.push(',');
                    }
                    out.push_str(&json_string(k, false));
                    out.push(':');
                    out.push_str(v);
                }
                out.push('}');
            }
            Some(b'[') => {
                self.i += 1;
                out.push('[');
                let mut first = true;
                loop {
                    self.ws();
                    match self.b.get(self.i) {
                        Some(b']') | None => {
                            self.i += 1;
                            break;
                        }
                        Some(b',') => self.i += 1,
                        Some(_) => {
                            if !first {
                                out.push(',');
                            }
                            first = false;
                            self.value(out);
                        }
                    }
                }
                out.push(']');
            }
            Some(b'"') => {
                let s = self.decoded();
                out.push_str(&json_string(&s, false));
            }
            Some(_) => {
                let start = self.i;
                while self.i < self.b.len()
                    && !matches!(
                        self.b[self.i],
                        b',' | b']' | b'}' | b' ' | b'\t' | b'\r' | b'\n'
                    )
                {
                    self.i += 1;
                }
                out.push_str(std::str::from_utf8(&self.b[start..self.i]).unwrap_or("null"));
            }
            None => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical() {
        assert_eq!(
            canonical_json(
                r#" { "b" : [ 1.50 , true, null, {"z":"é<", "a":{}} ], "a" : "x\ny" } "#
            ),
            "{\"a\":\"x\\ny\",\"b\":[1.50,true,null,{\"a\":{},\"z\":\"é<\"}]}"
        );
        assert_eq!(canonical_json("[]"), "[]");
        assert_eq!(canonical_json(" 7 "), "7");
    }
}
