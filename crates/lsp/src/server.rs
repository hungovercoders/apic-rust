//! The server: one client's session, from `initialize` to `exit`.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::fmt;
use std::io::{BufReader, Read, Write};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;

use apic_gocompat::io_error_text;
use apic_httpc::CancelToken;
use apic_httpfile::{File, Request};
use apic_project::{path, Project};
use apic_runner::Runner;
use serde_json::{json, Value};

use crate::jsonrpc::{
    read_message, to_json, Conn, Message, ReadError, RpcError, CODE_INVALID_PARAMS,
    CODE_INVALID_REQUEST, CODE_METHOD_NOT_FOUND, CODE_NOT_INITIALIZED,
};
use crate::protocol::{
    document_uri, lines, path_to_uri, uri_to_path, watched_uris, CodeDescription, Diagnostic,
    DidChangeParams, ExecuteCommandParams, InitializeParams, Mismatch, Node, Obj, Position,
    PublishDiagnosticsParams, Range, TextDocumentItem, TextDocumentPositionParams, Units,
    SEVERITY_ERROR, SEVERITY_WARNING,
};

/// Configures a server.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Options {
    /// The project root to use when the client names no workspace
    /// (`apic lsp -C dir`).
    pub root: String,
    /// The environment hover, completion and the run command use; the
    /// client's initializationOptions.env replaces it.
    pub env: String,
    /// Reported to the client in serverInfo.
    pub version: String,
    /// How long the server waits after an edit before it checks the
    /// project again, so a burst of keystrokes costs one check. Zero
    /// checks after every edit. A request (completion, hover, a lens) for
    /// a project with a check pending runs the check first.
    pub debounce: Duration,
}

/// What [`Error::NoShutdown`] says.
pub const ERR_NO_SHUTDOWN: &str = "the client exited without a shutdown request";

/// Why [`serve`] stopped, other than a shutdown request followed by exit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// The client exited, or closed the stream, without a shutdown
    /// request first; the protocol asks the server to exit 1 then.
    NoShutdown,
    /// The token `serve` was given was cancelled (Ctrl-C, SIGTERM).
    Cancelled,
    /// The stream broke: a message that could not be read or written.
    Io(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NoShutdown => f.write_str(ERR_NO_SHUTDOWN),
            Error::Cancelled => f.write_str("context canceled"),
            Error::Io(msg) => f.write_str(msg),
        }
    }
}

impl std::error::Error for Error {}

// Command names the server executes through workspace/executeCommand,
// offered by its code lenses. The arguments are the document URI and the
// request's target (file#name or file#N, relative to the project root).

/// Sends a request and answers with what `apic run --json` prints.
pub const COMMAND_RUN: &str = "apic.lsp.run";
/// Answers with what `apic describe --json` prints.
pub const COMMAND_DESCRIBE: &str = "apic.lsp.describe";
/// Answers with the request as a curl command, redacted.
pub const COMMAND_CURL: &str = "apic.lsp.curl";
/// Checks every project of the workspace now and answers once its
/// diagnostics are published. It takes no arguments.
pub const COMMAND_VALIDATE: &str = "apic.lsp.validate";

/// The files whose directory is a project root, as the VS Code extension
/// looks for them.
const MARKERS: [&str; 3] = [
    apic_project::CONFIG_FILE,
    apic_env::PUBLIC_FILE,
    apic_env::PRIVATE_FILE,
];

/// How often the loop looks at its cancel token while the input is quiet.
const POLL: Duration = Duration::from_millis(50);

/// What a handler answers a request with.
pub(crate) enum Reply {
    /// No result: JSON null.
    Null,
    /// The result, as JSON text.
    Json(String),
    /// A command that replies itself once it finishes.
    Deferred,
}

impl Reply {
    pub fn of<T: serde::Serialize + ?Sized>(v: &T) -> Reply {
        Reply::Json(to_json(v))
    }
}

pub(crate) type Handled = Result<Reply, RpcError>;

/// A check waiting out the debounce. Dropping it stops the wait.
pub(crate) struct Timer {
    id: u64,
    _stop: mpsc::Sender<()>,
}

/// One client's session.
pub(crate) struct Server {
    pub conn: Conn,
    pub opts: Options,
    /// Guards the session's state; a run started from a code lens and a
    /// debounced check finish on their own threads.
    mu: Mutex<State>,
    /// Runs and checks in flight.
    wg: Mutex<Vec<JoinHandle<()>>>,
    /// Cancelled on exit.
    pub run_cancel: CancelToken,
}

/// Everything the server knows about the session.
#[derive(Default)]
pub(crate) struct State {
    pub initialized: bool,
    pub shutdown: bool,
    /// Workspace folders, absolute.
    pub roots: Vec<String>,
    /// Workspace folder → the project root the client fixes for it.
    pub project_dirs: HashMap<String, String>,
    /// The environment in effect where no project has its own.
    pub env: String,
    /// The environment per project root, when the client picked one.
    pub envs: HashMap<String, String>,
    /// The client takes snippet completions.
    pub snippets: bool,
    /// The client lets the server register file watchers.
    pub watchable: bool,
    /// How the client counts columns.
    pub units: Units,
    /// Open documents by absolute path.
    pub docs: HashMap<String, String>,
    /// By project root.
    pub projects: HashMap<String, Arc<ProjectState>>,
    /// For describing and completing, by root + "\x00" + env.
    pub runners: HashMap<String, Arc<Runner>>,
    /// Checks waiting out the debounce, by root.
    pub pending: HashMap<String, Timer>,
    pub next_timer: u64,
    /// Last JSON body run here, by root, env and history key.
    pub bodies: HashMap<String, Value>,
    /// Path → the root whose check published diagnostics for it.
    pub published: HashMap<String, String>,
    /// Path → what was last published, so an unchanged set is not sent
    /// again.
    pub sent: HashMap<String, Vec<Diagnostic>>,
}

/// A loaded project and what the server derived from it.
pub(crate) struct ProjectState {
    pub root: String,
    pub p: Option<Arc<Project>>,
    /// Why the project could not be loaded, shown as a diagnostic.
    pub err: Option<String>,
}

fn load(root: &str, docs: &HashMap<String, String>) -> Arc<ProjectState> {
    let (p, err) = match apic_project::load_overlay(root, docs) {
        Ok(p) => (Some(p), None),
        Err(e) => (None, Some(e.to_string())),
    };
    Arc::new(ProjectState {
        root: root.to_string(),
        p,
        err,
    })
}

/// Runs a language server on `r` and `w` until the client sends exit or
/// closes the stream. It returns `Ok` after a shutdown request and an
/// exit notification, [`Error::NoShutdown`] when the client goes without
/// one, and [`Error::Cancelled`] once `cancel` is cancelled.
pub fn serve(
    cancel: &CancelToken,
    r: impl Read + Send + 'static,
    w: impl Write + Send + 'static,
    opts: Options,
) -> Result<(), Error> {
    let s = Arc::new(Server {
        conn: Conn::new(w),
        mu: Mutex::new(State {
            env: opts.env.clone(),
            ..State::default()
        }),
        opts,
        wg: Mutex::new(Vec::new()),
        run_cancel: CancelToken::new(),
    });
    let result = s.run_loop(cancel, r);
    // Checks still waiting will not run; runs in flight are cancelled and
    // waited for.
    s.lock().pending.clear();
    s.run_cancel.cancel();
    loop {
        let handles = std::mem::take(&mut *s.wg.lock().unwrap_or_else(|e| e.into_inner()));
        if handles.is_empty() {
            break;
        }
        for h in handles {
            let _ = h.join();
        }
    }
    result
}

impl Server {
    pub(crate) fn lock(&self) -> MutexGuard<'_, State> {
        self.mu.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Starts a thread `serve` waits for before it returns.
    pub(crate) fn spawn(&self, f: impl FnOnce() + Send + 'static) {
        let mut wg = self.wg.lock().unwrap_or_else(|e| e.into_inner());
        wg.retain(|h| !h.is_finished());
        wg.push(std::thread::spawn(f));
    }

    fn run_loop(
        self: &Arc<Self>,
        cancel: &CancelToken,
        r: impl Read + Send + 'static,
    ) -> Result<(), Error> {
        // Reads happen off the loop so a cancelled token (Ctrl-C, SIGTERM)
        // ends the server while the input is quiet, not only at the next
        // message.
        let (tx, reads) = mpsc::sync_channel::<Result<Message, ReadError>>(0);
        std::thread::spawn(move || {
            let mut r = BufReader::new(r);
            loop {
                let read = read_message(&mut r);
                // The connection is gone on anything but a message or one
                // that did not parse; the loop below returns on it.
                let gone = matches!(read, Err(ReadError::Eof | ReadError::Other(_)));
                if tx.send(read).is_err() || gone {
                    return;
                }
            }
        });
        loop {
            let read = loop {
                if cancel.is_cancelled() {
                    return Err(Error::Cancelled);
                }
                match reads.recv_timeout(POLL) {
                    Ok(read) => break read,
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => break Err(ReadError::Eof),
                }
            };
            let m = match read {
                Ok(m) => m,
                Err(ReadError::Eof) => return self.exit_err(),
                Err(ReadError::Rpc(e)) => {
                    let _ = self.conn.reply(None, Err(e));
                    continue;
                }
                Err(ReadError::Other(msg)) => return Err(Error::Io(msg)),
            };
            if m.method.is_empty() {
                continue; // a reply to a request the server sent (registerCapability)
            }
            if m.method == "exit" {
                return self.exit_err();
            }
            let result = match self.handle(&m) {
                Ok(Reply::Deferred) => continue, // a command that replies when it finishes
                Ok(Reply::Null) => Ok(None),
                Ok(Reply::Json(json)) => Ok(Some(json)),
                Err(e) => Err(e),
            };
            if let Some(id) = &m.id {
                self.conn
                    .reply(Some(id), result)
                    .map_err(|e| Error::Io(io_error_text(&e)))?;
            }
        }
    }

    fn exit_err(&self) -> Result<(), Error> {
        if self.lock().shutdown {
            return Ok(());
        }
        Err(Error::NoShutdown)
    }

    /// Dispatches one request or notification.
    fn handle(self: &Arc<Self>, m: &Message) -> Handled {
        let (ready, down) = {
            let st = self.lock();
            (st.initialized, st.shutdown)
        };
        if m.method == "initialize" {
            return self.initialize(m);
        }
        if !ready {
            if m.id.is_none() {
                return Ok(Reply::Null); // notifications before initialize are dropped
            }
            return Err(RpcError::new(
                CODE_NOT_INITIALIZED,
                "the server is not initialized",
            ));
        }
        if down && m.id.is_some() {
            return Err(RpcError::new(
                CODE_INVALID_REQUEST,
                "the server is shutting down",
            ));
        }
        // Each method the server handles, after initialize.
        match m.method.as_str() {
            "initialized" => self.on_initialized(),
            "shutdown" => self.on_shutdown(),
            "textDocument/didOpen" => self.on_did_open(m),
            "textDocument/didChange" => self.on_did_change(m),
            // The buffer already holds what was saved.
            "textDocument/didSave" => Ok(Reply::Null),
            "textDocument/didClose" => self.on_did_close(m),
            "workspace/didChangeWatchedFiles" => self.on_did_change_watched_files(m),
            "workspace/didChangeConfiguration" => self.on_did_change_configuration(m),
            "textDocument/completion" => self.on_completion(m),
            "textDocument/hover" => self.on_hover(m),
            "textDocument/codeLens" => self.on_code_lens(m),
            "textDocument/formatting" => self.on_formatting(m),
            "workspace/executeCommand" => self.on_execute_command(m),
            method => {
                if m.id.is_none() || method.starts_with("$/") {
                    return Ok(Reply::Null); // unknown notifications, and $/ requests, are ignored
                }
                Err(RpcError::new(
                    CODE_METHOD_NOT_FOUND,
                    format!("apic does not handle {method}"),
                ))
            }
        }
    }

    /// Registers the file watchers and checks the workspace.
    fn on_initialized(self: &Arc<Self>) -> Handled {
        self.register_watchers();
        self.validate_workspace();
        Ok(Reply::Null)
    }

    /// Marks the server as shutting down.
    fn on_shutdown(&self) -> Handled {
        self.lock().shutdown = true;
        Ok(Reply::Null)
    }

    /// Tracks a document the client opened.
    fn on_did_open(self: &Arc<Self>, m: &Message) -> Handled {
        let doc = decode(
            m,
            "lsp.didOpenParams",
            "didOpenParams",
            TextDocumentItem::read,
        )?;
        self.open(&doc.uri, doc.text);
        Ok(Reply::Null)
    }

    /// Applies an edit to an open document.
    fn on_did_change(self: &Arc<Self>, m: &Message) -> Handled {
        let p = decode(
            m,
            "lsp.didChangeParams",
            "didChangeParams",
            DidChangeParams::read,
        )?;
        self.change(p);
        Ok(Reply::Null)
    }

    /// Forgets a document the client closed.
    fn on_did_close(self: &Arc<Self>, m: &Message) -> Handled {
        let uri = decode(m, "lsp.didCloseParams", "didCloseParams", document_uri)?;
        self.close(&uri);
        Ok(Reply::Null)
    }

    /// Reacts to files changed outside the editor.
    fn on_did_change_watched_files(self: &Arc<Self>, m: &Message) -> Handled {
        let uris = decode(
            m,
            "lsp.didChangeWatchedFilesParams",
            "didChangeWatchedFilesParams",
            watched_uris,
        )?;
        self.watched(&uris);
        Ok(Reply::Null)
    }

    /// Takes the client's environment settings and drops the runners
    /// built for the old ones.
    fn on_did_change_configuration(&self, m: &Message) -> Handled {
        const APIC: &str =
            r#"struct { Env *string "json:\"env\""; Envs map[string]string "json:\"envs\"" }"#;
        let settings = format!(r#"struct {{ Apic {APIC} "json:\"apic\"" }}"#);
        let root = format!(r#"struct {{ Settings {settings} "json:\"settings\"" }}"#);
        let (env, envs) = decode(m, &root, "", |o| {
            let apic = o.object("settings", "", &settings).object("apic", "", APIC);
            (apic.opt_string("env"), apic.string_map("envs"))
        })?;
        let mut st = self.lock();
        if let Some(e) = env {
            st.env = e;
        }
        st.set_envs(&envs);
        st.runners.clear();
        Ok(Reply::Null)
    }

    /// Answers textDocument/completion once the document's check has run.
    fn on_completion(self: &Arc<Self>, m: &Message) -> Handled {
        let p = decode(
            m,
            "lsp.textDocumentPositionParams",
            "textDocumentPositionParams",
            TextDocumentPositionParams::read,
        )?;
        self.settle(&p.uri);
        self.completion(&p)
    }

    /// Answers textDocument/hover once the document's check has run.
    fn on_hover(self: &Arc<Self>, m: &Message) -> Handled {
        let p = decode(
            m,
            "lsp.textDocumentPositionParams",
            "textDocumentPositionParams",
            TextDocumentPositionParams::read,
        )?;
        self.settle(&p.uri);
        self.hover(&p)
    }

    /// Answers textDocument/codeLens once the document's check has run.
    fn on_code_lens(self: &Arc<Self>, m: &Message) -> Handled {
        let uri = decode(m, DOCUMENT_ONLY, "", document_uri)?;
        self.settle(&uri);
        Ok(match self.code_lenses(&uri) {
            Some(lenses) => Reply::of(&lenses),
            None => Reply::Null,
        })
    }

    /// Answers textDocument/formatting.
    fn on_formatting(&self, m: &Message) -> Handled {
        let uri = decode(m, DOCUMENT_ONLY, "", document_uri)?;
        Ok(match self.format(&uri) {
            Some(edits) => Reply::of(&edits),
            None => Reply::Null,
        })
    }

    /// Runs one of the server's commands.
    fn on_execute_command(self: &Arc<Self>, m: &Message) -> Handled {
        let p = decode(
            m,
            "lsp.executeCommandParams",
            "executeCommandParams",
            ExecuteCommandParams::read,
        )?;
        self.execute(m.id.as_deref(), &p)
    }

    fn initialize(&self, m: &Message) -> Handled {
        let p = decode(
            m,
            "lsp.initializeParams",
            "initializeParams",
            InitializeParams::read,
        )?;
        let mut st = self.lock();
        for uri in &p.workspace_folders {
            if let Some(path) = uri_to_path(uri) {
                st.roots.push(path);
            }
        }
        if st.roots.is_empty() {
            if let Some(path) = uri_to_path(&p.root_uri) {
                st.roots = vec![path];
            } else if !p.root_path.is_empty() {
                st.roots = vec![path::clean(&p.root_path)];
            } else if !self.opts.root.is_empty() {
                if let Ok(abs) = path::abs(&self.opts.root) {
                    st.roots = vec![abs];
                }
            }
        }
        if !p.env.is_empty() {
            st.env = p.env.clone();
        }
        st.set_envs(&p.envs);
        for (folder, root) in &p.project_roots {
            st.project_dirs
                .insert(path::clean(folder), path::clean(root));
        }
        st.snippets = p.snippet_support;
        let mut encoding = "utf-16";
        for e in &p.position_encodings {
            if e == "utf-8" {
                encoding = "utf-8";
                st.units = Units { utf8: true };
            }
        }
        st.initialized = true;
        st.watchable = p.dynamic_registration;
        let mut caps = json!({
            "positionEncoding": encoding,
            "textDocumentSync": {"openClose": true, "change": 1, "save": {"includeText": false}},
            "completionProvider": {
                "triggerCharacters": ["@", "{", ".", " ", "$"],
            },
            "hoverProvider": true,
            "executeCommandProvider": {"commands": [COMMAND_RUN, COMMAND_DESCRIBE, COMMAND_CURL, COMMAND_VALIDATE]},
        });
        if p.code_lens.unwrap_or(true) {
            caps["codeLensProvider"] = json!({"resolveProvider": false});
        }
        if p.formatting.unwrap_or(true) {
            caps["documentFormattingProvider"] = json!(true);
        }
        Ok(Reply::of(&json!({
            "capabilities": caps,
            "serverInfo": {"name": "apic", "version": self.opts.version},
        })))
    }

    /// Asks a client that can to tell the server about changes to request
    /// files, apic.yaml and the env files made outside the editor (a git
    /// checkout, another tool).
    fn register_watchers(&self) {
        if !self.lock().watchable {
            return;
        }
        let params = json!({"registrations": [{
            "id": "apic-watch", "method": "workspace/didChangeWatchedFiles",
            "registerOptions": {"watchers": [
                {"globPattern": "**/*.{http,rest}"},
                {"globPattern": "**/{apic.yaml,http-client.env.json,http-client.private.env.json,.env}"},
            ]},
        }]});
        let _ = self.conn.write(&Message {
            id: Some("\"apic-watch\"".to_string()),
            method: "client/registerCapability".to_string(),
            params: Some(to_json(&params)),
            ..Message::default()
        });
    }

    fn open(&self, uri: &str, text: String) {
        let Some(path) = uri_to_path(uri) else {
            return;
        };
        let root = {
            let mut st = self.lock();
            st.docs.insert(path.clone(), text);
            st.sent.remove(&path); // a client may have dropped them when the file closed
            st.root_for(&path)
        };
        self.refresh(&root); // a newly opened file gets its diagnostics at once
    }

    fn change(self: &Arc<Self>, mut p: DidChangeParams) {
        let Some(path) = uri_to_path(&p.uri) else {
            return;
        };
        // The server asks for full sync, so the last change is the whole text.
        let Some(text) = p.content_changes.pop() else {
            return;
        };
        let root = {
            let mut st = self.lock();
            st.docs.insert(path.clone(), text);
            st.root_for(&path)
        };
        self.schedule(&root);
    }

    fn close(self: &Arc<Self>, uri: &str) {
        let Some(path) = uri_to_path(uri) else {
            return;
        };
        let root = {
            let mut st = self.lock();
            st.docs.remove(&path);
            st.root_for(&path)
        };
        self.schedule(&root); // the file on disk is what counts now
    }

    /// Re-checks the projects a change outside the editor touched.
    fn watched(self: &Arc<Self>, uris: &[String]) {
        let roots: BTreeSet<String> = {
            let st = self.lock();
            uris.iter()
                .filter_map(|uri| uri_to_path(uri))
                .map(|path| st.root_for(&path))
                .collect()
        };
        for r in roots {
            self.schedule(&r);
        }
    }

    /// Checks a project once the edits stop for `Options::debounce`, or
    /// at once without a debounce.
    fn schedule(self: &Arc<Self>, root: &str) {
        let debounce = self.opts.debounce;
        if debounce.is_zero() {
            self.refresh(root);
            return;
        }
        let mut st = self.lock();
        st.next_timer += 1;
        let id = st.next_timer;
        let (stop, stopped) = mpsc::channel::<()>();
        let (s, key) = (Arc::clone(self), root.to_string());
        self.spawn(move || {
            // Anything but the time running out means the check was
            // replaced, settled early or abandoned.
            if !matches!(
                stopped.recv_timeout(debounce),
                Err(RecvTimeoutError::Timeout)
            ) {
                return;
            }
            let current = {
                let mut st = s.lock();
                let current = st.pending.get(&key).is_some_and(|t| t.id == id);
                if current {
                    st.pending.remove(&key);
                }
                current
            };
            if current {
                s.refresh(&key);
            }
        });
        // Replacing the timer that was waiting stops it.
        st.pending
            .insert(root.to_string(), Timer { id, _stop: stop });
    }

    /// Runs a pending check of a document's project now, so a request
    /// about the document sees its latest text.
    pub(crate) fn settle(&self, uri: &str) {
        let Some(path) = uri_to_path(uri) else {
            return;
        };
        let (root, pending) = {
            let mut st = self.lock();
            let root = st.root_for(&path);
            let pending = st.pending.remove(&root).is_some();
            (root, pending)
        };
        if pending {
            self.refresh(&root);
        }
    }

    /// Checks the project of every workspace folder, every project nested
    /// inside one, and every project a document has opened since,
    /// publishing diagnostics for files that are not open too, as `apic
    /// validate` in each project would report them.
    pub(crate) fn validate_workspace(&self) {
        let mut roots: VecDeque<String> = {
            let st = self.lock();
            // root_for walks up from a file; a name inside the folder
            // starts the walk at the folder itself.
            let mut roots: VecDeque<String> = st
                .roots
                .iter()
                .map(|folder| st.root_for(&path::join(folder, "_")))
                .collect();
            let mut loaded: Vec<String> = st.projects.keys().cloned().collect();
            loaded.sort();
            roots.extend(loaded);
            roots
        };
        let mut done = HashSet::new();
        while let Some(r) = roots.pop_front() {
            if !done.insert(r.clone()) {
                continue;
            }
            roots.extend(self.refresh(&r));
        }
    }

    /// Reloads a project with the open buffers in place of their files
    /// and publishes the diagnostics of the files it owns, clearing those
    /// that went away. A file under the root that belongs to a project
    /// nested inside it is that project's business: refresh returns those
    /// roots.
    pub(crate) fn refresh(&self, root: &str) -> Vec<String> {
        let mut st = self.lock();
        let ps = load(root, &st.docs);
        st.projects.insert(root.to_string(), Arc::clone(&ps));
        let prefix = format!("{root}\0");
        st.runners.retain(|k, _| !k.starts_with(&prefix));
        // root_for with its answers kept, for a check that asks about
        // many files.
        let mut seen: HashMap<String, String> = HashMap::new();
        let mut owner = |st: &State, path: &str| -> String {
            if let Some(r) = seen.get(path) {
                return r.clone();
            }
            let r = st.root_for(path);
            seen.insert(path.to_string(), r.clone());
            r
        };
        let mut nested: BTreeSet<String> = BTreeSet::new();
        let mut diags: BTreeMap<String, Vec<Diagnostic>> = BTreeMap::new();
        if let Some(err) = &ps.err {
            diags.insert(
                path::join(root, apic_project::CONFIG_FILE),
                vec![Diagnostic {
                    severity: SEVERITY_ERROR,
                    source: "apic".to_string(),
                    message: err.clone(),
                    ..Diagnostic::default()
                }],
            );
        } else if let Some(p) = &ps.p {
            for f in &p.files {
                let o = owner(&st, &path::join(root, &f.path));
                if o != root {
                    nested.insert(o);
                }
            }
            let mut texts: HashMap<String, String> = HashMap::new();
            for d in p.validate() {
                let file = path::join(root, &d.path);
                if owner(&st, &file) != root && path::dir(&file) != root {
                    continue; // a nested project's file: its own check publishes it
                }
                let text = texts.entry(file.clone()).or_insert_with(|| st.text(&file));
                let diag = to_diagnostic(st.units, &lines(text), &d);
                diags.entry(file).or_default().push(diag);
            }
        }
        // Every open file of the project gets an answer, an empty one when
        // it is clean, so an editor never keeps markers from before it
        // opened.
        let open: Vec<String> = st.docs.keys().cloned().collect();
        for file in open {
            if !diags.contains_key(&file) && owner(&st, &file) == root {
                diags.insert(file, Vec::new());
            }
        }
        for (file, by) in &st.published {
            if by == root && !diags.contains_key(file) {
                diags.insert(file.clone(), Vec::new());
            }
        }
        let mut send: Vec<(String, Vec<Diagnostic>)> = Vec::new();
        for (file, ds) in diags {
            if ds.is_empty() {
                st.published.remove(&file);
            } else {
                st.published.insert(file.clone(), root.to_string());
            }
            if st.sent.get(&file) == Some(&ds) {
                continue;
            }
            st.sent.insert(file.clone(), ds.clone());
            send.push((file, ds));
        }
        drop(st);

        for (file, diagnostics) in send {
            let _ = self.conn.notify(
                "textDocument/publishDiagnostics",
                &PublishDiagnosticsParams {
                    uri: path_to_uri(&file),
                    diagnostics,
                },
            );
        }
        nested.into_iter().collect()
    }
}

/// What the unnamed parameters of a method that takes only a document
/// are called when they are not an object.
const DOCUMENT_ONLY: &str =
    r#"struct { TextDocument lsp.textDocumentIdentifier "json:\"textDocument\"" }"#;

/// Reads a message's parameters with `read`; a value of the wrong type is
/// answered with the invalid-parameters error.
fn decode<T>(
    m: &Message,
    type_name: &str,
    strukt: &'static str,
    read: impl FnOnce(&Obj<'_>) -> T,
) -> Result<T, RpcError> {
    let invalid = |msg: String| RpcError::new(CODE_INVALID_PARAMS, msg);
    let Some(raw) = &m.params else {
        return Err(invalid("unexpected end of JSON input".to_string()));
    };
    let v = Node::parse(raw).map_err(invalid)?;
    let err = Mismatch::default();
    let out = read(&Obj::root(&v, type_name, strukt, &err));
    match err.into_inner() {
        Some((_, msg)) => Err(invalid(msg)),
        None => Ok(out),
    }
}

impl State {
    /// Records the environments the client picked per project root; an
    /// empty value goes back to the default.
    pub(crate) fn set_envs(&mut self, envs: &BTreeMap<String, String>) {
        for (root, e) in envs {
            let root = path::clean(root);
            if e.is_empty() {
                self.envs.remove(&root);
            } else {
                self.envs.insert(root, e.clone());
            }
        }
    }

    /// The environment a project runs in: the one the client picked for
    /// it, else the server's, else (when that is empty too) apic.yaml's,
    /// which the runner applies.
    pub(crate) fn env_for(&self, root: &str) -> String {
        self.envs.get(root).unwrap_or(&self.env).clone()
    }

    /// The project a file belongs to: inside a workspace folder, the root
    /// the client fixed for that folder, else the nearest directory above
    /// the file holding apic.yaml or an http-client env file without
    /// leaving the folder, else the folder; outside every folder, the
    /// file's own directory.
    pub(crate) fn root_for(&self, file: &str) -> String {
        let mut folder = "";
        for r in &self.roots {
            if apic_project::within(r, file).is_some() && r.len() > folder.len() {
                folder = r;
            }
        }
        if folder.is_empty() {
            return path::dir(file);
        }
        if let Some(fixed) = self.project_dirs.get(folder) {
            return fixed.clone();
        }
        let mut dir = path::dir(file);
        loop {
            for m in MARKERS {
                if std::fs::metadata(path::join(&dir, m)).is_ok() {
                    return dir;
                }
            }
            let parent = path::dir(&dir);
            if dir == folder || parent == dir {
                return folder.to_string();
            }
            dir = parent;
        }
    }

    /// A file's content: the open buffer, else what is on disk.
    pub(crate) fn text(&self, file: &str) -> String {
        if let Some(t) = self.docs.get(file) {
            return t.clone();
        }
        match std::fs::read(file) {
            Ok(data) => String::from_utf8_lossy(&data).into_owned(),
            Err(_) => String::new(),
        }
    }

    /// Returns the loaded project a document belongs to, loading it the
    /// first time.
    pub(crate) fn project_for(&mut self, file: &str) -> Arc<ProjectState> {
        let root = self.root_for(file);
        if let Some(ps) = self.projects.get(&root) {
            return Arc::clone(ps);
        }
        let ps = load(&root, &self.docs);
        self.projects.insert(root, Arc::clone(&ps));
        ps
    }

    /// A runner over a loaded project for describing and completing, even
    /// when a file has errors: those are the diagnostics' business, and
    /// the rest of the project is still worth describing. Nothing it does
    /// writes to disk. It is kept until the project is checked again or
    /// the environment changes.
    pub(crate) fn runner_for(&mut self, ps: &ProjectState) -> Result<Arc<Runner>, String> {
        if let Some(err) = &ps.err {
            return Err(err.clone());
        }
        let Some(p) = &ps.p else {
            return Err(String::new());
        };
        let env = self.env_for(&ps.root);
        let key = format!("{}\0{env}", ps.root);
        if let Some(r) = self.runners.get(&key) {
            return Ok(Arc::clone(r));
        }
        let mut p = Project::clone(p);
        p.diagnostics.clear();
        let opts = apic_runner::Options {
            env,
            no_history: true,
            ..apic_runner::Options::default()
        };
        let r = Arc::new(Runner::new(Arc::new(p), opts).map_err(|e| e.to_string())?);
        self.runners.insert(key, Arc::clone(&r));
        Ok(r)
    }
}

/// Converts apic's 1-based byte columns to the client's 0-based line and
/// character, given the file's lines. A diagnostic without a column
/// covers its whole line; one without a line, the start of the file.
fn to_diagnostic(units: Units, ls: &[&str], d: &apic_httpfile::Diagnostic) -> Diagnostic {
    let line_text = |n: usize| -> &str { ls.get(n).copied().unwrap_or("") };
    let mut out = Diagnostic {
        severity: SEVERITY_ERROR,
        source: "apic".to_string(),
        code: d.code.clone(),
        message: d.message.clone(),
        ..Diagnostic::default()
    };
    if d.severity == "warning" {
        out.severity = SEVERITY_WARNING;
    }
    if !d.code.is_empty() {
        out.code_description = Some(CodeDescription {
            href: "https://apic.sh/cli/#apic-validate".to_string(),
        });
    }
    if d.line == 0 {
        return out;
    }
    let start = d.line - 1;
    let text = line_text(start);
    if d.column == 0 {
        out.range = Range {
            start: Position {
                line: start as i64,
                character: 0,
            },
            end: Position {
                line: start as i64,
                character: units.to_client(text, text.len()),
            },
        };
        return out;
    }
    let end = if d.end_line > 0 {
        d.end_line - 1
    } else {
        start
    };
    let end_col = if d.end_column > 0 {
        d.end_column
    } else {
        line_text(end).len() + 1
    };
    out.range = Range {
        start: Position {
            line: start as i64,
            character: units.to_client(text, d.column - 1),
        },
        end: Position {
            line: end as i64,
            character: units.to_client(line_text(end), end_col - 1),
        },
    };
    out
}

/// The request whose block holds 0-based line `n` of a file: the blocks
/// are split by ### lines, as the parser splits them.
pub(crate) fn request_at(f: Option<&Arc<File>>, text: &str, n: i64) -> Option<Arc<Request>> {
    let f = f?;
    let ls = lines(text);
    let last = ls.len() as i64 - 1;
    let (mut start, mut end) = (0, last);
    let mut i = n.min(last);
    while i >= 0 {
        if ls[i as usize].starts_with("###") {
            start = i;
            break;
        }
        i -= 1;
    }
    let mut i = (n + 1).max(0);
    while i <= last {
        if ls[i as usize].starts_with("###") {
            end = i - 1;
            break;
        }
        i += 1;
    }
    f.requests
        .iter()
        .find(|r| {
            let at = r.line as i64 - 1;
            at >= start && at <= end
        })
        .cloned()
}

/// Returns the parsed file at `file` in a project.
pub(crate) fn file_of<'a>(ps: &'a ProjectState, file: &str) -> Option<&'a Arc<File>> {
    let p = ps.p.as_ref()?;
    let rel = apic_project::within(&ps.root, file)?;
    p.file(&path::to_slash(&rel))
}

/// A request's run target relative to its project: file#name, or file#N
/// for an unnamed one.
pub(crate) fn target(r: &Request) -> String {
    if !r.name.is_empty() {
        return format!("{}#{}", r.file.path, r.name);
    }
    format!("{}#{}", r.file.path, r.index)
}
