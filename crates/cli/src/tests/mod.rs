//! The commands run in-process, on buffers, against local servers.

use std::path::Path;

use apic_httpc::CancelToken;
use apic_httpd::{Request, Response, TestServer};
use tempfile::TempDir;

use crate::{App, Buffer, Input};

mod adhoc;
mod data;
mod demo;
mod disabled;
mod errors;
mod firstrun;
mod fmt;
mod graphql;
mod history;
mod init;
mod jbauth;
mod lsp;
mod mcp_install;
mod mock;
mod output;
mod refs;
mod retry;
mod select;
mod session;
mod signal;
mod skill;
mod snippet;
mod suggest;
mod test;
mod ui;
mod validate;

/// Runs a command line and returns the exit code, stdout and stderr.
pub(crate) fn execute(args: &[&str]) -> (i32, String, String) {
    execute_with(&CancelToken::new(), None, args)
}

/// Runs a command line under a token the test may cancel, with stdin
/// when the command reads it.
pub(crate) fn execute_with(cancel: &CancelToken, stdin: Option<&str>, args: &[&str]) -> (i32, String, String) {
    let mut a = App::new();
    let (out, err) = (Buffer::new(), Buffer::new());
    a.stdout = out.output();
    a.stderr = err.output();
    if let Some(text) = stdin {
        a.stdin = Input::text(text);
    }
    let code = a.execute(cancel, args);
    (code, out.string(), err.string())
}

/// The same with the arguments owned, for lists built at run time.
pub(crate) fn execute_vec(args: &[String]) -> (i32, String, String) {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    execute(&args)
}

pub(crate) fn tmp() -> TempDir {
    tempfile::tempdir().expect("a temporary directory")
}

/// A directory's path as text.
pub(crate) fn s(dir: &TempDir) -> String {
    dir.path().to_str().expect("a UTF-8 path").to_string()
}

/// `dir/name` as text.
pub(crate) fn join(dir: impl AsRef<Path>, name: &str) -> String {
    dir.as_ref().join(name).to_str().expect("a UTF-8 path").to_string()
}

pub(crate) fn must_write(path: impl AsRef<Path>, content: &str) {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create the parent directory");
    }
    std::fs::write(path, content).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
}

pub(crate) fn must_read(path: impl AsRef<Path>) -> String {
    let path = path.as_ref();
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// A server that answers every request the same way.
pub(crate) fn server(f: impl Fn(&Request) -> Response + Send + Sync + 'static) -> TestServer {
    TestServer::new(f)
}

/// A server that answers nothing until it is dropped, so a request to it
/// is still in flight when the command is cancelled.
pub(crate) struct HangServer {
    done: CancelToken,
    srv: TestServer,
}

impl HangServer {
    pub(crate) fn new() -> HangServer {
        let done = CancelToken::new();
        let wait = done.clone();
        let srv = TestServer::new(move |_: &Request| {
            wait.wait();
            Response::new(200)
        });
        HangServer { done, srv }
    }

    pub(crate) fn url(&self) -> String {
        self.srv.url()
    }
}

impl Drop for HangServer {
    fn drop(&mut self) {
        self.done.cancel();
    }
}

/// A port nothing listens on.
pub(crate) fn free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind a free port");
    l.local_addr().expect("the bound address").port()
}

/// Cancels a token after a while, from another thread.
pub(crate) fn cancel_after(cancel: &CancelToken, after: std::time::Duration) {
    let cancel = cancel.clone();
    std::thread::spawn(move || {
        std::thread::sleep(after);
        cancel.cancel();
    });
}
