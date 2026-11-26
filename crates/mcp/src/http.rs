//! The streamable HTTP transport, for an agent that is not on the same
//! machine: a hosted agent, a shared development box, or a container that
//! also runs the API under test. stdio stays the default. The server
//! binds the loopback interface unless a bearer token guards it, because
//! an MCP server runs the project's requests with the project's
//! credentials and must not be reachable by whoever can route a packet to
//! the host.
//!
//! A client POSTs JSON-RPC messages. The first, `initialize`, opens a
//! session and is answered with its id in `Mcp-Session-Id`; later requests
//! carry that header. Calls are answered as a `text/event-stream` of
//! `message` events, notifications with 202. GET opens the session's
//! stream for messages from the server, and DELETE ends the session.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};

use apic_gocompat::{json_string, quote};
use apic_httpc::CancelToken;
use apic_httpd::{BodySender, Handler, Listener, Request, Response};
use apic_runner::Error;

use crate::protocol::{
    check_request, lock, read_batch, response, Id, Message, RpcError, Session,
    CODE_UNSUPPORTED_PROTOCOL_VERSION, DEFAULT_HTTP_VERSION, NO_BATCH_VERSION, SESSIONLESS_VERSION,
    SUPPORTED_PROTOCOL_VERSIONS,
};
use crate::{new, Config, Inner};

const SESSION_HEADER: &str = "Mcp-Session-Id";
const PROTOCOL_VERSION_HEADER: &str = "Mcp-Protocol-Version";
const LAST_EVENT_ID_HEADER: &str = "Last-Event-ID";
const META_PROTOCOL_VERSION: &str = "io.modelcontextprotocol/protocolVersion";

/// Splits `host:port`, taking the brackets off an IPv6 host.
fn split_host_port(addr: &str) -> Option<(&str, &str)> {
    if let Some(rest) = addr.strip_prefix('[') {
        let (host, port) = rest.split_once("]:")?;
        return Some((host, port));
    }
    let (host, port) = addr.rsplit_once(':')?;
    (!host.contains(':')).then_some((host, port))
}

fn is_loopback_host(host: &str) -> bool {
    host == "localhost"
        || host
            .trim_matches(['[', ']'])
            .parse::<IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// Reports whether an address, with or without a port, is this machine
/// talking to itself.
fn is_loopback(addr: &str) -> bool {
    match split_host_port(addr) {
        Some((host, _)) => is_loopback_host(host),
        None => is_loopback_host(addr),
    }
}

/// Refuses an address outside the loopback interface unless a token
/// guards the server.
pub fn check_bind(addr: &str, token: &str) -> Result<(), String> {
    let Some((host, _)) = split_host_port(addr) else {
        return Err(format!("--http: {} is not host:port", quote(addr)));
    };
    if !token.is_empty() || is_loopback_host(host) {
        return Ok(());
    }
    Err(format!(
        "--http {addr} binds outside the loopback interface; set --token (or APIC_MCP_TOKEN) so only your agent can call it"
    ))
}

/// Compares without stopping at the first difference, so the time taken
/// says nothing about where the two part.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// A session id: 26 characters of base32 over random bytes.
fn new_session_id() -> String {
    const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut bytes = [0u8; 26];
    if getrandom::getrandom(&mut bytes).is_err() {
        // No source of randomness: the clock and the address of a local
        // are all there is.
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        for (i, b) in bytes.iter_mut().enumerate() {
            *b = (seed >> (i % 16 * 8)) as u8 ^ (i as u8).wrapping_mul(31);
        }
    }
    bytes
        .iter()
        .map(|b| ALPHABET[(b & 31) as usize] as char)
        .collect()
}

struct HttpSession {
    session: Session,
    /// The stream a GET opened for messages from the server, while its
    /// client is still there.
    standalone: Mutex<Option<BodySender>>,
}

struct Transport {
    inner: Arc<Inner>,
    sessions: Mutex<HashMap<String, Arc<HttpSession>>>,
}

fn sse_headers(resp: Response) -> Response {
    resp.with_header("Cache-Control", "no-cache, no-transform")
        .with_header("Content-Type", "text/event-stream")
        .with_header("Connection", "keep-alive")
}

/// The media types an Accept header takes: JSON, an event stream.
fn accepts(req: &Request) -> (bool, bool) {
    let (mut json, mut stream) = (false, false);
    for value in req.headers.values("Accept") {
        for raw in value.split(',') {
            let base = raw
                .split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase();
            match base.as_str() {
                "application/json" | "application/*" => json = true,
                "text/event-stream" | "text/*" => stream = true,
                "*/*" => {
                    json = true;
                    stream = true;
                }
                _ => {}
            }
        }
    }
    (json, stream)
}

fn json_rpc_error(status: u16, id: Option<&Id>, err: RpcError) -> Response {
    let body = match id {
        Some(id) => response(id, Err(err)),
        None => response(&Id::Int(0), Err(err)).replacen("\"id\":0,", "", 1),
    };
    Response::new(status)
        .with_header("Content-Type", "application/json")
        .with_body(body)
}

impl Transport {
    fn lookup(&self, id: &str) -> Result<Arc<HttpSession>, Response> {
        lock(&self.sessions)
            .get(id)
            .cloned()
            .ok_or_else(|| Response::error(404, "session not found"))
    }

    fn serve(&self, req: &Request) -> Response {
        // A page in a browser can reach a server on this machine by
        // pointing a name it controls at 127.0.0.1; the Host header gives
        // it away.
        if is_loopback(&req.remote_addr) && !is_loopback(&req.host) {
            return Response::error(
                403,
                &format!("Forbidden: invalid Host header {}", quote(&req.host)),
            );
        }
        let version = req.header(PROTOCOL_VERSION_HEADER).unwrap_or("");
        if !version.is_empty()
            && !SUPPORTED_PROTOCOL_VERSIONS.contains(&version)
            && version < SESSIONLESS_VERSION
        {
            return Response::error(
                400,
                &format!(
                    "Bad Request: Unsupported protocol version (supported versions: {})",
                    SUPPORTED_PROTOCOL_VERSIONS.join(",")
                ),
            );
        }
        match req.method.as_str() {
            "GET" => self.get(req),
            "POST" => self.post(req, version),
            "DELETE" => self.delete(req),
            _ => {
                Response::error(405, "Method Not Allowed").with_header("Allow", "GET, POST, DELETE")
            }
        }
    }

    fn get(&self, req: &Request) -> Response {
        if !accepts(req).1 {
            return Response::error(
                400,
                "Accept must contain 'text/event-stream' for GET requests",
            );
        }
        let id = req.header(SESSION_HEADER).unwrap_or("");
        if id.is_empty() {
            return Response::error(400, "Bad Request: GET requires an Mcp-Session-Id header");
        }
        let session = match self.lookup(id) {
            Ok(session) => session,
            Err(resp) => return resp,
        };
        if let Some(eid) = req.header(LAST_EVENT_ID_HEADER) {
            let well_formed = matches!(
                eid.split('_').collect::<Vec<_>>().as_slice(),
                [_, idx] if idx.parse::<u32>().is_ok()
            );
            if !well_formed {
                return Response::error(400, &format!("malformed Last-Event-ID {}", quote(eid)));
            }
            return Response::error(400, "stream replay unsupported");
        }
        let mut standalone = lock(&session.standalone);
        if standalone.as_ref().is_some_and(|s| !s.is_closed()) {
            return Response::error(409, "stream ID conflicts with ongoing stream");
        }
        // The stream stays open until its client goes or the session
        // ends; the server has nothing of its own to say on it.
        let (resp, sender) = Response::streaming(200);
        sender.send(": ok\n\n");
        *standalone = Some(sender);
        sse_headers(resp)
    }

    fn delete(&self, req: &Request) -> Response {
        let id = req.header(SESSION_HEADER).unwrap_or("");
        if id.is_empty() {
            return Response::error(400, "Bad Request: DELETE requires an Mcp-Session-Id header");
        }
        let removed = lock(&self.sessions).remove(id);
        let Some(session) = removed else {
            return Response::error(404, "session not found");
        };
        session.session.close();
        lock(&session.standalone).take();
        Response::new(204)
    }

    fn post(&self, req: &Request, version: &str) -> Response {
        let content_type = req.header("Content-Type").unwrap_or("");
        let media = content_type.split(';').next().unwrap_or("").trim();
        if !media.eq_ignore_ascii_case("application/json") {
            return Response::error(415, "Content-Type must be 'application/json'");
        }
        let (json_ok, stream_ok) = accepts(req);
        if !json_ok || !stream_ok {
            return Response::error(
                400,
                "Accept must contain both 'application/json' and 'text/event-stream'",
            );
        }
        let id = req.header(SESSION_HEADER).unwrap_or("");
        if !id.is_empty() {
            return match self.lookup(id) {
                Ok(session) => self.post_to(&session, id, req, version),
                Err(resp) => resp,
            };
        }
        // No session yet: this request opens one, which lasts only if the
        // request is the `initialize` it has to start with.
        let id = new_session_id();
        let session = Arc::new(HttpSession {
            session: Session::new(self.inner.clone()),
            standalone: Mutex::new(None),
        });
        lock(&self.sessions).insert(id.clone(), session.clone());
        let resp = self.post_to(&session, &id, req, version);
        if !session.session.initialized() {
            session.session.close();
            lock(&self.sessions).remove(&id);
        }
        resp
    }

    fn post_to(
        &self,
        session: &HttpSession,
        id: &str,
        req: &Request,
        header_version: &str,
    ) -> Response {
        if req.header(LAST_EVENT_ID_HEADER).is_some() {
            return Response::error(400, "can't send Last-Event-ID for POST request");
        }
        if req.body.is_empty() {
            return Response::error(400, "POST requires a non-empty body");
        }
        let (incoming, is_batch) = match read_batch(&req.text()) {
            Ok(read) => read,
            Err(err) => return Response::error(400, &format!("malformed payload: {err}")),
        };
        let version = if header_version.is_empty() {
            DEFAULT_HTTP_VERSION
        } else {
            header_version
        };
        if is_batch && version >= NO_BATCH_VERSION {
            return Response::error(
                400,
                &format!(
                    "JSON-RPC batching is not supported in {NO_BATCH_VERSION} and later (request version: {version})"
                ),
            );
        }
        let mut calls = 0;
        let mut is_initialize = false;
        for msg in &incoming {
            let Message::Request {
                id: call_id,
                method,
                params,
            } = msg
            else {
                continue;
            };
            if let Err(err) = check_request(method, call_id.is_some(), params.is_some()) {
                return Response::error(400, &err.message);
            }
            is_initialize |= method == "initialize";
            let meta_version = params
                .as_ref()
                .and_then(|p| serde_json::from_str::<serde_json::Value>(p.get()).ok())
                .and_then(|v| {
                    v.get("_meta")?
                        .get(META_PROTOCOL_VERSION)?
                        .as_str()
                        .map(str::to_string)
                })
                .unwrap_or_default();
            if (call_id.is_some() && version >= SESSIONLESS_VERSION) || !meta_version.is_empty() {
                // That protocol has no sessions, and this server keeps
                // one per client.
                let legacy: Vec<String> = SUPPORTED_PROTOCOL_VERSIONS
                    .iter()
                    .filter(|v| **v < SESSIONLESS_VERSION)
                    .map(|v| format!("\"{v}\""))
                    .collect();
                let err = RpcError {
                    code: CODE_UNSUPPORTED_PROTOCOL_VERSION,
                    message: format!(
                        "protocol version {} is not supported by this server",
                        quote(version)
                    ),
                    data: Some(format!(
                        "{{\"supported\":[{}],\"requested\":{}}}",
                        legacy.join(","),
                        json_string(version, false)
                    )),
                };
                return json_rpc_error(400, call_id.as_ref(), err);
            }
            if call_id.is_some() {
                calls += 1;
            }
        }
        if calls == 0 {
            for msg in &incoming {
                session.session.handle(msg);
            }
            return Response::new(202);
        }
        let (mut resp, sender) = Response::streaming(200);
        resp = sse_headers(resp);
        if is_initialize {
            resp = resp.with_header(SESSION_HEADER, id);
        }
        // Each response goes out as its call finishes; the stream ends
        // with the last.
        for msg in &incoming {
            if let Some(line) = session.session.handle(msg) {
                sender.send(format!("event: message\ndata: {line}\n\n"));
            }
        }
        resp
    }
}

/// Serves the project over the MCP streamable HTTP transport, behind a
/// bearer token when one is given.
pub fn handler(cfg: Config, token: &str) -> Result<Handler, Error> {
    let server = new(cfg)?;
    let transport = Transport {
        inner: server.inner,
        sessions: Mutex::new(HashMap::new()),
    };
    let token = token.to_string();
    Ok(Box::new(move |req: &Request| {
        if !token.is_empty() {
            // Every request without the bearer token is answered 401.
            let given = req
                .header("Authorization")
                .and_then(|h| h.strip_prefix("Bearer "))
                .map(str::trim);
            if !given.is_some_and(|g| constant_time_eq(g.as_bytes(), token.as_bytes())) {
                return Response::error(
                    401,
                    "unauthorized: this apic mcp server needs its bearer token",
                )
                .with_header("WWW-Authenticate", "Bearer realm=\"apic mcp\"");
            }
        }
        transport.serve(req)
    }))
}

/// Listens on `addr` and serves until `cancel` is cancelled. `ready`,
/// when set, is called with the bound address once the listener is up.
pub fn serve_http(
    cancel: &CancelToken,
    cfg: Config,
    addr: &str,
    token: &str,
    ready: Option<&dyn Fn(&str)>,
) -> Result<(), Error> {
    check_bind(addr, token).map_err(Error::Other)?;
    let h = handler(cfg, token)?;
    let listener = Listener::bind(addr).map_err(|e| Error::Other(e.to_string()))?;
    if let Some(ready) = ready {
        ready(&listener.addr().to_string());
    }
    apic_httpd::serve(listener, h, cancel);
    Ok(())
}
