//! The server driven over in-memory pipes, as an editor would drive it.

use std::collections::{BTreeMap, HashMap};
use std::io::{self, BufReader, Cursor, Read, Write};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use apic_httpc::CancelToken;
use apic_httpd::{Response, TestServer};
use apic_project::path;
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use tempfile::TempDir;

use crate::complete::{escape_dollars, plain_snippet};
use crate::jsonrpc::{
    read_message, to_json, Conn, Message, CODE_INVALID_PARAMS, CODE_METHOD_NOT_FOUND,
    CODE_NOT_INITIALIZED, MAX_MESSAGE,
};
use crate::protocol::{
    lines, path_to_uri, uri_to_path, CodeLens, CompletionItem, CompletionList, Diagnostic, Hover,
    PublishDiagnosticsParams, TextEdit, Units, FORMAT_PLAIN, FORMAT_SNIPPET, SEVERITY_ERROR,
};
use crate::server::State;
use crate::{serve, Error, Options, COMMAND_CURL, COMMAND_DESCRIBE, COMMAND_RUN, COMMAND_VALIDATE};

/// The reading end of an in-memory pipe: the end of the stream once the
/// writing end is dropped.
struct PipeReader {
    chunks: Receiver<Vec<u8>>,
    rest: Cursor<Vec<u8>>,
}

struct PipeWriter(Sender<Vec<u8>>);

fn pipe() -> (PipeReader, PipeWriter) {
    let (tx, rx) = mpsc::channel();
    (
        PipeReader {
            chunks: rx,
            rest: Cursor::new(Vec::new()),
        },
        PipeWriter(tx),
    )
}

impl Read for PipeReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            let n = self.rest.read(buf)?;
            if n > 0 || buf.is_empty() {
                return Ok(n);
            }
            match self.chunks.recv() {
                Ok(chunk) => self.rest = Cursor::new(chunk),
                Err(_) => return Ok(0),
            }
        }
    }
}

impl Write for PipeWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0
            .send(buf.to_vec())
            .map_err(|_| io::Error::from(io::ErrorKind::BrokenPipe))?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Default)]
struct Inbox {
    replies: HashMap<String, Message>,
    notes: Vec<Message>,
}

/// Drives a server over in-memory pipes, as an editor would.
struct Client {
    conn: Conn,
    next_id: i64,
    done: Receiver<Result<(), Error>>,
    inbox: Arc<(Mutex<Inbox>, Condvar)>,
}

fn start(opts: Options) -> Client {
    let (to_server, from_client) = pipe();
    let (to_client, from_server) = pipe();
    let (done_tx, done) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = done_tx.send(serve(&CancelToken::new(), to_server, from_server, opts));
    });
    let inbox: Arc<(Mutex<Inbox>, Condvar)> = Arc::default();
    let shared = Arc::clone(&inbox);
    std::thread::spawn(move || {
        let mut r = BufReader::new(to_client);
        while let Ok(m) = read_message(&mut r) {
            let (lock, arrived) = &*shared;
            let mut inbox = lock.lock().unwrap();
            if !m.method.is_empty() {
                inbox.notes.push(m);
            } else if let Some(id) = m.id.clone() {
                inbox.replies.insert(id, m);
            }
            arrived.notify_all();
        }
    });
    // Dropping the client closes its end, which stops the server.
    Client {
        conn: Conn::new(from_client),
        next_id: 0,
        done,
        inbox,
    }
}

impl Client {
    /// Sends a request and waits for its reply.
    fn call(&mut self, method: &str, params: Value) -> Message {
        self.next_id += 1;
        let id = self.next_id.to_string();
        self.conn
            .write(&Message {
                id: Some(id.clone()),
                method: method.to_string(),
                params: Some(to_json(&params)),
                ..Message::default()
            })
            .unwrap();
        let mut m = None;
        self.wait_for(method, |inbox| {
            m = inbox.replies.get(&id).cloned();
            m.is_some()
        });
        m.unwrap()
    }

    fn notify(&self, method: &str, params: Value) {
        self.conn.notify(method, &params).unwrap();
    }

    /// Waits until `cond` holds, under the client's lock.
    fn wait_for(&self, what: &str, mut cond: impl FnMut(&Inbox) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        let (lock, arrived) = &*self.inbox;
        let mut inbox = lock.lock().unwrap();
        loop {
            if cond(&inbox) {
                return;
            }
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            inbox = arrived
                .wait_timeout(inbox, Duration::from_millis(20))
                .unwrap()
                .0;
        }
    }

    /// Waits for the latest diagnostics published for a path after the
    /// first `after` notifications.
    fn diagnostics(&self, file: &str, after: usize) -> Vec<Diagnostic> {
        let mut out = Vec::new();
        let uri = path_to_uri(file);
        self.wait_for(&format!("diagnostics for {file}"), |inbox| {
            for n in inbox.notes[after.min(inbox.notes.len())..].iter().rev() {
                if n.method != "textDocument/publishDiagnostics" {
                    continue;
                }
                let p: PublishDiagnosticsParams =
                    serde_json::from_str(n.params.as_deref().unwrap_or("null")).unwrap();
                if p.uri == uri {
                    out = p.diagnostics;
                    return true;
                }
            }
            false
        });
        out
    }

    fn note_count(&self) -> usize {
        self.inbox.0.lock().unwrap().notes.len()
    }

    fn notes(&self) -> Vec<Message> {
        self.inbox.0.lock().unwrap().notes.clone()
    }

    fn open(&self, file: &str, text: &str) {
        self.notify(
            "textDocument/didOpen",
            json!({"textDocument": {"uri": path_to_uri(file), "languageId": "http", "version": 1, "text": text}}),
        );
    }

    fn change(&self, file: &str, text: &str, version: i64) {
        self.notify(
            "textDocument/didChange",
            json!({
                "textDocument": {"uri": path_to_uri(file), "version": version},
                "contentChanges": [{"text": text}],
            }),
        );
    }

    fn finished(&self) -> Result<(), Error> {
        self.done
            .recv_timeout(Duration::from_secs(5))
            .expect("the server did not exit")
    }
}

#[track_caller]
fn result<T: DeserializeOwned>(m: &Message) -> T {
    assert!(m.error.is_none(), "error reply: {:?}", m.error);
    let raw = m.result.as_deref().unwrap_or("null");
    serde_json::from_str(raw).unwrap_or_else(|e| panic!("{e}: {raw}"))
}

fn write_file(file: &str, content: &str) {
    std::fs::create_dir_all(path::dir(file)).unwrap();
    std::fs::write(file, content).unwrap();
}

fn temp_dir() -> (TempDir, String) {
    let tmp = TempDir::new().unwrap();
    // The real path, so the paths the server works out match the test's.
    let dir = path::eval_symlinks(&tmp.path().to_string_lossy()).unwrap();
    (tmp, dir)
}

fn doc(file: &str) -> Value {
    json!({"uri": path_to_uri(file)})
}

const API_HTTP: &str = r#"### Log in
# @name login
# @assert status == 200
# @capture token = body.$.token
POST {{baseUrl}}/login
Content-Type: application/json

{"user": "{{user}}", "password": "{{password}}"}

### Who am I
# @name whoami
# @ref login
GET {{baseUrl}}/me
Authorization: Bearer {{token}}
"#;

/// What a fixture keeps alive: the API and the directory.
struct Fixture {
    _srv: TestServer,
    _tmp: TempDir,
}

/// A project with an env file, a secret and two requests, and a server
/// initialised on it.
fn fixture(snippets: bool) -> (Client, String, String, Fixture) {
    let srv = TestServer::new(|_: &apic_httpd::Request| {
        Response::json(
            200,
            r#"{"token":"t-1","user":{"name":"alice","roles":["admin"]}}"#,
        )
    });
    let (tmp, dir) = temp_dir();
    write_file(
        &path::join(&dir, "http-client.env.json"),
        &format!(
            r#"{{"dev": {{"baseUrl": "{}", "user": "alice"}}}}"#,
            srv.url()
        ),
    );
    write_file(
        &path::join(&dir, "http-client.private.env.json"),
        r#"{"dev": {"password": "s3cret"}}"#,
    );
    write_file(&path::join(&dir, "apic.yaml"), "env: dev\n");
    let file = path::join(&dir, "api.http");
    write_file(&file, API_HTTP);
    let mut c = start(Options {
        version: "test".to_string(),
        ..Options::default()
    });
    let init = c.call(
        "initialize",
        json!({
            "rootUri": path_to_uri(&dir),
            "capabilities": {"textDocument": {"completion": {"completionItem": {"snippetSupport": snippets}}}},
        }),
    );
    let caps = &result::<Value>(&init)["capabilities"];
    for k in [
        "completionProvider",
        "hoverProvider",
        "codeLensProvider",
        "documentFormattingProvider",
        "executeCommandProvider",
    ] {
        assert!(!caps[k].is_null(), "capability {k} missing: {caps}");
    }
    c.notify("initialized", json!({}));
    (
        c,
        dir,
        file,
        Fixture {
            _srv: srv,
            _tmp: tmp,
        },
    )
}

#[test]
fn diagnostics_follow_the_buffer() {
    let (c, _, file, _keep) = fixture(true);
    c.open(&file, API_HTTP);
    let d = c.diagnostics(&file, 0);
    assert!(d.is_empty(), "a clean file has diagnostics: {d:?}");
    // An unsaved edit: a bad assertion on line 3.
    let broken = API_HTTP.replacen("# @assert status == 200", "# @assert", 1);
    let n = c.note_count();
    c.change(&file, &broken, 2);
    let d = c.diagnostics(&file, n);
    assert!(
        d.len() == 1
            && d[0].code == "bad-assert"
            && d[0].severity == SEVERITY_ERROR
            && d[0].range.start.line == 2,
        "diagnostics after the edit = {d:?}"
    );
    assert!(
        d[0].range.start.character == 2 && d[0].range.end.character == 9,
        "range = {:?}, want the @assert key (2..9)",
        d[0].range
    );
    let n = c.note_count();
    c.change(&file, API_HTTP, 3);
    let d = c.diagnostics(&file, n);
    assert!(
        d.is_empty(),
        "fixing it should clear the diagnostics: {d:?}"
    );
}

#[test]
fn diagnostic_columns_are_utf16() {
    let (c, _, file, _keep) = fixture(true);
    // é is two bytes and one UTF-16 unit; 😀 is four bytes and two units.
    let text = "### é😀\n# @name café\n# @frobnicate\nGET https://example.com\n";
    c.open(&file, text);
    let d = c.diagnostics(&file, 0);
    assert!(
        d.len() == 1 && d[0].code == "unknown-directive" && d[0].range.start.line == 2,
        "diagnostics = {d:?}"
    );
    let u = Units::default();
    assert_eq!(u.to_client("é😀x", "é😀".len()), 3, "UTF-16 column of x");
    assert_eq!(u.to_byte("é😀x", 3), "é😀".len(), "byte offset of column 3");
}

fn completion_labels(m: &Message) -> (BTreeMap<String, CompletionItem>, Vec<String>) {
    let list: CompletionList = result(m);
    let mut by_label = BTreeMap::new();
    let mut labels = Vec::new();
    for it in list.items {
        labels.push(it.label.clone());
        by_label.insert(it.label.clone(), it);
    }
    (by_label, labels)
}

fn new_text(items: &BTreeMap<String, CompletionItem>, label: &str) -> String {
    items
        .get(label)
        .and_then(|it| it.text_edit.as_ref())
        .map(|e| e.new_text.clone())
        .unwrap_or_default()
}

#[test]
fn completion() {
    let (mut c, _, file, _keep) = fixture(true);
    let text = format!("{API_HTTP}\n### New\n# @\nGET {{{{\n# @assert \n# @auth \n# @ref \n");
    c.open(&file, &text);
    let ls = lines(&text);
    let at = |line: usize| json!({"textDocument": doc(&file), "position": {"line": line, "character": ls[line].len()}});
    let find = |prefix: &str| -> usize {
        ls.iter()
            .rposition(|l| *l == prefix)
            .unwrap_or_else(|| panic!("no line {prefix:?}"))
    };

    let (items, labels) = completion_labels(&c.call("textDocument/completion", at(find("# @"))));
    assert!(
        labels[0] == "@name"
            && items["@assert"].insert_text_format == FORMAT_SNIPPET
            && items.contains_key("@retry"),
        "directives = {labels:?}"
    );
    let e = items["@name"].text_edit.as_ref().unwrap();
    assert!(
        e.range.start.character == 2 && e.new_text == "@name ${1:request-name}",
        "@name edit = {e:?}"
    );

    let (items, _) = completion_labels(&c.call("textDocument/completion", at(find("GET {{"))));
    let it = &items["baseUrl"];
    assert!(
        !it.detail.is_empty() && new_text(&items, "baseUrl") == "baseUrl}}",
        "baseUrl = {it:?}"
    );
    let it = &items["password"];
    assert!(
        it.documentation
            .as_ref()
            .is_some_and(|d| d.value == "= ***"),
        "a secret's value should be masked: {it:?}"
    );
    assert!(
        items.contains_key("$uuid") && items.contains_key("login.response.body.$"),
        "built-ins and response references missing: {items:?}"
    );

    let (items, _) = completion_labels(&c.call("textDocument/completion", at(find("# @assert "))));
    assert!(
        items.contains_key("status") && items.contains_key("body.$."),
        "selectors = {items:?}"
    );
    let (items, _) = completion_labels(&c.call("textDocument/completion", at(find("# @auth "))));
    assert!(
        items.contains_key("bearer") && items.contains_key("aws"),
        "auth types = {items:?}"
    );
    let (items, _) = completion_labels(&c.call("textDocument/completion", at(find("# @ref "))));
    assert!(
        items.contains_key("login") && items.contains_key("whoami"),
        "ref targets = {items:?}"
    );
    let none = c.call("textDocument/completion", at(0));
    assert!(
        none.error.is_none() && none.result.is_none(),
        "a title line offers nothing: {none:?}"
    );
}

#[test]
fn completion_without_snippet_support() {
    let (mut c, _, file, _keep) = fixture(false);
    c.open(&file, "# @\n");
    let (items, _) = completion_labels(&c.call(
        "textDocument/completion",
        json!({"textDocument": doc(&file), "position": {"line": 0, "character": 3}}),
    ));
    let it = &items["@assert"];
    assert!(
        it.insert_text_format == FORMAT_PLAIN
            && new_text(&items, "@assert") == "@assert status == 200",
        "plain @assert = {:?}",
        it.text_edit
    );
}

#[test]
fn body_path_completion_uses_the_last_run() {
    let (mut c, _, file, _keep) = fixture(true);
    c.open(&file, API_HTTP);
    let run = c.call(
        "workspace/executeCommand",
        json!({"command": COMMAND_RUN, "arguments": [path_to_uri(&file), "api.http#login"]}),
    );
    let res: Value = result(&run);
    assert_eq!(res["ok"], json!(true), "run login = {res}");
    let text = API_HTTP.replacen(
        "# @capture token = body.$.token",
        "# @capture token = body.$.user.",
        1,
    );
    c.change(&file, &text, 2);
    let line = 3;
    let (items, _) = completion_labels(&c.call(
        "textDocument/completion",
        json!({"textDocument": doc(&file), "position": {"line": line, "character": lines(&text)[line].len()}}),
    ));
    assert!(
        items.contains_key("body.$.user.name")
            && items
                .get("body.$.user.roles")
                .is_some_and(|it| it.detail == "array of 1"),
        "body paths after a run = {items:?}"
    );
}

#[test]
fn hover() {
    let (mut c, _, file, _keep) = fixture(true);
    c.open(&file, API_HTTP);
    let ls = lines(API_HTTP);
    let mut hover_at = |line: usize, word: &str| -> Message {
        let at = ls[line].find(&format!("{{{{{word}")).unwrap() + 2;
        c.call(
            "textDocument/hover",
            json!({"textDocument": doc(&file), "position": {"line": line, "character": at}}),
        )
    };
    let h: Hover = result(&hover_at(4, "baseUrl"));
    assert!(
        h.contents.value.contains("**baseUrl** = `http://127.0.0.1")
            && h.contents.value.contains("http-client.env.json"),
        "baseUrl hover = {:?}",
        h.contents.value
    );
    assert!(
        h.range
            .is_some_and(|r| r.start.character == ls[4].find("{{").unwrap() as i64),
        "hover range = {:?}",
        h.range
    );
    let h: Hover = result(&hover_at(7, "password"));
    assert!(
        h.contents.value.contains("`***`")
            && !h.contents.value.contains("s3cret")
            && h.contents.value.contains("secret, masked"),
        "a secret's hover = {:?}",
        h.contents.value
    );
    let h: Hover = result(&hover_at(13, "token"));
    assert!(
        h.contents.value.contains("not set")
            && h.contents.value.contains("which `# @ref` runs first"),
        "a captured variable's hover = {:?}",
        h.contents.value
    );
    let off = c.call(
        "textDocument/hover",
        json!({"textDocument": doc(&file), "position": {"line": 0, "character": 5}}),
    );
    assert_eq!(off.result, None, "hover off a placeholder");
}

#[test]
fn code_lenses_and_commands() {
    let (mut c, _, file, _keep) = fixture(true);
    c.open(&file, API_HTTP);
    let lenses: Vec<CodeLens> =
        result(&c.call("textDocument/codeLens", json!({"textDocument": doc(&file)})));
    let cmd = |i: usize| lenses[i].command.clone().unwrap();
    assert!(
        lenses.len() == 6
            && cmd(0).title == "▶ Run login"
            && lenses[0].range.start.line == 4
            && lenses[3].range.start.line == 12,
        "lenses = {lenses:?}"
    );
    let args = cmd(4).arguments;
    assert!(
        args.len() == 2
            && args[1] == json!("api.http#whoami")
            && cmd(4).command == COMMAND_DESCRIBE,
        "describe lens = {:?}",
        cmd(4)
    );

    let n = c.note_count();
    let run = c.call(
        "workspace/executeCommand",
        json!({"command": COMMAND_RUN, "arguments": cmd(3).arguments}),
    );
    let res: Value = result(&run);
    assert!(
        res["ok"] == json!(true) && !res["ran_first"].is_null(),
        "run whoami = {res}"
    );
    c.wait_for("a summary", |inbox| {
        inbox.notes[n..].iter().any(|m| {
            m.method == "window/showMessage"
                && m.params
                    .as_deref()
                    .is_some_and(|p| p.contains("✓ whoami · 200 OK"))
        })
    });

    let d: Value = result(&c.call(
        "workspace/executeCommand",
        json!({"command": COMMAND_DESCRIBE, "arguments": cmd(1).arguments}),
    ));
    assert!(
        d["name"] == json!("login") && !d.to_string().contains("s3cret"),
        "describe = {d}"
    );
    let curl: String = result(&c.call(
        "workspace/executeCommand",
        json!({"command": COMMAND_CURL, "arguments": cmd(2).arguments}),
    ));
    assert!(
        curl.starts_with("curl ") && curl.contains("/login"),
        "curl = {curl:?}"
    );
    let bad = c.call(
        "workspace/executeCommand",
        json!({"command": "apic.lsp.nope", "arguments": cmd(2).arguments}),
    );
    assert!(
        bad.error
            .as_ref()
            .is_some_and(|e| e.code == CODE_INVALID_PARAMS),
        "an unknown command = {:?}",
        bad.error
    );
}

#[test]
fn formatting() {
    let (mut c, _, file, _keep) = fixture(true);
    let messy = "###   Log in\n# @name login\npost {{baseUrl}}/login\ncontent-type: application/json\n\n{\"a\":1}\n";
    c.open(&file, messy);
    let edits: Vec<TextEdit> = result(&c.call(
        "textDocument/formatting",
        json!({"textDocument": doc(&file), "options": {"tabSize": 2, "insertSpaces": true}}),
    ));
    assert!(
        edits.len() == 1
            && edits[0].new_text.contains("Content-Type: application/json")
            && edits[0].range.end.line == 6,
        "edits = {edits:?}"
    );
    c.change(&file, &edits[0].new_text, 2);
    let edits: Vec<TextEdit> = result(&c.call(
        "textDocument/formatting",
        json!({"textDocument": doc(&file)}),
    ));
    assert!(edits.is_empty(), "formatting a formatted file = {edits:?}");
}

#[test]
fn watched_files_recheck_the_project() {
    let (c, dir, file, _keep) = fixture(true);
    c.open(&file, API_HTTP);
    c.diagnostics(&file, 0);
    // A retry policy that does not parse, written outside the editor.
    let cfg = path::join(&dir, "apic.yaml");
    write_file(&cfg, "env: dev\nretry: lots\n");
    let n = c.note_count();
    c.notify(
        "workspace/didChangeWatchedFiles",
        json!({"changes": [{"uri": path_to_uri(&cfg), "type": 2}]}),
    );
    let d = c.diagnostics(&cfg, n);
    assert!(
        d.len() == 1 && d[0].code == "bad-retry",
        "apic.yaml diagnostics = {d:?}"
    );
}

#[test]
fn lifecycle() {
    let (_tmp, dir) = temp_dir();
    let mut c = start(Options::default());
    let early = c.call("textDocument/hover", json!({}));
    assert!(
        early
            .error
            .as_ref()
            .is_some_and(|e| e.code == CODE_NOT_INITIALIZED),
        "a request before initialize = {:?}",
        early.error
    );
    c.call("initialize", json!({"rootUri": path_to_uri(&dir)}));
    let unknown = c.call("textDocument/references", json!({}));
    assert!(
        unknown
            .error
            .as_ref()
            .is_some_and(|e| e.code == CODE_METHOD_NOT_FOUND),
        "an unknown method = {:?}",
        unknown.error
    );
    let m = c.call("shutdown", Value::Null);
    assert!(m.error.is_none(), "{:?}", m.error);
    c.notify("exit", Value::Null);
    assert_eq!(c.finished(), Ok(()), "exit after shutdown");

    let mut c = start(Options::default());
    c.call("initialize", json!({}));
    c.notify("exit", Value::Null);
    assert_eq!(
        c.finished(),
        Err(Error::NoShutdown),
        "exit without shutdown should be an error"
    );
}

#[test]
fn framing_rejects_a_bad_length() {
    let mut r = Cursor::new("Content-Length: lots\r\n\r\n{}");
    let err = read_message(&mut r).unwrap_err();
    assert!(err.to_string().contains("Content-Length"), "err = {err}");
    let mut r = Cursor::new(format!("Content-Length: {}\r\n\r\n{{}}", MAX_MESSAGE + 1));
    assert!(
        read_message(&mut r).is_err(),
        "an oversized message was accepted"
    );
}

#[test]
fn plain_snippet_cases() {
    for (input, want) in [
        (
            "assert ${1:status} ${2|==,!=|} ${3:200}",
            "assert status == 200",
        ),
        ("$env.${1:NAME}", "$env.NAME"),
        ("retry ${1:5} ${2:1s}$0", "retry 5 1s"),
    ] {
        assert_eq!(plain_snippet(input), want, "plain_snippet({input:?})");
    }
}

#[test]
fn uris() {
    let (_tmp, dir) = temp_dir();
    let p = path::join_all(&[&dir, "a b", "x.http"]);
    assert_eq!(
        uri_to_path(&path_to_uri(&p)).as_deref(),
        Some(p.as_str()),
        "round trip of {p}"
    );
    assert_eq!(
        uri_to_path("untitled:Untitled-1"),
        None,
        "an untitled buffer is not a file"
    );
}

#[test]
fn the_workspace_is_checked_without_opening_a_file() {
    let (_tmp, dir) = temp_dir();
    write_file(&path::join(&dir, "apic.yaml"), "env: dev\n");
    let warn = path::join(&dir, "warn.http");
    write_file(
        &warn,
        "### w\n# @name w\n# @frobnicate\nGET https://example.com\n",
    );
    let mut c = start(Options::default());
    c.call("initialize", json!({"rootUri": path_to_uri(&dir)}));
    c.notify("initialized", json!({}));
    let d = c.diagnostics(&warn, 0);
    assert!(
        d.len() == 1 && d[0].code == "unknown-directive",
        "diagnostics at start = {d:?}"
    );
    // The file is fixed on disk; apic.lsp.validate re-checks and answers
    // once the diagnostics are out.
    write_file(&warn, "### w\n# @name w\nGET https://example.com\n");
    let n = c.note_count();
    let m = c.call(
        "workspace/executeCommand",
        json!({"command": COMMAND_VALIDATE}),
    );
    assert!(m.error.is_none(), "{:?}", m.error);
    assert_ne!(c.note_count(), n, "validate answered before publishing");
    let d = c.diagnostics(&warn, n);
    assert!(d.is_empty(), "after the fix = {d:?}");
}

#[test]
fn initialization_options_turn_features_off() {
    let (_tmp, dir) = temp_dir();
    let mut c = start(Options::default());
    let init = c.call(
        "initialize",
        json!({
            "rootUri": path_to_uri(&dir),
            "initializationOptions": {"env": "staging", "codeLens": false, "formatting": false},
        }),
    );
    let caps = &result::<Value>(&init)["capabilities"];
    assert!(
        caps["codeLensProvider"].is_null()
            && caps["documentFormattingProvider"].is_null()
            && caps["hoverProvider"] == json!(true),
        "capabilities = {caps}"
    );
}

#[test]
fn builtin_snippets_keep_their_dollar() {
    for snippets in [true, false] {
        let (mut c, _, file, _keep) = fixture(snippets);
        let text = format!("{API_HTTP}\n### More\nGET {{{{\n");
        c.open(&file, &text);
        let last = lines(&text).len() - 2;
        let (items, _) = completion_labels(&c.call(
            "textDocument/completion",
            json!({"textDocument": doc(&file), "position": {"line": last, "character": 6}}),
        ));
        let want: [(&str, &str); 3] = if snippets {
            [
                ("$uuid", r"\$uuid}}"),
                ("$env", r"\$env.${1:NAME}}}"),
                (
                    "login.response.body.$",
                    r"login.response.body.\$.${1:path}}}",
                ),
            ]
        } else {
            [
                ("$uuid", "$uuid}}"),
                ("$env", "$env.NAME}}"),
                ("login.response.body.$", "login.response.body.$.path}}"),
            ]
        };
        for (label, insert) in want {
            assert!(
                items.contains_key(label),
                "snippets={snippets}: no {label} in {items:?}"
            );
            assert_eq!(
                new_text(&items, label),
                insert,
                "snippets={snippets} {label}"
            );
        }
    }
    assert_eq!(
        escape_dollars("assert ${1:status} $0 body.$.x $uuid"),
        r"assert ${1:status} $0 body.\$.x \$uuid"
    );
}

#[test]
fn body_path_completion_reads_history_in_the_configured_environment() {
    let (mut c, dir, file, _keep) = fixture(true);
    write_file(&path::join(&dir, "apic.yaml"), "env: dev\nhistory: 3\n");
    // A run from the terminal recorded login under dev, apic.yaml's env;
    // the editor named no environment.
    let stored = r#"{"ok":true,"request":{"name":"login"},"response":{"status":200,"headers":{},"body":{"token":"t","expires":3600},"duration_ms":1,"size":1}}"#;
    apic_history::Store::new(std::path::Path::new(&dir), 3)
        .record("dev", "login", apic_history::Time::now(), stored)
        .unwrap();
    let text = API_HTTP.replacen(
        "# @capture token = body.$.token",
        "# @capture token = body.$.",
        1,
    );
    c.open(&file, &text);
    let (items, _) = completion_labels(&c.call(
        "textDocument/completion",
        json!({"textDocument": doc(&file), "position": {"line": 3, "character": lines(&text)[3].len()}}),
    ));
    assert!(
        items.contains_key("body.$.expires") && items.contains_key("body.$.token"),
        "history keys = {items:?}"
    );
}

/// A workspace folder with no marker of its own holding projects a and
/// b, each with a request named login and environments dev and staging
/// that point at different hosts.
fn two_projects() -> (TempDir, String) {
    let (tmp, ws) = temp_dir();
    for p in ["a", "b"] {
        write_file(&path::join_all(&[&ws, p, "apic.yaml"]), "env: dev\n");
        write_file(
            &path::join_all(&[&ws, p, "http-client.env.json"]),
            &format!(r#"{{"dev": {{"host": "{p}-dev"}}, "staging": {{"host": "{p}-staging"}}}}"#),
        );
        write_file(
            &path::join_all(&[&ws, p, "api.http"]),
            "### Log in\n# @name login\nGET https://{{host}}/login\n",
        );
    }
    (tmp, ws)
}

fn hover_value(c: &mut Client, file: &str) -> String {
    let m = c.call(
        "textDocument/hover",
        json!({"textDocument": doc(file), "position": {"line": 2, "character": 13}}),
    );
    result::<Hover>(&m).contents.value
}

fn must_read(file: &str) -> String {
    std::fs::read_to_string(file).unwrap()
}

#[test]
fn environments_are_per_project() {
    let (_tmp, ws) = two_projects();
    let a = path::join_all(&[&ws, "a", "api.http"]);
    let b = path::join_all(&[&ws, "b", "api.http"]);
    let mut c = start(Options::default());
    c.call(
        "initialize",
        json!({"rootUri": path_to_uri(&ws), "initializationOptions": {"envs": {path::join(&ws, "a"): "staging"}}}),
    );
    c.notify("initialized", json!({}));
    c.open(&a, &must_read(&a));
    c.open(&b, &must_read(&b));
    let v = hover_value(&mut c, &a);
    assert!(v.contains("a-staging"), "a's host = {v:?}, want staging");
    let v = hover_value(&mut c, &b);
    assert!(
        v.contains("b-dev"),
        "b's host = {v:?}, want apic.yaml's dev"
    );
    c.notify(
        "workspace/didChangeConfiguration",
        json!({"settings": {"apic": {"envs": {path::join(&ws, "b"): "staging"}}}}),
    );
    let v = hover_value(&mut c, &b);
    assert!(v.contains("b-staging"), "b's host after the change = {v:?}");
    let v = hover_value(&mut c, &a);
    assert!(v.contains("a-staging"), "changing b moved a: {v:?}");
}

#[test]
fn nested_projects_keep_their_own_diagnostics() {
    let (_tmp, ws) = two_projects();
    let a = path::join_all(&[&ws, "a", "api.http"]);
    let mut c = start(Options::default());
    c.call("initialize", json!({"rootUri": path_to_uri(&ws)}));
    c.notify("initialized", json!({}));
    // Both projects name a request login; that is no clash, so the
    // workspace check must not publish duplicate-name warnings.
    c.call(
        "workspace/executeCommand",
        json!({"command": COMMAND_VALIDATE}),
    );
    for n in c.notes() {
        let params = n.params.unwrap_or_default();
        assert!(
            !(n.method == "textDocument/publishDiagnostics" && params.contains("duplicate-name")),
            "a cross-project duplicate was published: {params}"
        );
    }
    let n = c.note_count();
    c.open(
        &a,
        "### Log in\n# @name login\n# @frobnicate\nGET https://{{host}}/login\n",
    );
    let d = c.diagnostics(&a, n);
    assert!(
        d.len() == 1 && d[0].code == "unknown-directive",
        "a's own problem = {d:?}"
    );
}

#[test]
fn project_roots_fix_the_root() {
    let (_tmp, ws) = temp_dir();
    write_file(
        &path::join_all(&[&ws, "api", "http-client.env.json"]),
        r#"{"dev": {"host": "right"}}"#,
    );
    write_file(
        &path::join_all(&[&ws, "api", "requests", "sub", "http-client.env.json"]),
        r#"{"dev": {"host": "stray"}}"#,
    );
    let file = path::join_all(&[&ws, "api", "requests", "sub", "x.http"]);
    write_file(&file, "### X\n# @name x\nGET https://{{host}}/x\n");
    let mut c = start(Options {
        env: "dev".to_string(),
        ..Options::default()
    });
    c.call(
        "initialize",
        json!({"rootUri": path_to_uri(&ws), "initializationOptions": {"projectRoots": {ws.clone(): path::join(&ws, "api")}}}),
    );
    c.open(&file, &must_read(&file));
    let v = hover_value(&mut c, &file);
    assert!(v.contains("`right`"), "host = {v:?}, want the fixed root's");
}

#[test]
fn files_outside_the_workspace_stay_in_their_directory() {
    let (_tmp, home) = temp_dir();
    write_file(&path::join(&home, "http-client.env.json"), "{}");
    let s = State {
        roots: vec![path::join(&home, "work")],
        ..State::default()
    };
    let scratch = path::join_all(&[&home, "scratch", "t.http"]);
    assert_eq!(
        s.root_for(&scratch),
        path::dir(&scratch),
        "root of a file outside the workspace, want its directory"
    );
    let inside = path::join_all(&[&home, "work", "..build", "x.http"]);
    assert_eq!(
        s.root_for(&inside),
        path::join(&home, "work"),
        "root of {inside}, want the workspace folder"
    );
}

#[test]
fn debounced_edits_are_settled_before_a_request() {
    let (_tmp, dir) = temp_dir();
    write_file(&path::join(&dir, "apic.yaml"), "");
    let file = path::join(&dir, "api.http");
    write_file(&file, "### A\n# @name a\nGET https://example.com\n");
    let mut c = start(Options {
        debounce: Duration::from_secs(3600),
        ..Options::default()
    });
    c.call("initialize", json!({"rootUri": path_to_uri(&dir)}));
    c.open(&file, &must_read(&file));
    let text = "### A\n# @name a\nGET https://example.com\n\n### B\n# @name fresh\nGET https://example.com\n\n### C\n# @ref \nGET https://example.com\n";
    c.change(&file, text, 2);
    // The check is an hour away, but the completion settles it first.
    let (items, _) = completion_labels(&c.call(
        "textDocument/completion",
        json!({"textDocument": doc(&file), "position": {"line": 9, "character": 7}}),
    ));
    assert!(
        items.contains_key("fresh"),
        "ref targets = {items:?}, want the request just typed"
    );
    // Edits pile up behind the debounce; the server still stops at once.
    c.change(&file, &format!("{text}\n"), 3);
    c.change(&file, &format!("{text}\n\n"), 4);
    c.call("shutdown", Value::Null);
    c.notify("exit", Value::Null);
    assert_eq!(c.finished(), Ok(()), "exit with checks pending");
}

#[test]
fn curl_lens_is_redacted() {
    let (mut c, _, file, _keep) = fixture(true);
    c.open(&file, API_HTTP);
    let curl: String = result(&c.call(
        "workspace/executeCommand",
        json!({"command": COMMAND_CURL, "arguments": [path_to_uri(&file), "api.http#login"]}),
    ));
    assert!(
        !curl.contains("s3cret") && curl.contains("/login"),
        "curl = {curl:?}"
    );
}

/// A writer the test can read back.
#[derive(Clone, Default)]
struct Shared(Arc<Mutex<Vec<u8>>>);

impl Write for Shared {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn a_parse_error_is_answered_with_a_null_id() {
    let out = Shared::default();
    let input = Cursor::new("Content-Length: 7\r\n\r\n{nope!}");
    let err = serve(&CancelToken::new(), input, out.clone(), Options::default());
    assert_eq!(err, Err(Error::NoShutdown));
    let out = String::from_utf8(out.0.lock().unwrap().clone()).unwrap();
    assert!(
        out.contains(r#""id":null"#) && out.contains(r#""code":-32700"#),
        "reply = {out}"
    );
}

#[test]
fn a_cancelled_token_stops_the_server() {
    let (to_server, _from_client) = pipe();
    let cancel = CancelToken::new();
    let stop = cancel.clone();
    let t =
        std::thread::spawn(move || serve(&stop, to_server, Shared::default(), Options::default()));
    cancel.cancel();
    assert_eq!(t.join().unwrap(), Err(Error::Cancelled));
}

#[test]
fn a_parameter_of_the_wrong_type_is_named() {
    let mut c = start(Options::default());
    c.call("initialize", json!({}));
    let message = |m: Message| m.error.map(|e| (e.code, e.message)).unwrap_or_default();
    // The first one in the document is the one reported.
    let bad = c.call(
        "textDocument/hover",
        json!({"position": {"line": "x"}, "textDocument": {"uri": 5}}),
    );
    assert_eq!(
        message(bad),
        (
            CODE_INVALID_PARAMS,
            "json: cannot unmarshal string into Go struct field position.position.line of type int"
                .to_string()
        )
    );
    let bad = c.call(
        "textDocument/hover",
        json!({"textDocument": {"uri": "file:///x.http"}, "position": {"line": 1.5}}),
    );
    assert_eq!(
        message(bad).1,
        "json: cannot unmarshal number 1.5 into Go struct field position.position.line of type int"
    );
    let bad = c.call("textDocument/codeLens", json!([]));
    assert_eq!(
        message(bad).1,
        r#"json: cannot unmarshal array into Go value of type struct { TextDocument lsp.textDocumentIdentifier "json:\"textDocument\"" }"#
    );
    // Keys are matched without regard to case, and null leaves a default.
    let ok = c.call(
        "textDocument/hover",
        json!({"TextDocument": {"URI": "untitled:1"}, "position": null}),
    );
    assert!(ok.error.is_none() && ok.result.is_none(), "{ok:?}");
}

#[test]
fn a_reply_carries_the_id_as_it_was_sent() {
    let out = Shared::default();
    let body = r#"{"jsonrpc":"2.0","id": "<a&b>" ,"method":"shutdown"}"#;
    let input = Cursor::new(format!("Content-Length: {}\r\n\r\n{body}", body.len()));
    let _ = serve(&CancelToken::new(), input, out.clone(), Options::default());
    let out = String::from_utf8(out.0.lock().unwrap().clone()).unwrap();
    // The angle brackets and the ampersand go out as escapes.
    let esc = |hex: &str| format!("{}u{hex}", '\\');
    let want = format!(
        r#"{{"jsonrpc":"2.0","id":"{}a{}b{}","error":{{"code":-32002,"message":"the server is not initialized"}}}}"#,
        esc("003c"),
        esc("0026"),
        esc("003e")
    );
    assert!(out.ends_with(&want), "reply = {out}");
}
