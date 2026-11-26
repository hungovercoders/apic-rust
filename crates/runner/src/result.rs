//! What a run produces: the resolved request, the response and the
//! result, with the JSON shape `apic run --json` prints.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use apic_gocompat::{compact_json, escape_html, quote};
use apic_httpfile::{Header, Request};
use serde::de::Error as _;
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

use crate::errors::ErrorInfo;
use crate::proxy::ProxyInfo;
use crate::tls::TlsInfo;

/// What a hidden value is replaced with in output.
pub const MASKED: &str = "***";

/// [`Response::body_encoding`] for a body that is not text.
pub const BASE64: &str = "base64";

/// A request with every placeholder substituted.
///
/// It serialises with its headers as an object (sensitive values masked)
/// and its body as a string; [`Result`]'s own serialisation applies
/// --redact on top.
#[derive(Clone, Debug, Default)]
pub struct Resolved {
    pub name: String,
    pub file: String,
    pub line: usize,
    pub method: String,
    pub url: String,
    pub headers: Vec<Header>,
    pub body: String,
    /// The auth type applied, e.g. "aws".
    pub auth: String,
    /// The version the request line asks for, HTTP/1.1 or HTTP/2; empty
    /// when it names none and the protocol is negotiated.
    pub http_version: String,
    /// The non-default TLS setup for this request's host: a private CA, a
    /// client certificate or no verification.
    pub tls: Option<TlsInfo>,
    /// The proxy the request goes through, when one is configured by
    /// flag, apic.yaml or the environment.
    pub proxy: Option<ProxyInfo>,
    /// The rendered spec (contains secrets).
    pub auth_spec: Option<apic_auth::Spec>,
    /// Names the headers whose value came from a secret source (private
    /// env file, .env, session or a capture).
    pub secret_headers: BTreeSet<String>,
    /// The parts of a multipart/form-data body, for renderers and the
    /// curl exporter; `body` then holds a one-line summary of them.
    pub parts: Vec<FormPart>,
    /// The assembled multipart body, sent as is.
    pub(crate) raw_body: Option<Vec<u8>>,
    pub(crate) missing: Vec<String>,
    /// Says a value from a secret source (private env file, .env, the
    /// session, a capture, --var) or a credential went into the request,
    /// so a file its response is saved to is created 0600.
    pub(crate) secret: bool,
}

/// One part of a resolved multipart/form-data body. `value` is the
/// rendered text of a text part; `file` is the path of a file part,
/// relative to the project root, as `apic curl` needs it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FormPart {
    pub name: String,
    pub filename: String,
    pub content_type: String,
    pub value: String,
    pub file: String,
    pub size: usize,
}

/// Masked in every output regardless of where their value came from.
pub(crate) const SENSITIVE_HEADERS: [&str; 7] = [
    "authorization",
    "proxy-authorization",
    "cookie",
    "x-api-key",
    "x-auth-token",
    "api-key",
    "x-amz-security-token",
];

pub(crate) fn is_sensitive_header(name: &str) -> bool {
    SENSITIVE_HEADERS.contains(&name.to_lowercase().as_str())
}

#[derive(Serialize)]
struct ResolvedView<'a> {
    #[serde(skip_serializing_if = "str::is_empty")]
    name: &'a str,
    file: &'a str,
    line: usize,
    method: &'a str,
    url: String,
    #[serde(skip_serializing_if = "str::is_empty")]
    auth: &'a str,
    #[serde(skip_serializing_if = "str::is_empty")]
    http_version: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    tls: &'a Option<TlsInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    proxy: &'a Option<ProxyInfo>,
    headers: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "String::is_empty")]
    body: String,
}

/// Emits JSON text that is already in its final form.
fn serialize_raw<S: serde::Serializer>(text: String, s: S) -> std::result::Result<S::Ok, S::Error> {
    RawValue::from_string(text)
        .map_err(serde::ser::Error::custom)?
        .serialize(s)
}

pub(crate) fn header_map(hs: &[Header]) -> BTreeMap<String, String> {
    hs.iter()
        .map(|h| (h.name.clone(), h.value.clone()))
        .collect()
}

impl Resolved {
    /// The bytes the request sends: the assembled multipart body when
    /// there is one, else the rendered body.
    pub fn body_bytes(&self) -> Vec<u8> {
        match &self.raw_body {
            Some(raw) => raw.clone(),
            None => self.body.clone().into_bytes(),
        }
    }

    /// The placeholders that have no value yet, in order of appearance.
    pub fn missing(&self) -> &[String] {
        &self.missing
    }

    /// The request headers with sensitive values masked; with `redact`
    /// set every value is masked.
    pub fn display_headers(&self, redact: bool) -> Vec<Header> {
        self.headers
            .iter()
            .map(|h| {
                let mut h = h.clone();
                if redact || is_sensitive_header(&h.name) || self.secret_headers.contains(&h.name) {
                    h.value = MASKED.to_string();
                }
                h
            })
            .collect()
    }

    /// The body, or the mask when redacting.
    pub fn display_body(&self, redact: bool) -> String {
        if redact && !self.body.is_empty() {
            return MASKED.to_string();
        }
        self.body.clone()
    }

    /// The URL with query values masked when redacting.
    pub fn display_url(&self, redact: bool) -> String {
        if !redact {
            return self.url.clone();
        }
        let Some((base, query)) = self.url.split_once('?') else {
            return self.url.clone();
        };
        let parts: Vec<String> = query
            .split('&')
            .map(|p| match p.split_once('=') {
                Some((k, _)) => format!("{k}={MASKED}"),
                None => p.to_string(),
            })
            .collect();
        format!("{base}?{}", parts.join("&"))
    }

    /// The request as JSON text, exactly as `--json` carries it: headers
    /// as an object, `<`, `>` and `&` escaped.
    pub fn to_json(&self, redact: bool) -> String {
        let view = ResolvedView {
            name: &self.name,
            file: &self.file,
            line: self.line,
            method: &self.method,
            url: self.display_url(redact),
            auth: &self.auth,
            http_version: &self.http_version,
            tls: &self.tls,
            proxy: &self.proxy,
            headers: header_map(&self.display_headers(redact)),
            body: self.display_body(redact),
        };
        escape_html(&serde_json::to_string(&view).unwrap_or_default())
    }
}

impl Serialize for Resolved {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        serialize_raw(self.to_json(false), s)
    }
}

/// The body of a [`Response`] as JSON shows it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Body {
    /// No body at all: `null`.
    #[default]
    None,
    /// The body is JSON: its text, written into the output as JSON
    /// (compacted) rather than as a string.
    Json(Arc<str>),
    /// The body as text; the base64 of the bytes when
    /// [`Response::body_encoding`] says so.
    Text(Arc<str>),
}

impl Body {
    /// A JSON body from its text.
    pub fn json(text: impl AsRef<str>) -> Body {
        Body::Json(Arc::from(text.as_ref()))
    }

    /// A text body.
    pub fn text(text: impl AsRef<str>) -> Body {
        Body::Text(Arc::from(text.as_ref()))
    }

    /// Reports whether the body is JSON.
    pub fn is_json(&self) -> bool {
        matches!(self, Body::Json(_))
    }

    /// The JSON text of a JSON body.
    pub fn as_json(&self) -> Option<&str> {
        match self {
            Body::Json(s) => Some(s),
            _ => None,
        }
    }

    /// The text of a text body.
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Body::Text(s) => Some(s),
            _ => None,
        }
    }

    /// Reports whether there is no body.
    pub fn is_none(&self) -> bool {
        matches!(self, Body::None)
    }
}

impl From<serde_json::Value> for Body {
    fn from(v: serde_json::Value) -> Body {
        match v {
            serde_json::Value::Null => Body::None,
            serde_json::Value::String(s) => Body::text(s),
            other => Body::json(other.to_string()),
        }
    }
}

impl Serialize for Body {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        match self {
            Body::None => s.serialize_unit(),
            Body::Json(text) => serialize_raw(compact_json(text, false), s),
            Body::Text(text) => s.serialize_str(text),
        }
    }
}

impl<'de> Deserialize<'de> for Body {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Body, D::Error> {
        let raw = Box::<RawValue>::deserialize(d)?;
        let text = raw.get().trim();
        if text == "null" {
            return Ok(Body::None);
        }
        if text.starts_with('"') {
            let s: String = serde_json::from_str(text).map_err(D::Error::custom)?;
            return Ok(Body::text(s));
        }
        Ok(Body::json(crate::stored::canonical_json(text)))
    }
}

/// The JSON-friendly view of a response.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Response {
    pub status: i64,
    pub status_text: String,
    pub headers: BTreeMap<String, String>,
    /// The JSON itself when the body is JSON, else a string.
    pub body: Body,
    /// [`BASE64`] when the body is not text (not valid UTF-8), in which
    /// case `body` is the base64 of the bytes; empty otherwise. Renderers
    /// read it to show a size instead of the bytes.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub body_encoding: String,
    /// Says `body` is the first [`Result::body_limit`] bytes of the
    /// response, as text, because the caller asked for a bounded body
    /// (`apic run --body-limit`); `size` is still the whole body's.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub body_truncated: bool,
    pub duration_ms: i64,
    pub size: usize,
    /// The protocol the response came over: "HTTP/1.1", "HTTP/2.0".
    #[serde(skip_serializing_if = "String::is_empty")]
    pub proto: String,
    /// Where the round trip went.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timings: Option<Timings>,
}

/// Breaks a round trip down: name resolution, the TCP connection, the TLS
/// handshake, the wait for the first response byte, and the total
/// including the body. A reused connection has no DNS, connect or TLS
/// time. Redirects and a digest challenge add their hops' times together.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Timings {
    pub dns_ms: i64,
    pub connect_ms: i64,
    pub tls_ms: i64,
    pub ttfb_ms: i64,
    pub total_ms: i64,
    pub reused: bool,
}

impl Timings {
    /// The client's timings in whole milliseconds.
    pub(crate) fn from_http(t: &apic_httpc::Timings) -> Timings {
        let ms = |d: std::time::Duration| d.as_millis() as i64;
        Timings {
            dns_ms: ms(t.dns),
            connect_ms: ms(t.connect),
            tls_ms: ms(t.tls),
            ttfb_ms: ms(t.ttfb),
            total_ms: ms(t.total),
            reused: t.reused,
        }
    }
}

/// Places a result in a data-driven run: which row of how many, and the
/// row's values (the variables it supplied).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Iteration {
    /// 1-based.
    pub index: usize,
    pub total: usize,
    pub row: BTreeMap<String, String>,
}

/// The outcome of running one request.
///
/// It serialises as `apic run --json` prints it: sensitive request and
/// response headers masked always, and everything (headers, bodies, query
/// values, captures and assertion values) when `redact` is set.
#[derive(Clone, Debug, Default)]
pub struct Result {
    pub ok: bool,
    pub request: Resolved,
    pub response: Option<Response>,
    pub captures: BTreeMap<String, String>,
    pub asserts: Vec<apic_assert::Result>,
    pub errors: Vec<String>,
    /// Problems that did not fail the request, such as a response history
    /// that could not be written.
    pub warnings: Vec<String>,
    /// The error that stopped the request, with its catalogue code, when
    /// one did; `errors` keeps the message too.
    pub error: Option<ErrorInfo>,
    /// Marks a result from [`crate::Runner::dry_run`]: the request as it
    /// would be sent, with no response because nothing was.
    pub dry_run: bool,
    /// Says the response history took this response, under `record_key`
    /// (see [`crate::history_key`]), so `apic select` can read it;
    /// otherwise `unrecorded` says why not. Read by the renderers.
    pub recorded: bool,
    pub record_key: String,
    pub unrecorded: String,
    /// When set, bounds the body the displays show (the JSON, --body-only
    /// and the report) to that many bytes, marking the response
    /// truncated. It is for a caller whose context the body goes into, an
    /// agent above all; the response history keeps the whole body, which
    /// `apic select` reads.
    pub body_limit: usize,
    /// How many times the request was sent under a `# @retry` policy (or
    /// --retry, or retry in apic.yaml); zero when none applied.
    pub attempts: usize,
    /// Where the response body was written, for a request with a `>>
    /// file` line or a run with --output: relative to the project root
    /// when inside it, else as given.
    pub saved_to: String,
    /// The results of the requests `# @ref` and `# @forceRef` ran first,
    /// in run order; a dependency's own dependencies nest under it.
    pub deps: Vec<Result>,
    /// Says why a flow did not send the request: "disabled" for `#
    /// @disabled`. A skipped result is OK and has no response.
    pub skipped: String,
    /// The row of `apic run --data` this result belongs to.
    pub iteration: Option<Iteration>,
    /// Set from the runner's `redact` option.
    pub redact: bool,
    pub(crate) raw: Option<Arc<apic_selector::Response>>,
    pub(crate) req: Option<Arc<Request>>,
    /// Says what the auth did on the wire beyond setting a header: for
    /// digest, whether a challenge was answered. Shown by run -v.
    pub(crate) auth_note: String,
}

#[derive(Serialize)]
struct ResultView<'a> {
    ok: bool,
    #[serde(skip_serializing_if = "<[String]>::is_empty")]
    errors: &'a [String],
    #[serde(skip_serializing_if = "<[String]>::is_empty")]
    warnings: &'a [String],
    #[serde(skip_serializing_if = "Option::is_none")]
    error: &'a Option<ErrorInfo>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    dry_run: bool,
    #[serde(skip_serializing_if = "is_zero")]
    attempts: usize,
    #[serde(skip_serializing_if = "str::is_empty")]
    saved_to: &'a str,
    #[serde(skip_serializing_if = "<[Result]>::is_empty")]
    ran_first: &'a [Result],
    #[serde(skip_serializing_if = "str::is_empty")]
    skipped: &'a str,
    request: Box<RawValue>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    captures: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response: Option<Response>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    asserts: Vec<apic_assert::Result>,
    #[serde(skip_serializing_if = "Option::is_none")]
    iteration: Option<Iteration>,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

impl Result {
    /// A result for `req`, as the runner builds them.
    pub(crate) fn for_request(req: &Arc<Request>) -> Result {
        Result {
            req: Some(req.clone()),
            ..Result::default()
        }
    }

    /// Reports what the request's auth did on the wire, for verbose
    /// output: "digest: 401 challenge answered (2 requests)" and the like.
    /// Empty for the header-setting types.
    pub fn auth_note(&self) -> &str {
        &self.auth_note
    }

    /// The request this result is for, so a caller holding results of
    /// dependencies (`deps`) can map them back to the requests that
    /// produced them. It is `None` for results built by hand.
    pub fn req(&self) -> Option<&Arc<Request>> {
        self.req.as_ref()
    }

    /// The underlying response for renderers.
    pub fn raw(&self) -> Option<&apic_selector::Response> {
        self.raw.as_deref()
    }

    /// Records a problem found after the response came back, such as a
    /// save that could not be written; the request then counts as failed.
    pub(crate) fn fail(&mut self, msg: impl Into<String>) {
        self.errors.push(msg.into());
        self.ok = false;
    }

    /// The captures, masked when redacting.
    pub fn display_captures(&self) -> BTreeMap<String, String> {
        if !self.redact {
            return self.captures.clone();
        }
        self.captures
            .keys()
            .map(|k| (k.clone(), MASKED.to_string()))
            .collect()
    }

    /// The one-line reason a result is not OK: the first failed assertion
    /// with what it got, else the first error, else "".
    pub fn problem(&self) -> String {
        for a in self.display_asserts() {
            if !a.error.is_empty() {
                return format!("{}: {}", a.expr, a.error);
            }
            if !a.pass {
                return format!("{}: got {}", a.expr, quote(&truncate(&a.actual, 40)));
            }
        }
        self.errors.first().cloned().unwrap_or_default()
    }

    /// The result as JSON text, exactly as `apic run --json` carries it:
    /// compact, `<`, `>` and `&` escaped. Serialising a result with
    /// `serde_json` gives the same text.
    pub fn to_json(&self) -> String {
        let mut iteration = self.iteration.clone();
        if let (Some(it), true) = (&mut iteration, self.redact) {
            // A row is data the caller supplied, which may hold secrets.
            for v in it.row.values_mut() {
                *v = MASKED.to_string();
            }
        }
        let request = RawValue::from_string(self.request.to_json(self.redact))
            .unwrap_or_else(|_| RawValue::from_string("null".to_string()).expect("valid JSON"));
        let view = ResultView {
            ok: self.ok,
            errors: &self.errors,
            warnings: &self.warnings,
            error: &self.error,
            dry_run: self.dry_run,
            attempts: self.attempts,
            saved_to: &self.saved_to,
            ran_first: &self.deps,
            skipped: &self.skipped,
            request,
            captures: self.display_captures(),
            response: self.display_response(),
            asserts: self.display_asserts(),
            iteration,
        };
        escape_html(&serde_json::to_string(&view).unwrap_or_default())
    }
}

impl Serialize for Result {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        serialize_raw(self.to_json(), s)
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n {
        return s.to_string();
    }
    let mut end = n;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}
