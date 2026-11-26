//! The parts of the protocol apic uses, as the specification names them.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;

use apic_project::path;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Position {
    pub line: i64,
    pub character: i64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Range {
    pub start: Position,
    pub end: Position,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct TextDocumentItem {
    pub uri: String,
    pub version: i64,
    pub text: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct TextDocumentPositionParams {
    /// The document's URI.
    pub uri: String,
    pub position: Position,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct DidChangeParams {
    pub uri: String,
    pub version: i64,
    /// The text of each change; the server asks for full sync, so each is
    /// a whole document.
    pub content_changes: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct InitializeParams {
    pub root_uri: String,
    pub root_path: String,
    pub workspace_folders: Vec<String>,
    pub env: String,
    /// The environment per project root, for a client that picks one per
    /// project (the VS Code extension does).
    pub envs: BTreeMap<String, String>,
    /// Fixes the project root of every file in a workspace folder, folder
    /// to root, as the extension's apic.projectDir does.
    pub project_roots: BTreeMap<String, String>,
    /// `code_lens` and `formatting` set false leave those features to a
    /// client that has its own (the VS Code extension keeps its lenses
    /// and formatter).
    pub code_lens: Option<bool>,
    pub formatting: Option<bool>,
    pub position_encodings: Vec<String>,
    pub snippet_support: bool,
    pub dynamic_registration: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Diagnostic {
    pub range: Range,
    pub severity: i64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub code: String,
    #[serde(
        default,
        rename = "codeDescription",
        skip_serializing_if = "Option::is_none"
    )]
    pub code_description: Option<CodeDescription>,
    pub source: String,
    pub message: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CodeDescription {
    pub href: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PublishDiagnosticsParams {
    pub uri: String,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct MarkupContent {
    pub kind: String,
    pub value: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Hover {
    pub contents: MarkupContent,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<Range>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TextEdit {
    pub range: Range,
    #[serde(rename = "newText")]
    pub new_text: String,
}

fn is_zero(n: &i64) -> bool {
    *n == 0
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CompletionItem {
    pub label: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub kind: i64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub documentation: Option<MarkupContent>,
    #[serde(default, rename = "sortText", skip_serializing_if = "String::is_empty")]
    pub sort_text: String,
    #[serde(
        default,
        rename = "filterText",
        skip_serializing_if = "String::is_empty"
    )]
    pub filter_text: String,
    #[serde(default, rename = "insertTextFormat", skip_serializing_if = "is_zero")]
    pub insert_text_format: i64,
    #[serde(default, rename = "textEdit", skip_serializing_if = "Option::is_none")]
    pub text_edit: Option<TextEdit>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CompletionList {
    #[serde(rename = "isIncomplete")]
    pub is_incomplete: bool,
    pub items: Vec<CompletionItem>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Command {
    pub title: String,
    pub command: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub arguments: Vec<Value>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CodeLens {
    pub range: Range,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<Command>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ExecuteCommandParams {
    pub command: String,
    pub arguments: Vec<Node>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ShowMessageParams {
    #[serde(rename = "type")]
    pub kind: i64,
    pub message: String,
}

// Diagnostic severities, completion item kinds, insert text formats and
// message types, as numbered by the specification.
pub(crate) const SEVERITY_ERROR: i64 = 1;
pub(crate) const SEVERITY_WARNING: i64 = 2;

pub(crate) const KIND_KEYWORD: i64 = 14;
#[allow(dead_code)]
pub(crate) const KIND_SNIPPET: i64 = 15;
pub(crate) const KIND_VARIABLE: i64 = 6;
pub(crate) const KIND_FIELD: i64 = 5;
pub(crate) const KIND_OPERATOR: i64 = 24;
pub(crate) const KIND_MODULE: i64 = 9;

pub(crate) const FORMAT_PLAIN: i64 = 1;
pub(crate) const FORMAT_SNIPPET: i64 = 2;

pub(crate) const MESSAGE_ERROR: i64 = 1;
pub(crate) const MESSAGE_WARNING: i64 = 2;
pub(crate) const MESSAGE_INFO: i64 = 3;
pub(crate) const MESSAGE_LOG: i64 = 4;

/// A JSON value as it was written: the members of an object in their
/// order, and each value numbered in the order the document has them, so
/// the first value of the wrong type can be told from a later one.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Node {
    seq: usize,
    kind: Kind,
}

#[derive(Clone, Debug, PartialEq)]
enum Kind {
    Null,
    Bool(bool),
    /// A whole number that fits 64 bits.
    Int(i64),
    /// Any other number, as text.
    Number(String),
    String(String),
    Array(Vec<Node>),
    Object(Vec<(String, Node)>),
}

thread_local! {
    /// Numbers the values of the document being parsed.
    static NEXT_SEQ: Cell<usize> = const { Cell::new(0) };
}

fn node(kind: Kind) -> Node {
    let seq = NEXT_SEQ.with(|n| n.replace(n.get() + 1));
    Node { seq, kind }
}

impl Node {
    /// Parses JSON text.
    pub fn parse(text: &str) -> Result<Node, String> {
        NEXT_SEQ.with(|n| n.set(0));
        serde_json::from_str(text).map_err(|e| e.to_string())
    }

    /// What the value is called in a message about the wrong type.
    pub fn kind_name(&self) -> &'static str {
        match self.kind {
            Kind::Null => "null",
            Kind::Bool(_) => "bool",
            Kind::Int(_) | Kind::Number(_) => "number",
            Kind::String(_) => "string",
            Kind::Array(_) => "array",
            Kind::Object(_) => "object",
        }
    }

    pub fn is_null(&self) -> bool {
        self.kind == Kind::Null
    }

    pub fn as_str(&self) -> Option<&str> {
        match &self.kind {
            Kind::String(s) => Some(s),
            _ => None,
        }
    }
}

impl<'de> Deserialize<'de> for Node {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Node, D::Error> {
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = Node;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a JSON value")
            }

            fn visit_unit<E>(self) -> Result<Node, E> {
                Ok(node(Kind::Null))
            }

            fn visit_bool<E>(self, b: bool) -> Result<Node, E> {
                Ok(node(Kind::Bool(b)))
            }

            fn visit_i64<E>(self, n: i64) -> Result<Node, E> {
                Ok(node(Kind::Int(n)))
            }

            fn visit_u64<E>(self, n: u64) -> Result<Node, E> {
                Ok(node(match i64::try_from(n) {
                    Ok(n) => Kind::Int(n),
                    Err(_) => Kind::Number(n.to_string()),
                }))
            }

            fn visit_f64<E>(self, n: f64) -> Result<Node, E> {
                Ok(node(Kind::Number(format!("{n:?}"))))
            }

            fn visit_str<E>(self, s: &str) -> Result<Node, E> {
                Ok(node(Kind::String(s.to_string())))
            }

            fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut a: A) -> Result<Node, A::Error> {
                let mut this = node(Kind::Null);
                let mut items = Vec::new();
                while let Some(item) = a.next_element()? {
                    items.push(item);
                }
                this.kind = Kind::Array(items);
                Ok(this)
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(self, mut a: A) -> Result<Node, A::Error> {
                let mut this = node(Kind::Null);
                let mut members = Vec::new();
                while let Some(member) = a.next_entry::<String, Node>()? {
                    members.push(member);
                }
                this.kind = Kind::Object(members);
                Ok(this)
            }
        }
        d.deserialize_any(V)
    }
}

/// The first value of the wrong type met while reading a message: where
/// it stands in the document and what to say about it.
pub(crate) type Mismatch = RefCell<Option<(usize, String)>>;

/// A JSON object being read into the parameters of a method. Reading is
/// lenient, as clients expect: a key the server does not know is ignored,
/// a missing or null one leaves the default, and a key is matched as
/// written, else without regard to case. A value of the wrong type is an
/// error; the first one of the document is kept in `err` and reading goes
/// on.
pub(crate) struct Obj<'a> {
    members: &'a [(String, Node)],
    /// The name of the structure being filled, empty for an unnamed one.
    strukt: &'static str,
    /// The keys from the root down to this object, joined by dots.
    path: String,
    err: &'a Mismatch,
}

impl<'a> Obj<'a> {
    /// The root object of a message's parameters; `type_name` names what
    /// was expected in the message about anything else.
    pub fn root(v: &'a Node, type_name: &str, strukt: &'static str, err: &'a Mismatch) -> Obj<'a> {
        let members: &[(String, Node)] = match &v.kind {
            Kind::Object(m) => m,
            Kind::Null => &[],
            _ => {
                set(
                    err,
                    v.seq,
                    format!(
                        "json: cannot unmarshal {} into Go value of type {type_name}",
                        v.kind_name()
                    ),
                );
                &[]
            }
        };
        Obj {
            members,
            strukt,
            path: String::new(),
            err,
        }
    }

    fn get(&self, key: &str) -> Option<&'a Node> {
        let named = |exact: bool| {
            self.members
                .iter()
                .rev()
                .find(|(k, _)| {
                    if exact {
                        k == key
                    } else {
                        k.eq_ignore_ascii_case(key)
                    }
                })
                .map(|(_, v)| v)
        };
        named(true)
            .or_else(|| named(false))
            .filter(|v| !v.is_null())
    }

    /// Reports whether the key is there with a value other than null.
    pub fn has(&self, key: &str) -> bool {
        self.get(key).is_some()
    }

    fn field(&self, key: &str) -> String {
        if self.path.is_empty() {
            key.to_string()
        } else {
            format!("{}.{key}", self.path)
        }
    }

    fn mismatch(&self, key: &str, v: &Node, value: &str, type_name: &str) {
        set(
            self.err,
            v.seq,
            format!(
                "json: cannot unmarshal {value} into Go struct field {}.{} of type {type_name}",
                self.strukt,
                self.field(key)
            ),
        );
    }

    fn text(&self, key: &str, v: &Node) -> String {
        match &v.kind {
            Kind::String(s) => s.clone(),
            Kind::Null => String::new(),
            _ => {
                self.mismatch(key, v, v.kind_name(), "string");
                String::new()
            }
        }
    }

    pub fn string(&self, key: &str) -> String {
        self.opt_string(key).unwrap_or_default()
    }

    pub fn opt_string(&self, key: &str) -> Option<String> {
        let v = self.get(key)?;
        v.as_str().map(str::to_string).or_else(|| {
            self.mismatch(key, v, v.kind_name(), "string");
            None
        })
    }

    pub fn int(&self, key: &str) -> i64 {
        let Some(v) = self.get(key) else {
            return 0;
        };
        match &v.kind {
            Kind::Int(n) => *n,
            Kind::Number(n) => {
                self.mismatch(key, v, &format!("number {n}"), "int");
                0
            }
            _ => {
                self.mismatch(key, v, v.kind_name(), "int");
                0
            }
        }
    }

    pub fn boolean(&self, key: &str) -> bool {
        self.opt_bool(key).unwrap_or_default()
    }

    pub fn opt_bool(&self, key: &str) -> Option<bool> {
        let v = self.get(key)?;
        match v.kind {
            Kind::Bool(b) => Some(b),
            _ => {
                self.mismatch(key, v, v.kind_name(), "bool");
                None
            }
        }
    }

    /// The object under a key; `strukt` is its structure's name and
    /// `type_name` what a value of another type is told was expected.
    pub fn object(&self, key: &str, strukt: &'static str, type_name: &str) -> Obj<'a> {
        let members: &[(String, Node)] = match self.get(key) {
            None => &[],
            Some(v) => match &v.kind {
                Kind::Object(m) => m,
                _ => {
                    self.mismatch(key, v, v.kind_name(), type_name);
                    &[]
                }
            },
        };
        Obj {
            members,
            strukt,
            path: self.field(key),
            err: self.err,
        }
    }

    /// The elements of the list under a key.
    pub fn list(&self, key: &str, type_name: &str) -> &'a [Node] {
        match self.get(key) {
            None => &[],
            Some(v) => match &v.kind {
                Kind::Array(a) => a,
                _ => {
                    self.mismatch(key, v, v.kind_name(), type_name);
                    &[]
                }
            },
        }
    }

    /// One element of the list under a key, as an object.
    pub fn element(
        &self,
        key: &str,
        v: &'a Node,
        strukt: &'static str,
        type_name: &str,
    ) -> Obj<'a> {
        let members: &[(String, Node)] = match &v.kind {
            Kind::Object(m) => m,
            Kind::Null => &[],
            _ => {
                self.mismatch(key, v, v.kind_name(), type_name);
                &[]
            }
        };
        Obj {
            members,
            strukt,
            path: self.field(key),
            err: self.err,
        }
    }

    pub fn string_list(&self, key: &str) -> Vec<String> {
        self.list(key, "[]string")
            .iter()
            .map(|v| self.text(key, v))
            .collect()
    }

    pub fn string_map(&self, key: &str) -> BTreeMap<String, String> {
        let mut out = BTreeMap::new();
        let Some(v) = self.get(key) else {
            return out;
        };
        match &v.kind {
            Kind::Object(m) => {
                for (k, v) in m {
                    out.insert(k.clone(), self.text(key, v));
                }
            }
            _ => self.mismatch(key, v, v.kind_name(), "map[string]string"),
        }
        out
    }
}

fn set(err: &Mismatch, seq: usize, msg: String) {
    let mut e = err.borrow_mut();
    if e.as_ref().is_none_or(|(first, _)| seq < *first) {
        *e = Some((seq, msg));
    }
}

const TEXT_DOCUMENT_ID: &str = "lsp.textDocumentIdentifier";

impl TextDocumentItem {
    pub fn read(o: &Obj<'_>) -> TextDocumentItem {
        let d = o.object("textDocument", "textDocumentItem", "lsp.textDocumentItem");
        TextDocumentItem {
            uri: d.string("uri"),
            version: d.int("version"),
            text: d.string("text"),
        }
    }
}

/// The URI of the `textDocument` of a method's parameters.
pub(crate) fn document_uri(o: &Obj<'_>) -> String {
    o.object("textDocument", "textDocumentIdentifier", TEXT_DOCUMENT_ID)
        .string("uri")
}

impl TextDocumentPositionParams {
    pub fn read(o: &Obj<'_>) -> TextDocumentPositionParams {
        let uri = document_uri(o);
        let p = o.object("position", "position", "lsp.position");
        TextDocumentPositionParams {
            uri,
            position: Position {
                line: p.int("line"),
                character: p.int("character"),
            },
        }
    }
}

impl DidChangeParams {
    pub fn read(o: &Obj<'_>) -> DidChangeParams {
        let d = o.object(
            "textDocument",
            "",
            r#"struct { URI string "json:\"uri\""; Version int "json:\"version\"" }"#,
        );
        const CHANGE: &str = r#"struct { Range *lsp.lspRange "json:\"range,omitempty\""; Text string "json:\"text\"" }"#;
        let changes = o.list("contentChanges", &format!("[]{CHANGE}"));
        DidChangeParams {
            uri: d.string("uri"),
            version: d.int("version"),
            content_changes: changes
                .iter()
                .map(|c| {
                    let c = o.element("contentChanges", c, "", CHANGE);
                    let r = c.object("range", "lspRange", "lsp.lspRange");
                    for end in ["start", "end"] {
                        let p = r.object(end, "position", "lsp.position");
                        let _ = (p.int("line"), p.int("character"));
                    }
                    c.string("text")
                })
                .collect(),
        }
    }
}

/// The URIs of the files a `workspace/didChangeWatchedFiles` names.
pub(crate) fn watched_uris(o: &Obj<'_>) -> Vec<String> {
    const CHANGE: &str = r#"struct { URI string "json:\"uri\""; Type int "json:\"type\"" }"#;
    o.list("changes", &format!("[]{CHANGE}"))
        .iter()
        .map(|c| {
            let c = o.element("changes", c, "", CHANGE);
            let _ = c.int("type");
            c.string("uri")
        })
        .collect()
}

impl InitializeParams {
    pub fn read(o: &Obj<'_>) -> InitializeParams {
        const FOLDER: &str = r#"struct { URI string "json:\"uri\"" }"#;
        const OPTIONS: &str = r#"struct { Env string "json:\"env\""; Envs map[string]string "json:\"envs\""; ProjectRoots map[string]string "json:\"projectRoots\""; CodeLens *bool "json:\"codeLens\""; Formatting *bool "json:\"formatting\"" }"#;
        const GENERAL: &str =
            r#"struct { PositionEncodings []string "json:\"positionEncodings\"" }"#;
        const ITEM: &str = r#"struct { SnippetSupport bool "json:\"snippetSupport\"" }"#;
        const WATCHED: &str =
            r#"struct { DynamicRegistration bool "json:\"dynamicRegistration\"" }"#;
        let completion = format!(r#"struct {{ CompletionItem {ITEM} "json:\"completionItem\"" }}"#);
        let text_document =
            format!(r#"struct {{ Completion {completion} "json:\"completion\"" }}"#);
        let workspace = format!(
            r#"struct {{ DidChangeWatchedFiles {WATCHED} "json:\"didChangeWatchedFiles\"" }}"#
        );
        let capabilities = format!(
            r#"struct {{ General {GENERAL} "json:\"general\""; TextDocument {text_document} "json:\"textDocument\""; Workspace {workspace} "json:\"workspace\"" }}"#
        );

        let mut p = InitializeParams {
            root_uri: o.string("rootUri"),
            root_path: o.string("rootPath"),
            ..InitializeParams::default()
        };
        for f in o.list("workspaceFolders", &format!("[]{FOLDER}")) {
            p.workspace_folders
                .push(o.element("workspaceFolders", f, "", FOLDER).string("uri"));
        }
        let opts = o.object("initializationOptions", "", OPTIONS);
        p.env = opts.string("env");
        p.envs = opts.string_map("envs");
        p.project_roots = opts.string_map("projectRoots");
        p.code_lens = opts.opt_bool("codeLens");
        p.formatting = opts.opt_bool("formatting");
        let caps = o.object("capabilities", "", &capabilities);
        p.position_encodings = caps
            .object("general", "", GENERAL)
            .string_list("positionEncodings");
        p.snippet_support = caps
            .object("textDocument", "", &text_document)
            .object("completion", "", &completion)
            .object("completionItem", "", ITEM)
            .boolean("snippetSupport");
        p.dynamic_registration = caps
            .object("workspace", "", &workspace)
            .object("didChangeWatchedFiles", "", WATCHED)
            .boolean("dynamicRegistration");
        p
    }
}

impl ExecuteCommandParams {
    pub fn read(o: &Obj<'_>) -> ExecuteCommandParams {
        ExecuteCommandParams {
            command: o.string("command"),
            arguments: o.list("arguments", "[]json.RawMessage").to_vec(),
        }
    }
}

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Decodes the `%XX` escapes of a part of a URI; `None` for a bad one.
fn unescape(s: &str) -> Option<Vec<u8>> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let (hi, lo) = (hex(*b.get(i + 1)?)?, hex(*b.get(i + 2)?)?);
            out.push(hi << 4 | lo);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    Some(out)
}

/// Whether the text can be the authority of a URI: optional user
/// information, a host and an optional numeric port.
fn valid_authority(authority: &str) -> bool {
    let host = match authority.rfind('@') {
        Some(i) => {
            let user = &authority[..i];
            let ok = user
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"-._:~!$&'()*+,;=%@".contains(&c));
            if !ok {
                return false;
            }
            &authority[i + 1..]
        }
        None => authority,
    };
    let name = if let Some(rest) = host.strip_prefix('[') {
        // An IPv6 literal, with an optional port after the bracket.
        let Some(end) = rest.rfind(']') else {
            return false;
        };
        let port = &rest[end + 1..];
        if !(port.is_empty()
            || port.starts_with(':') && port[1..].bytes().all(|c| c.is_ascii_digit()))
        {
            return false;
        }
        &host[..end + 2]
    } else if let Some(i) = host.rfind(':') {
        if !host[i + 1..].bytes().all(|c| c.is_ascii_digit()) {
            return false;
        }
        host
    } else {
        host
    };
    let b = name.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c == b'%' {
            if i + 2 >= b.len() || hex(b[i + 1]).is_none() || hex(b[i + 2]).is_none() {
                return false;
            }
            // Only a percent sign or a byte of a non-ASCII character may
            // be escaped in a host.
            if hex(b[i + 1]).unwrap_or(0) < 8 && &name[i..i + 3] != "%25" {
                return false;
            }
            i += 3;
            continue;
        }
        if c < 0x80 && !(c.is_ascii_alphanumeric() || b"-_.~!$&'()*+,;=:[]<>\"".contains(&c)) {
            return false;
        }
        i += 1;
    }
    true
}

/// Turns a file:// URI into a local path; `None` for any other scheme (an
/// untitled buffer, a remote file).
pub(crate) fn uri_to_path(uri: &str) -> Option<String> {
    if uri.bytes().any(|b| b < 0x20 || b == 0x7f) {
        return None;
    }
    let (rest, fragment) = match uri.split_once('#') {
        Some((rest, fragment)) => (rest, fragment),
        None => (uri, ""),
    };
    unescape(fragment)?;
    // The scheme: a letter, then letters, digits, `+`, `-` and `.`, up to
    // a colon.
    let mut colon = None;
    for (i, c) in rest.bytes().enumerate() {
        match c {
            b'a'..=b'z' | b'A'..=b'Z' => {}
            b'0'..=b'9' | b'+' | b'-' | b'.' if i > 0 => {}
            b':' if i > 0 => {
                colon = Some(i);
                break;
            }
            _ => break,
        }
    }
    let colon = colon?;
    if !rest[..colon].eq_ignore_ascii_case("file") {
        return None;
    }
    let mut rest = &rest[colon + 1..];
    if let Some((before, _query)) = rest.split_once('?') {
        rest = before;
    }
    if !rest.starts_with('/') {
        // An opaque URI (file:name) has no path.
        return Some(path::clean(""));
    }
    if let Some(after) = rest.strip_prefix("//") {
        let (authority, tail) = match after.find('/') {
            Some(i) => (&after[..i], &after[i..]),
            None => (after, ""),
        };
        if !valid_authority(authority) {
            return None;
        }
        rest = tail;
    }
    let bytes = unescape(rest)?;
    let mut p = String::from_utf8_lossy(&bytes).into_owned();
    if cfg!(windows) {
        // file:///C:/dir/f.http has the path /C:/dir/f.http.
        if let Some(stripped) = p.strip_prefix('/') {
            p = stripped.to_string();
        }
        p = p.replace('/', "\\");
    }
    Some(path::clean(&p))
}

/// The file:// URI of a local path.
pub(crate) fn path_to_uri(path: &str) -> String {
    let mut p = path::to_slash(path);
    if !p.starts_with('/') {
        p.insert(0, '/'); // C:/dir becomes /C:/dir
    }
    let mut out = String::from("file://");
    for b in p.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~$&+,/:;=@".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Converts between the byte columns apic reports and the columns the
/// client counts in: UTF-16 code units unless it agreed to UTF-8.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Units {
    pub utf8: bool,
}

/// The nearest character boundary at or before byte `i` of `s`.
fn floor_boundary(s: &str, mut i: usize) -> usize {
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

impl Units {
    /// The client column of byte offset `col` (0-based) in `line`.
    pub fn to_client(self, line: &str, col: usize) -> i64 {
        let col = col.min(line.len());
        if self.utf8 {
            return col as i64;
        }
        line[..floor_boundary(line, col)]
            .chars()
            .map(|c| c.len_utf16() as i64)
            .sum()
    }

    /// The byte offset in `line` of client column `ch`.
    pub fn to_byte(self, line: &str, ch: i64) -> usize {
        if self.utf8 {
            // A column inside a character is taken as the character's
            // start, so the line can be cut there.
            return floor_boundary(line, ch.clamp(0, line.len() as i64) as usize);
        }
        let mut n = 0;
        for (i, c) in line.char_indices() {
            if n >= ch {
                return i;
            }
            n += c.len_utf16() as i64;
        }
        line.len()
    }

    /// The position just past the last character of `text`.
    pub fn end_of(self, text: &str) -> Position {
        let ls = lines(text);
        let last = ls[ls.len() - 1];
        Position {
            line: ls.len() as i64 - 1,
            character: self.to_client(last, last.len()),
        }
    }
}

/// Splits text the way editors number lines: on \n, with a trailing \r
/// dropped from each line.
pub(crate) fn lines(text: &str) -> Vec<&str> {
    text.split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
        .collect()
}
