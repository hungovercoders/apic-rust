//! Exposes a project's requests to AI agents over the Model Context
//! Protocol (stdio, or streamable HTTP behind a token), so an agent can
//! discover and call an API without shelling out.
//!
//! The protocol is implemented here: JSON-RPC 2.0, one message per line
//! on stdio ([`serve`], or [`Server::run`] over any reader and writer),
//! and the streamable HTTP transport ([`handler`], [`serve_http`]) with a
//! session per client. Calls run on threads of their own, so a slow
//! request does not hold up a ping, and `notifications/cancelled` stops
//! the call it names.

#![forbid(unsafe_code)]

mod http;
mod protocol;
mod tools;

use std::io::{BufRead, Write};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use apic_gocompat::{json_string, path_error};
use apic_httpc::CancelToken;
use apic_project::path;
use apic_runner::Error;

pub use http::{check_bind, handler, serve_http};

use protocol::{lock, read_batch, Message, RpcError, Session, NO_BATCH_VERSION};

/// Controls the server.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Config {
    /// The project root.
    pub dir: String,
    /// The default environment.
    pub env: String,
    pub version: String,
}

const INSTRUCTIONS: &str = r##"apic serves HTTP requests defined in plain .http files in a project.
Start with list_requests to see what is available, describe_request to learn
which variables a request needs, then run_request to send it. Values declared
with "# @capture" (for example a login token) are stored in the session and
reused by later calls automatically, so run a login request once and then
call the requests that depend on it. A request that declares "# @ref login"
runs login by itself when the token is missing; the result then lists what
ran first under ran_first. run_file runs every request in a file in
order as a flow, skipping those marked disabled (their results say
skipped: disabled); run_request sends a disabled request all the same. run_features runs the project's Gherkin .feature files and
reports which steps failed. validate_project checks every .http file without
sending anything, with the line and column of each problem, so run it after
editing a file. curl_request gives the equivalent curl command for a request,
or with lang the same request as HTTPie, PowerShell, Python, JavaScript or Go.
Assertion failures come back as ok=false, not as errors."##;

/// How long calls that were read before the input ended are given to
/// answer.
const END_OF_INPUT_GRACE: Duration = Duration::from_secs(2);

/// A resource the server advertises: one of the project's .http files.
struct Resource {
    uri: String,
    name: String,
    description: String,
}

/// What every session of a server shares.
pub(crate) struct Inner {
    pub(crate) cfg: Config,
    /// The project root, absolute.
    pub(crate) root: String,
    /// The files listed when the server started, by URI.
    resources: Vec<Resource>,
}

impl Inner {
    fn resources_json(&self) -> Vec<String> {
        self.resources
            .iter()
            .map(|r| {
                format!(
                    "{{\"description\":{},\"mimeType\":\"text/plain\",\"name\":{},\"uri\":{}}}",
                    json_string(&r.description, false),
                    json_string(&r.name, false),
                    json_string(&r.uri, false)
                )
            })
            .collect()
    }

    /// Answers `resources/read`. Only a file that was listed is looked
    /// at; anything else is not found, whatever is on disk.
    fn read_resource(&self, uri: &str) -> Result<String, RpcError> {
        if !self.resources.iter().any(|r| r.uri == uri) {
            // The same answer for a resource that is not listed and one
            // that could not be found, so the listing is all a client
            // learns.
            return Err(RpcError {
                code: protocol::CODE_INVALID_PARAMS,
                message: "Resource not found".to_string(),
                data: Some(format!("{{\"uri\":{}}}", json_string(uri, false))),
            });
        }
        let text = read_file(&self.root, uri).map_err(|message| RpcError::new(0, message))?;
        Ok(format!(
            "{{\"ttlMs\":0,\"cacheScope\":\"public\",\"contents\":[{{\"uri\":{},\"mimeType\":\"text/plain\",\"text\":{}}}]}}",
            json_string(uri, true),
            json_string(&text, true)
        ))
    }
}

/// Reads the file a resource URI names, if the server is willing to serve
/// it.
pub(crate) fn read_file(root: &str, uri: &str) -> Result<String, String> {
    let real = resource_path(root, uri)?;
    // `real` is one of the project's own .http files.
    let data = std::fs::read(&real).map_err(|e| path_error("open", Path::new(&real), &e))?;
    Ok(String::from_utf8_lossy(&data).into_owned())
}

/// Resolves a resource URI to a file this server is willing to serve, or
/// an error. Only the project's own .http files qualify: the private env
/// file, .env and .apic/session.json all sit under the root and would
/// otherwise be readable by any client that guessed the path.
///
/// The allowlist is rebuilt per read rather than taken from the snapshot
/// the server registered, so a file added after the server started is
/// readable, matching the tools, which re-load the project on every call.
fn resource_path(root: &str, uri: &str) -> Result<String, String> {
    let Some(file) = uri.strip_prefix("file://") else {
        return Err(format!("no such resource: {uri}"));
    };
    let outside = || format!("resource outside project: {uri}");
    let native = file.replace('/', std::path::MAIN_SEPARATOR_STR);
    let abs = path::abs(&native).map_err(|_| outside())?;
    let root_real =
        path::eval_symlinks(root).map_err(|e| path_error("lstat", Path::new(root), &e))?;
    let real = match path::eval_symlinks(&abs) {
        Ok(real) => real,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(path_error("lstat", Path::new(&abs), &e));
        }
        Err(_) => return Err(outside()),
    };
    if apic_project::within(&root_real, &real).is_none() {
        return Err(outside());
    }
    let p = apic_project::load(root).map_err(|e| e.to_string())?;
    for f in &p.files {
        let known = path::join(
            &root_real,
            &f.path.replace('/', std::path::MAIN_SEPARATOR_STR),
        );
        if path::eval_symlinks(&known).is_ok_and(|known| known == real) {
            return Ok(real);
        }
    }
    // One message for "not a project file" and "does not exist", so the
    // server is not an existence oracle for files under the root.
    Err(format!("no such resource: {uri}"))
}

/// An MCP server for one project.
#[derive(Clone)]
pub struct Server {
    inner: Arc<Inner>,
}

/// Builds an MCP server for the project in `cfg.dir`.
pub fn new(cfg: Config) -> Result<Server, Error> {
    let root = path::abs(&cfg.dir)
        .map_err(|e| Error::Other(path_error("getwd", Path::new(&cfg.dir), &e)))?;
    let p = apic_project::load(&root).map_err(Error::Project)?;
    // This advertised list is a snapshot, so a file added later is not
    // listed until the server restarts. Reads are re-validated against the
    // project as it is on disk (see `resource_path`), so the two cannot
    // drift into serving something they should not.
    let mut resources: Vec<Resource> = p
        .files
        .iter()
        .map(|f| {
            let abs = path::join(&root, &f.path.replace('/', std::path::MAIN_SEPARATOR_STR));
            Resource {
                uri: format!("file://{}", path::to_slash(&abs)),
                name: f.path.clone(),
                description: format!("{} request(s)", f.requests.len()),
            }
        })
        .collect();
    resources.sort_by(|a, b| a.uri.cmp(&b.uri));
    Ok(Server {
        inner: Arc::new(Inner {
            cfg,
            root,
            resources,
        }),
    })
}

/// Runs the server over stdio until the client disconnects or `cancel`
/// is cancelled.
pub fn serve(cancel: &CancelToken, cfg: Config) -> Result<(), Error> {
    let server = new(cfg)?;
    let stdin = std::io::stdin();
    server
        .run(cancel, stdin.lock(), std::io::stdout())
        .map_err(Error::Other)
}

impl Server {
    /// Serves one client: a JSON-RPC message per line of `reader`, each
    /// response a line on `writer`. Returns when the reader ends or
    /// `cancel` is cancelled. A line that is not a message ends the
    /// session with the error.
    pub fn run(
        &self,
        cancel: &CancelToken,
        reader: impl BufRead,
        writer: impl Write + Send + 'static,
    ) -> Result<(), String> {
        let session = Arc::new(Session::new(self.inner.clone()));
        let writer: Arc<Mutex<dyn Write + Send>> = Arc::new(Mutex::new(writer));
        let pending = Arc::new(AtomicUsize::new(0));
        let result = std::thread::scope(|scope| {
            let result = self.read_loop(scope, cancel, &session, &writer, &pending, reader);
            // Calls already read get a moment to answer; those still
            // running after it are told to stop, and what they would have
            // answered is not written.
            let deadline = Instant::now() + END_OF_INPUT_GRACE;
            while result.is_ok()
                && pending.load(Ordering::SeqCst) > 0
                && Instant::now() < deadline
                && !cancel.is_cancelled()
            {
                std::thread::sleep(Duration::from_millis(2));
            }
            session.close();
            result
        });
        let _ = lock(&writer).flush();
        result
    }

    fn read_loop<'scope>(
        &self,
        scope: &'scope std::thread::Scope<'scope, '_>,
        cancel: &CancelToken,
        session: &Arc<Session>,
        writer: &Arc<Mutex<dyn Write + Send>>,
        pending: &Arc<AtomicUsize>,
        reader: impl BufRead,
    ) -> Result<(), String> {
        // Runs a piece of work on a thread of its own, counted until it
        // is done.
        let spawn = |work: Box<dyn FnOnce() + Send>| {
            let pending = pending.clone();
            pending.fetch_add(1, Ordering::SeqCst);
            scope.spawn(move || {
                work();
                pending.fetch_sub(1, Ordering::SeqCst);
            });
        };
        for line in reader.lines() {
            let Ok(line) = line else {
                break;
            };
            if cancel.is_cancelled() {
                break;
            }
            if line.trim().is_empty() {
                continue;
            }
            let (msgs, batch) = read_batch(&line).map_err(|err| {
                // A line that is not JSON at all reads as the JSON error.
                match apic_gocompat::check_json(line.as_bytes()) {
                    Err(json_err) => json_err,
                    Ok(()) => err,
                }
            })?;
            let version = session.protocol_version();
            if batch && version.as_str() >= NO_BATCH_VERSION {
                return Err(format!(
                    "JSON-RPC batching is not supported in {NO_BATCH_VERSION} and later (request version: {version})"
                ));
            }
            if batch {
                // A batch is answered as one: its responses in the order
                // of its calls.
                let (session, writer) = (session.clone(), writer.clone());
                spawn(Box::new(move || {
                    let responses: Vec<String> =
                        msgs.iter().filter_map(|m| session.handle(m)).collect();
                    if !responses.is_empty() {
                        write_line(&session, &writer, &format!("[{}]", responses.join(",")));
                    }
                }));
                continue;
            }
            for msg in msgs {
                let is_initialize =
                    matches!(&msg, Message::Request { method, .. } if method == "initialize");
                if msg.call_id().is_none() || is_initialize {
                    // Notifications are taken in order, and nothing
                    // overtakes `initialize`.
                    if let Some(response) = session.handle(&msg) {
                        write_line(session, writer, &response);
                    }
                    continue;
                }
                let (session, writer) = (session.clone(), writer.clone());
                spawn(Box::new(move || {
                    if let Some(response) = session.handle(&msg) {
                        write_line(&session, &writer, &response);
                    }
                }));
            }
        }
        Ok(())
    }
}

fn write_line(session: &Session, writer: &Arc<Mutex<dyn Write + Send>>, line: &str) {
    if session.closed.is_cancelled() {
        return;
    }
    let mut w = lock(writer);
    let _ = w.write_all(line.as_bytes());
    let _ = w.write_all(b"\n");
    let _ = w.flush();
}

#[cfg(test)]
mod tests;
