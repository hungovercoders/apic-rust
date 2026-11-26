//! JSON-RPC 2.0 messages and the protocol's Content-Length framing.

use std::fmt;
use std::io::{self, BufRead, Read, Write};
use std::sync::Mutex;

use apic_gocompat::{check_json, compact_json, escape_html, io_error_text, json_string, quote};
use serde_json::value::RawValue;

use crate::protocol::{Mismatch, Node, Obj};

/// Any JSON-RPC 2.0 message: a request (`id` and `method`), a
/// notification (`method` only) or a response (`id` and `result` or
/// `error`). `id`, `params` and `result` are JSON text, kept as it was
/// sent so an id is answered exactly as the client wrote it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Message {
    pub id: Option<String>,
    pub method: String,
    pub params: Option<String>,
    pub result: Option<String>,
    pub error: Option<RpcError>,
}

/// A JSON-RPC error object.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct RpcError {
    pub code: i64,
    pub message: String,
}

impl RpcError {
    pub fn new(code: i64, message: impl Into<String>) -> RpcError {
        RpcError {
            code,
            message: message.into(),
        }
    }

    /// The error for a request that failed for a reason of its own (a
    /// project that does not load, a target that names nothing).
    pub fn failed(message: impl fmt::Display) -> RpcError {
        RpcError::new(CODE_REQUEST_FAILED, message.to_string())
    }
}

impl fmt::Display for RpcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.message, self.code)
    }
}

impl std::error::Error for RpcError {}

// JSON-RPC and LSP error codes.
pub(crate) const CODE_PARSE_ERROR: i64 = -32700;
pub(crate) const CODE_INVALID_REQUEST: i64 = -32600;
pub(crate) const CODE_METHOD_NOT_FOUND: i64 = -32601;
pub(crate) const CODE_INVALID_PARAMS: i64 = -32602;
#[allow(dead_code)]
pub(crate) const CODE_INTERNAL_ERROR: i64 = -32603;
pub(crate) const CODE_NOT_INITIALIZED: i64 = -32002;
pub(crate) const CODE_REQUEST_FAILED: i64 = -32803;

/// Bounds one message, so a client that sends a nonsense Content-Length
/// cannot make the server allocate without limit.
pub(crate) const MAX_MESSAGE: i64 = 64 << 20;

/// Why a message could not be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ReadError {
    /// A clean end of the stream.
    Eof,
    /// A message that arrived whole but is not JSON-RPC: the connection
    /// is still good, and the message is answered with this error.
    Rpc(RpcError),
    /// The connection is gone or the framing is broken.
    Other(String),
}

impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReadError::Eof => f.write_str("EOF"),
            ReadError::Rpc(e) => e.fmt(f),
            ReadError::Other(msg) => f.write_str(msg),
        }
    }
}

/// How reading a header block ended early.
enum HeaderError {
    Eof,
    Other(String),
}

fn io_text(err: &io::Error) -> String {
    if err.kind() == io::ErrorKind::UnexpectedEof {
        return "unexpected EOF".to_string();
    }
    io_error_text(err)
}

/// Reads one line without its line ending; `None` at the end of the
/// stream. A last line with no line ending is still a line.
fn read_line(r: &mut impl BufRead) -> Result<Option<Vec<u8>>, HeaderError> {
    let mut line = Vec::new();
    let n = r
        .read_until(b'\n', &mut line)
        .map_err(|e| HeaderError::Other(io_text(&e)))?;
    if n == 0 {
        return Ok(None);
    }
    if line.last() == Some(&b'\n') {
        line.pop();
        if line.last() == Some(&b'\r') {
            line.pop();
        }
    }
    Ok(Some(line))
}

fn peek(r: &mut impl BufRead) -> Option<u8> {
    r.fill_buf().ok().and_then(|b| b.first().copied())
}

fn trim(line: &[u8]) -> &[u8] {
    let blank = |b: &u8| *b == b' ' || *b == b'\t';
    let start = line.iter().position(|b| !blank(b)).unwrap_or(line.len());
    let end = line
        .iter()
        .rposition(|b| !blank(b))
        .map_or(start, |i| i + 1);
    &line[start..end]
}

/// Whether a byte may appear in a header's name.
fn token_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b)
}

/// Reads a block of `Name: value` headers up to its blank line, as mail
/// and HTTP write them: a line that starts with a space or a tab
/// continues the one before. The headers read so far come back with the
/// error.
fn read_headers(r: &mut impl BufRead) -> (Vec<(String, String)>, Option<HeaderError>) {
    let mut headers: Vec<(String, String)> = Vec::new();
    if matches!(peek(r), Some(b' ' | b'\t')) {
        let line = match read_line(r) {
            Ok(line) => line.unwrap_or_default(),
            Err(err) => return (headers, Some(err)),
        };
        let msg = format!(
            "malformed MIME header initial line: {}",
            quote(&String::from_utf8_lossy(&line))
        );
        return (headers, Some(HeaderError::Other(msg)));
    }
    loop {
        let line = match read_line(r) {
            Ok(Some(line)) => line,
            Ok(None) => return (headers, Some(HeaderError::Eof)),
            Err(err) => return (headers, Some(err)),
        };
        if line.is_empty() {
            return (headers, None);
        }
        if !line.contains(&b':') {
            let msg = format!(
                "malformed MIME header: missing colon: {}",
                quote(&String::from_utf8_lossy(&line))
            );
            return (headers, Some(HeaderError::Other(msg)));
        }
        let mut kv = trim(&line).to_vec();
        let mut ended = None;
        while matches!(peek(r), Some(b' ' | b'\t')) {
            match read_line(r) {
                Ok(Some(more)) => {
                    kv.push(b' ');
                    kv.extend_from_slice(trim(&more));
                }
                Ok(None) => break,
                Err(err) => {
                    ended = Some(err);
                    break;
                }
            }
        }
        if kv.is_empty() {
            return (headers, ended);
        }
        let colon = kv.iter().position(|b| *b == b':').unwrap_or(kv.len());
        let (key, value) = (&kv[..colon], &kv[(colon + 1).min(kv.len())..]);
        // A name with a space before the colon is let through, as mail
        // readers let it; it then matches no header anybody asks for.
        let named = !key.is_empty() && key.iter().all(|b| token_byte(*b) || *b == b' ');
        let printable = value
            .iter()
            .all(|b| *b == b'\t' || (*b >= 0x20 && *b != 0x7f));
        if !named || !printable {
            let msg = format!(
                "malformed MIME header line: {}",
                quote(&String::from_utf8_lossy(&kv))
            );
            return (headers, Some(HeaderError::Other(msg)));
        }
        let start = value
            .iter()
            .position(|b| *b != b' ' && *b != b'\t')
            .unwrap_or(value.len());
        headers.push((
            String::from_utf8_lossy(key).into_owned(),
            String::from_utf8_lossy(&value[start..]).into_owned(),
        ));
        if ended.is_some() {
            return (headers, ended);
        }
    }
}

/// Reads exactly `n` bytes. The error is `None` when the stream ended
/// before the first of them.
fn read_full(r: &mut impl Read, n: usize) -> Result<Vec<u8>, Option<String>> {
    let mut body = vec![0u8; n];
    let mut got = 0;
    while got < n {
        match r.read(&mut body[got..]) {
            Ok(0) if got == 0 => return Err(None),
            Ok(0) => return Err(Some("unexpected EOF".to_string())),
            Ok(k) => got += k,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(Some(io_text(&e))),
        }
    }
    Ok(body)
}

/// Returns the next message of a stream.
pub(crate) fn read_message(r: &mut impl BufRead) -> Result<Message, ReadError> {
    let (headers, err) = read_headers(r);
    match err {
        // The stream ended, between two messages or inside a header: the
        // client is gone either way.
        Some(HeaderError::Eof) => return Err(ReadError::Eof),
        Some(HeaderError::Other(msg)) => {
            return Err(ReadError::Other(format!("reading message header: {msg}")))
        }
        None => {}
    }
    let length = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("Content-Length"))
        .map_or("", |(_, v)| v.as_str());
    let n = match length.trim().parse::<i64>() {
        Ok(n) if (0..=MAX_MESSAGE).contains(&n) => n as usize,
        _ => {
            return Err(ReadError::Other(format!(
                "bad Content-Length {}",
                quote(length)
            )))
        }
    };
    let body = read_full(r, n).map_err(|e| match e {
        Some(e) => ReadError::Other(format!("reading message body: {e}")),
        None => ReadError::Eof,
    })?;
    parse_message(&body).map_err(|msg| ReadError::Rpc(RpcError::new(CODE_PARSE_ERROR, msg)))
}

/// Decodes the body of a message; the error is what the sender is told.
fn parse_message(body: &[u8]) -> Result<Message, String> {
    check_json(body)?;
    let text = String::from_utf8_lossy(body);
    let root = Node::parse(&text)?;
    let err = Mismatch::default();
    let o = Obj::root(&root, "lsp.message", "message", &err);
    if root.is_null() || err.borrow().is_some() {
        return match err.into_inner() {
            Some((_, msg)) => Err(msg),
            None => Ok(Message::default()),
        };
    }
    let fields: std::collections::BTreeMap<String, Box<RawValue>> =
        serde_json::from_str(&text).map_err(|e| e.to_string())?;
    // A key is matched as written, else without regard to case.
    let field = |name: &str| -> Option<&str> {
        fields
            .get(name)
            .or_else(|| {
                fields
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case(name))
                    .map(|(_, v)| v)
            })
            .map(|v| v.get())
    };
    let _ = o.string("jsonrpc");
    let mut m = Message {
        id: field("id").filter(|t| *t != "null").map(str::to_string),
        method: o.string("method"),
        params: field("params").map(str::to_string),
        result: field("result").filter(|t| *t != "null").map(str::to_string),
        error: None,
    };
    if o.has("error") {
        let e = o.object("error", "rpcError", "lsp.rpcError");
        m.error = Some(RpcError {
            code: e.int("code"),
            message: e.string("message"),
        });
    }
    if let Some((_, msg)) = err.into_inner() {
        return Err(msg);
    }
    Ok(m)
}

/// The JSON text of a message, as it goes on the wire: `<`, `>` and `&`
/// escaped, an id as the client wrote it.
fn encode(m: &Message) -> String {
    let mut b = String::from("{\"jsonrpc\":\"2.0\"");
    if let Some(id) = &m.id {
        b.push_str(",\"id\":");
        b.push_str(&compact_json(id, true));
    }
    if !m.method.is_empty() {
        b.push_str(",\"method\":");
        b.push_str(&json_string(&m.method, true));
    }
    if let Some(params) = &m.params {
        b.push_str(",\"params\":");
        b.push_str(&compact_json(params, true));
    }
    if let Some(result) = &m.result {
        b.push_str(",\"result\":");
        b.push_str(&compact_json(result, true));
    }
    if let Some(e) = &m.error {
        b.push_str(&format!(
            ",\"error\":{{\"code\":{},\"message\":{}}}",
            e.code,
            json_string(&e.message, true)
        ));
    }
    b.push('}');
    escape_html(&b)
}

/// The JSON text of a value a reply or a notification carries.
pub(crate) fn to_json<T: serde::Serialize + ?Sized>(v: &T) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "null".to_string())
}

/// Writes framed messages. Writes are serialised, since replies and
/// notifications come from more than one thread.
pub(crate) struct Conn {
    w: Mutex<Box<dyn Write + Send>>,
}

impl Conn {
    pub fn new(w: impl Write + Send + 'static) -> Conn {
        Conn {
            w: Mutex::new(Box::new(w)),
        }
    }

    /// Sends one message.
    pub fn write(&self, m: &Message) -> io::Result<()> {
        let body = encode(m);
        let mut w = self.w.lock().unwrap_or_else(|e| e.into_inner());
        write!(w, "Content-Length: {}\r\n\r\n", body.len())?;
        w.write_all(body.as_bytes())?;
        w.flush()
    }

    /// Answers a request. No result is sent as JSON null, which the
    /// protocol requires for "no result" (no hover, no edits).
    pub fn reply(
        &self,
        id: Option<&str>,
        result: std::result::Result<Option<String>, RpcError>,
    ) -> io::Result<()> {
        // A message too broken to have an id is answered with "id": null.
        let mut m = Message {
            id: Some(id.unwrap_or("null").to_string()),
            ..Message::default()
        };
        match result {
            Err(e) => m.error = Some(e),
            Ok(None) => m.result = Some("null".to_string()),
            Ok(Some(json)) => m.result = Some(json),
        }
        self.write(&m)
    }

    /// Sends a notification to the client.
    pub fn notify<T: serde::Serialize + ?Sized>(&self, method: &str, params: &T) -> io::Result<()> {
        self.write(&Message {
            method: method.to_string(),
            params: Some(to_json(params)),
            ..Message::default()
        })
    }
}
