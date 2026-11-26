//! The response half of redaction. The request half (`display_headers`,
//! `display_body`, `display_url`) lives beside `Resolved`; both are driven
//! by `Result::redact`, set from the runner's `redact` option.
//!
//! What --redact hides is a documented contract: see the site's cli page.
//! The rule is that structure survives and values do not, so a redacted
//! run still tells CI which request failed and why without writing a
//! credential into a stored log.

use std::collections::BTreeMap;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;

use crate::result::{Body, Response, Result, BASE64, MASKED};

/// These carry credentials the server sends back. They are masked whenever
/// the response is shown. Captures read the raw header, so
/// "# @capture sid = header.set-cookie" keeps working; only display changes.
const SENSITIVE_RESPONSE_HEADERS: [&str; 5] = [
    "set-cookie",
    "www-authenticate",
    "proxy-authenticate",
    "authorization",
    "x-amz-security-token",
];

impl Response {
    /// The response headers with every value masked when redacting, and
    /// sensitive values masked either way.
    pub fn display_headers(&self, redact: bool) -> BTreeMap<String, String> {
        self.headers
            .iter()
            .map(|(k, v)| {
                if redact || SENSITIVE_RESPONSE_HEADERS.contains(&k.to_lowercase().as_str()) {
                    (k.clone(), MASKED.to_string())
                } else {
                    (k.clone(), v.clone())
                }
            })
            .collect()
    }
}

impl Result {
    /// The response as it should be shown: status, timing and size
    /// survive because they are not secrets and CI needs them, while the
    /// body and header values are masked when redacting.
    pub fn display_response(&self) -> Option<Response> {
        let resp = self.response.as_ref()?;
        let mut out = resp.clone();
        out.headers = resp.display_headers(self.redact);
        if self.redact && !out.body.is_none() {
            out.body = Body::text(MASKED);
            out.body_encoding = String::new(); // the mask is text, whatever the body was
        } else if let Some(head) = self.limited() {
            // The first body_limit bytes, as text: a cut JSON document is
            // no longer one, so it is a string, and a cut binary body is
            // the base64 of its first bytes.
            out.body = if out.body_encoding == BASE64 {
                Body::text(STANDARD.encode(head))
            } else {
                Body::text(String::from_utf8_lossy(text_prefix(head)))
            };
            out.body_truncated = true;
        }
        Some(out)
    }

    /// The first `body_limit` bytes of the body, when the limit cuts it.
    fn limited(&self) -> Option<&[u8]> {
        let raw = self.raw.as_ref()?;
        (self.body_limit > 0 && raw.body.len() > self.body_limit)
            .then(|| &raw.body[..self.body_limit])
    }

    /// The raw response body for renderers, masked when redacting; empty
    /// when there is no response.
    pub fn display_raw_body(&self) -> Vec<u8> {
        let Some(raw) = &self.raw else {
            return Vec::new();
        };
        if self.redact && !raw.body.is_empty() {
            return MASKED.as_bytes().to_vec();
        }
        if let Some(head) = self.limited() {
            if self
                .response
                .as_ref()
                .is_some_and(|r| r.body_encoding == BASE64)
            {
                return head.to_vec(); // bytes, not text: the cut is on no character
            }
            return text_prefix(head).to_vec();
        }
        raw.body.clone()
    }

    /// The assertion results with the values they carry masked when
    /// redacting. An assertion like "# @assert body.$.token == {{token}}"
    /// bakes the rendered secret into the expression, the expected and
    /// the actual value, so all three are hidden; `pass` and `error` are
    /// structural and survive.
    pub fn display_asserts(&self) -> Vec<apic_assert::Result> {
        if !self.redact {
            return self.asserts.clone();
        }
        self.asserts
            .iter()
            .map(|a| {
                let mut a = a.clone();
                a.expr = redact_expr(&a.expr);
                if !a.actual.is_empty() {
                    a.actual = MASKED.to_string();
                }
                if !a.expected.is_empty() {
                    a.expected = MASKED.to_string();
                }
                a
            })
            .collect()
    }
}

/// Reports whether `p` starts with a whole character: a complete
/// encoding, or a byte that cannot start or continue one.
fn full_rune(p: &[u8]) -> bool {
    let Some(&first) = p.first() else {
        return false;
    };
    let (need, lo, hi) = match first {
        0xC2..=0xDF => (2, 0x80, 0xBF),
        0xE0 => (3, 0xA0, 0xBF),
        0xE1..=0xEC | 0xEE..=0xEF => (3, 0x80, 0xBF),
        0xED => (3, 0x80, 0x9F),
        0xF0 => (4, 0x90, 0xBF),
        0xF1..=0xF3 => (4, 0x80, 0xBF),
        0xF4 => (4, 0x80, 0x8F),
        _ => return true,
    };
    if p.len() >= need {
        return true;
    }
    if p.len() > 1 && !(lo..=hi).contains(&p[1]) {
        return true;
    }
    p.len() > 2 && !(0x80..=0xBF).contains(&p[2])
}

/// Drops a multi-byte character the cut fell inside, so the text stays
/// valid UTF-8. Only the last character can be cut, so only the last few
/// bytes are looked at: a stray invalid byte earlier in the body is the
/// body's own and stays.
pub(crate) fn text_prefix(b: &[u8]) -> &[u8] {
    for i in (b.len().saturating_sub(4)..b.len()).rev() {
        if b[i] & 0xC0 == 0x80 {
            continue;
        }
        if !full_rune(&b[i..]) {
            return &b[..i];
        }
        break;
    }
    b
}

/// Keeps the selector and operator of an assertion expression and hides
/// the expected value.
pub fn redact_expr(expr: &str) -> String {
    apic_assert::redact(expr, MASKED)
}
