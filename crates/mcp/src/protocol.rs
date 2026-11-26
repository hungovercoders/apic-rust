//! JSON-RPC 2.0 as MCP uses it: reading messages, the session's lifecycle
//! (initialize, then everything else), and the methods the server answers.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use apic_gocompat::{json_string, quote};
use apic_httpc::CancelToken;
use serde::Deserialize;
use serde_json::value::RawValue;
use serde_json::Value;

use crate::tools::{call_tool, tools_list};
use crate::Inner;

/// The protocol versions the server knows, newest first.
pub(crate) const SUPPORTED_PROTOCOL_VERSIONS: [&str; 5] = [
    "2026-07-28",
    "2025-11-25",
    "2025-06-18",
    "2025-03-26",
    "2024-11-05",
];
/// The first version that has no `initialize`: a session cannot be on it.
pub(crate) const SESSIONLESS_VERSION: &str = "2026-07-28";
/// The first version without JSON-RPC batches.
pub(crate) const NO_BATCH_VERSION: &str = "2025-06-18";
/// The version assumed when an HTTP request names none.
pub(crate) const DEFAULT_HTTP_VERSION: &str = "2025-03-26";

pub(crate) const CODE_INVALID_REQUEST: i64 = -32600;
pub(crate) const CODE_METHOD_NOT_FOUND: i64 = -32601;
pub(crate) const CODE_INVALID_PARAMS: i64 = -32602;
pub(crate) const CODE_UNSUPPORTED_PROTOCOL_VERSION: i64 = -32022;

pub(crate) fn lock<T: ?Sized>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A request id: a number or a string.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum Id {
    Int(i64),
    Str(String),
}

impl Id {
    pub(crate) fn json(&self) -> String {
        match self {
            Id::Int(n) => n.to_string(),
            Id::Str(s) => json_string(s, false),
        }
    }

    fn from_value(v: &Value) -> Option<Id> {
        match v {
            Value::Number(n) => Some(Id::Int(
                n.as_i64()
                    .unwrap_or_else(|| n.as_f64().unwrap_or(0.0) as i64),
            )),
            Value::String(s) => Some(Id::Str(s.clone())),
            _ => None,
        }
    }
}

/// One incoming message.
#[derive(Debug)]
pub(crate) enum Message {
    /// A call when it has an id, a notification when it has none.
    Request {
        id: Option<Id>,
        method: String,
        params: Option<Box<RawValue>>,
    },
    /// An answer to something the server asked; it asks nothing, so these
    /// are read and dropped.
    Response,
}

impl Message {
    pub(crate) fn call_id(&self) -> Option<&Id> {
        match self {
            Message::Request { id, .. } => id.as_ref(),
            Message::Response => None,
        }
    }
}

#[derive(Deserialize)]
struct Wire {
    #[serde(default)]
    jsonrpc: String,
    #[serde(default)]
    id: Value,
    #[serde(default, deserialize_with = "raw_present")]
    method: Option<Box<RawValue>>,
    #[serde(default, deserialize_with = "raw_present")]
    params: Option<Box<RawValue>>,
}

/// Keeps a member that is present as it was written, `null` included, so
/// "not given" and "given as null" stay apart.
fn raw_present<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Box<RawValue>>, D::Error> {
    Box::<RawValue>::deserialize(d).map(Some)
}

/// Reads one message. The error is what ends a stdio session, or what an
/// HTTP request is refused with.
pub(crate) fn decode_message(data: &str) -> Result<Message, String> {
    let wire: Wire =
        serde_json::from_str(data).map_err(|e| format!("unmarshaling jsonrpc message: {e}"))?;
    if wire.jsonrpc != "2.0" {
        return Err(format!(
            "invalid message version tag {}; expected \"2.0\"",
            quote(&wire.jsonrpc)
        ));
    }
    let id = match &wire.id {
        Value::Null => None,
        v => Some(Id::from_value(v).ok_or_else(|| {
            let kind = match v {
                Value::Bool(_) => "bool",
                Value::Array(_) => "[]interface {}",
                _ => "map[string]interface {}",
            };
            format!("parse error: invalid ID type {kind}")
        })?),
    };
    if let Some(method) = &wire.method {
        let method: Option<String> = serde_json::from_str(method.get())
            .map_err(|e| format!("unmarshaling jsonrpc message: {e}"))?;
        return Ok(Message::Request {
            id,
            method: method.unwrap_or_default(),
            params: wire.params,
        });
    }
    if id.is_none() {
        return Err("invalid request".to_string());
    }
    Ok(Message::Response)
}

/// Reads a payload that is one message or a batch of them, and says which.
pub(crate) fn read_batch(data: &str) -> Result<(Vec<Message>, bool), String> {
    if let Ok(raw) = serde_json::from_str::<Vec<Box<RawValue>>>(data) {
        if raw.is_empty() {
            return Err("empty batch".to_string());
        }
        let msgs = raw
            .iter()
            .map(|m| decode_message(m.get()))
            .collect::<Result<Vec<_>, _>>()?;
        return Ok((msgs, true));
    }
    Ok((vec![decode_message(data)?], false))
}

/// A JSON-RPC error. Code 0 is an error that has no code of its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RpcError {
    pub(crate) code: i64,
    pub(crate) message: String,
    /// Extra detail, as JSON text.
    pub(crate) data: Option<String>,
}

impl RpcError {
    pub(crate) fn new(code: i64, message: impl Into<String>) -> RpcError {
        RpcError {
            code,
            message: message.into(),
            data: None,
        }
    }

    fn plain(message: impl Into<String>) -> RpcError {
        RpcError::new(0, message)
    }

    fn json(&self) -> String {
        let mut out = format!(
            "{{\"code\":{},\"message\":{}",
            self.code,
            json_string(&self.message, false)
        );
        if let Some(data) = &self.data {
            out.push_str(&format!(",\"data\":{data}"));
        }
        out.push('}');
        out
    }
}

/// The response to a call, as one line of JSON.
pub(crate) fn response(id: &Id, result: Result<String, RpcError>) -> String {
    match result {
        Ok(result) => format!(
            "{{\"jsonrpc\":\"2.0\",\"id\":{},\"result\":{result}}}",
            id.json()
        ),
        Err(err) => format!(
            "{{\"jsonrpc\":\"2.0\",\"id\":{},\"error\":{}}}",
            id.json(),
            err.json()
        ),
    }
}

/// What a method takes and whether it is a call or a notification.
struct MethodInfo {
    notification: bool,
    missing_params_ok: bool,
    /// The name of the parameters, as an error about them reads.
    params_type: &'static str,
}

fn method_info(method: &str) -> Option<MethodInfo> {
    let call = |missing_params_ok, params_type| {
        Some(MethodInfo {
            notification: false,
            missing_params_ok,
            params_type,
        })
    };
    let notification = |missing_params_ok, params_type| {
        Some(MethodInfo {
            notification: true,
            missing_params_ok,
            params_type,
        })
    };
    match method {
        "completion/complete" => call(false, "CompleteParams"),
        "initialize" => call(false, "initializeParamsV2"),
        "ping" => call(true, "PingParams"),
        "prompts/list" => call(true, "ListPromptsParams"),
        "prompts/get" => call(false, "GetPromptParams"),
        "tools/list" => call(true, "ListToolsParams"),
        "tools/call" => call(false, "CallToolParamsRaw"),
        "resources/list" => call(true, "ListResourcesParams"),
        "resources/templates/list" => call(true, "ListResourceTemplatesParams"),
        "resources/read" => call(false, "ReadResourceParams"),
        "logging/setLevel" => call(false, "SetLoggingLevelParams"),
        "resources/subscribe" => call(false, "SubscribeParams"),
        "resources/unsubscribe" => call(false, "UnsubscribeParams"),
        "notifications/cancelled" => notification(true, "CancelledParams"),
        "notifications/initialized" => notification(true, "InitializedParams"),
        "notifications/roots/list_changed" => notification(true, "RootsListChangedParams"),
        "notifications/progress" => notification(false, "ProgressNotificationParams"),
        _ => None,
    }
}

/// Checks a request against the methods the server knows: the right kind
/// (call or notification) and parameters where they are required. The
/// HTTP transport refuses a request that fails this before running it.
pub(crate) fn check_request(method: &str, is_call: bool, has_params: bool) -> Result<(), RpcError> {
    let Some(info) = method_info(method) else {
        return Err(RpcError::new(
            CODE_METHOD_NOT_FOUND,
            format!("JSON RPC not handled: {} unsupported", quote(method)),
        ));
    };
    if info.notification && is_call {
        return Err(RpcError::new(
            CODE_INVALID_REQUEST,
            format!("invalid request: unexpected id for {}", quote(method)),
        ));
    }
    if !info.notification && !is_call {
        return Err(RpcError::new(
            CODE_INVALID_REQUEST,
            format!("invalid request: missing id for {}", quote(method)),
        ));
    }
    if !info.missing_params_ok && !has_params {
        return Err(RpcError::new(
            CODE_INVALID_REQUEST,
            "invalid request: missing required \"params\"",
        ));
    }
    Ok(())
}

#[derive(Default)]
struct State {
    /// `initialize` has been answered.
    initialized: bool,
    /// The version agreed in `initialize`.
    negotiated: String,
}

/// One client's session: where it is in the lifecycle and which of its
/// calls are running.
pub(crate) struct Session {
    inner: Arc<Inner>,
    state: Mutex<State>,
    in_flight: Mutex<HashMap<Id, CancelToken>>,
    /// Cancelled when the session ends.
    pub(crate) closed: CancelToken,
}

/// The version a session runs on: the client's when the server knows it,
/// else the newest the server has that still has sessions.
fn negotiated_version(client: &str) -> &'static str {
    SUPPORTED_PROTOCOL_VERSIONS
        .iter()
        .find(|v| **v == client && client < SESSIONLESS_VERSION)
        .copied()
        .unwrap_or("2025-11-25")
}

fn list_result(key: &str, items: &[String]) -> String {
    format!(
        "{{\"ttlMs\":0,\"cacheScope\":\"public\",\"{key}\":[{}]}}",
        items.join(",")
    )
}

impl Session {
    pub(crate) fn new(inner: Arc<Inner>) -> Session {
        Session {
            inner,
            state: Mutex::new(State::default()),
            in_flight: Mutex::new(HashMap::new()),
            closed: CancelToken::new(),
        }
    }

    /// Reports whether `initialize` has been answered.
    pub(crate) fn initialized(&self) -> bool {
        lock(&self.state).initialized
    }

    /// The version agreed in `initialize`; empty before it.
    pub(crate) fn protocol_version(&self) -> String {
        lock(&self.state).negotiated.clone()
    }

    /// Ends the session: every call still running is cancelled.
    pub(crate) fn close(&self) {
        self.closed.cancel();
        for token in lock(&self.in_flight).values() {
            token.cancel();
        }
    }

    /// Handles one message and returns the response line, if it gets one.
    pub(crate) fn handle(&self, msg: &Message) -> Option<String> {
        let Message::Request { id, method, params } = msg else {
            return None;
        };
        let params = params.as_deref();
        if method == "notifications/cancelled" && id.is_none() {
            self.cancel_call(params);
            return None;
        }
        let Some(id) = id else {
            // A notification is never answered, whatever comes of it.
            let _ = self.dispatch(method, false, params, &CancelToken::new());
            return None;
        };
        let cancel = CancelToken::new();
        lock(&self.in_flight).insert(id.clone(), cancel.clone());
        if self.closed.is_cancelled() {
            cancel.cancel();
        }
        let result = self.dispatch(method, true, params, &cancel);
        lock(&self.in_flight).remove(id);
        Some(response(id, result))
    }

    /// Stops the call a `notifications/cancelled` names.
    fn cancel_call(&self, params: Option<&RawValue>) {
        let request_id = params
            .and_then(|p| serde_json::from_str::<Value>(p.get()).ok())
            .and_then(|v| v.get("requestId").and_then(Id::from_value));
        if let Some(id) = request_id {
            if let Some(token) = lock(&self.in_flight).get(&id) {
                token.cancel();
            }
        }
    }

    fn dispatch(
        &self,
        method: &str,
        is_call: bool,
        params: Option<&RawValue>,
        cancel: &CancelToken,
    ) -> Result<String, RpcError> {
        // Until `initialize` has been answered only the lifecycle's own
        // methods are let through.
        match method {
            "initialize"
            | "ping"
            | "notifications/initialized"
            | "notifications/roots/list_changed"
            | "logging/setLevel"
            | "resources/subscribe"
            | "resources/unsubscribe" => {}
            "server/discover" => {
                return Err(RpcError::new(
                    CODE_METHOD_NOT_FOUND,
                    format!(
                        "\"server/discover\" is only supported in protocol version >= {SESSIONLESS_VERSION}"
                    ),
                ));
            }
            _ => {
                if !self.initialized() && is_call {
                    return Err(RpcError::plain(format!(
                        "method {} is invalid during session initialization",
                        quote(method)
                    )));
                }
            }
        }
        let Some(info) = method_info(method) else {
            return Err(RpcError::new(
                CODE_METHOD_NOT_FOUND,
                format!("method not found: {}", quote(method)),
            ));
        };
        check_request(method, is_call, params.is_some())?;
        let object = self.params_object(method, &info, params)?;
        let empty = serde_json::Map::new();
        let args = object.as_ref().unwrap_or(&empty);
        let cursor_check = || match args.get("cursor") {
            Some(Value::String(c)) if !c.is_empty() => {
                Err(RpcError::new(CODE_INVALID_PARAMS, "invalid params"))
            }
            _ => Ok(()),
        };
        match method {
            "initialize" => {
                let client = args
                    .get("protocolVersion")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let mut state = lock(&self.state);
                if state.initialized {
                    return Err(RpcError::plain("duplicate \"initialize\" received"));
                }
                state.initialized = true;
                state.negotiated = negotiated_version(client).to_string();
                Ok(format!(
                    "{{\"capabilities\":{{\"logging\":{{}},\"resources\":{{\"listChanged\":true}},\"tools\":{{\"listChanged\":true}}}},\"instructions\":{},\"protocolVersion\":\"{}\",\"serverInfo\":{{\"name\":\"apic\",\"version\":{}}}}}",
                    json_string(crate::INSTRUCTIONS, false),
                    state.negotiated,
                    json_string(&self.inner.cfg.version, false)
                ))
            }
            "ping" | "logging/setLevel" => Ok("{}".to_string()),
            "tools/list" => {
                cursor_check()?;
                Ok(list_result("tools", &tools_list()))
            }
            "resources/list" => {
                cursor_check()?;
                Ok(list_result("resources", &self.inner.resources_json()))
            }
            "resources/templates/list" => {
                cursor_check()?;
                Ok(list_result("resourceTemplates", &[]))
            }
            "prompts/list" => {
                cursor_check()?;
                Ok(list_result("prompts", &[]))
            }
            "prompts/get" => {
                let name = args.get("name").and_then(Value::as_str).unwrap_or("");
                Err(RpcError::new(
                    CODE_INVALID_PARAMS,
                    format!("unknown prompt {}", quote(name)),
                ))
            }
            "tools/call" => {
                let name = args.get("name").and_then(Value::as_str).unwrap_or("");
                // The arguments are read as they were written.
                let raw: Option<HashMap<String, Box<RawValue>>> =
                    params.and_then(|p| serde_json::from_str(p.get()).ok());
                let arguments = raw.as_ref().and_then(|m| m.get("arguments"));
                call_tool(&self.inner, name, arguments.map(AsRef::as_ref), cancel)
            }
            "resources/read" => {
                let uri = args.get("uri").and_then(Value::as_str).unwrap_or("");
                self.inner.read_resource(uri)
            }
            "completion/complete" if !args.contains_key("ref") => Err(RpcError::new(
                CODE_INVALID_PARAMS,
                "invalid params: missing required 'ref' field",
            )),
            "completion/complete" | "resources/subscribe" | "resources/unsubscribe" => {
                Err(RpcError::new(
                    CODE_METHOD_NOT_FOUND,
                    format!("method not found: {}", quote(method)),
                ))
            }
            // The notifications: nothing to do, nothing to say.
            _ => Ok("null".to_string()),
        }
    }

    /// Reads a method's parameters, which are an object when there are
    /// any.
    fn params_object(
        &self,
        method: &str,
        info: &MethodInfo,
        params: Option<&RawValue>,
    ) -> Result<Option<serde_json::Map<String, Value>>, RpcError> {
        let Some(raw) = params else {
            return Ok(None);
        };
        let ty = info.params_type;
        let mismatch = |detail: String| {
            let text = format!(
                "unmarshaling {} into a *mcp.{ty}: {detail}",
                quote(raw.get())
            );
            if method == "initialize" {
                RpcError::plain(format!("handling '{method}': {text}"))
            } else {
                RpcError::new(
                    CODE_INVALID_PARAMS,
                    format!("handling '{method}': invalid params: {text}"),
                )
            }
        };
        match serde_json::from_str::<Value>(raw.get()) {
            Ok(Value::Null) => {
                if info.missing_params_ok {
                    return Ok(None);
                }
                if method == "initialize" {
                    return Err(RpcError::plain(
                        "handling 'initialize': missing required \"params\"",
                    ));
                }
                Err(RpcError::new(
                    CODE_INVALID_REQUEST,
                    format!("handling '{method}': invalid request: missing required \"params\""),
                ))
            }
            Ok(Value::Object(obj)) => {
                // The members the server reads are strings.
                let wanted: &[&str] = match method {
                    "tools/call" | "prompts/get" => &["name"],
                    "resources/read" => &["uri"],
                    "initialize" => &["protocolVersion"],
                    "tools/list"
                    | "resources/list"
                    | "resources/templates/list"
                    | "prompts/list" => &["cursor"],
                    _ => &[],
                };
                for name in wanted {
                    match obj.get(*name) {
                        None | Some(Value::Null | Value::String(_)) => {}
                        Some(other) => {
                            // The error quotes the text from the value to
                            // the end of the parameters.
                            let key = format!("\"{name}\":");
                            let rest = raw.get().find(&key).map_or_else(
                                || other.to_string(),
                                |at| raw.get()[at + key.len()..].trim_start().to_string(),
                            );
                            return Err(mismatch(format!(
                                "json: cannot unmarshal {} into Go struct field mcp.{ty}.{name} of type string",
                                quote(&rest)
                            )));
                        }
                    }
                }
                Ok(Some(obj))
            }
            _ => Err(mismatch(format!(
                "json: cannot unmarshal {} into Go value of type mcp.{ty}",
                quote(raw.get())
            ))),
        }
    }
}
