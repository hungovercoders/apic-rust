//! The tests, and what they share: a model over the demo project served
//! by the demo API.

mod empty_tests;
mod golden_tests;
mod input_tests;
#[cfg(unix)]
mod input_unix_tests;
#[cfg(unix)]
mod program_tests;
mod sweep_tests;
mod term_tests;
mod ui_tests;

use std::cell::RefCell;
use std::io::{Read, Write};
use std::path::Path;
use std::rc::Rc;
use std::sync::{mpsc, Arc};

use apic_gocompat::quote;
use apic_output::style::{with_color_profile, Profile};
use apic_output::Theme;
use apic_runner::{shared_session, Options, Runner};
use tempfile::TempDir;

use crate::{parse_key, Cmd, Config, Key, Model, Msg};

/// Runs a test under the profile with no colour, where a view is its
/// text and nothing else.
pub(crate) fn ascii<R>(f: impl FnOnce() -> R) -> R {
    with_color_profile(Profile::Ascii, f)
}

pub(crate) struct Fixture {
    pub(crate) dir: TempDir,
    /// Kept so that the server lives as long as the model.
    pub(crate) _srv: apic_httpd::Server,
    pub(crate) m: Model,
    pub(crate) rebuilt: Rc<RefCell<Vec<String>>>,
}

pub(crate) fn write(path: &Path, text: &str) {
    std::fs::write(path, text).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
}

/// A runner over the project in `dir`, with a session kept in memory
/// unless the options bring one.
pub(crate) fn build_runner(
    dir: &Path,
    opts: &Options,
    env: &str,
) -> Result<Arc<Runner>, apic_runner::Error> {
    let p = apic_project::load(dir).map_err(apic_runner::Error::from)?;
    let mut o = opts.clone();
    o.env = env.to_string();
    if o.session.is_none() {
        o.session = Some(shared_session(apic_session::Store::new_memory()));
    }
    Ok(Arc::new(Runner::new(p, o)?))
}

/// Serves the demo API and writes the demo project against it, with two
/// environments, `local` and `other`, that both point at the server.
pub(crate) fn demo_project() -> (TempDir, apic_httpd::Server) {
    let srv = apic_httpd::Server::bind("127.0.0.1:0", apic_demoapi::new()).expect("bind");
    let dir = tempfile::tempdir().expect("tempdir");
    apic_demoapi::write_project(dir.path().to_str().expect("utf-8 path"), 1, true)
        .expect("write the demo project");
    let url = quote(&srv.url());
    let env = format!(
        "{{\n  \"local\": {{\"baseUrl\": {url}}},\n  \"other\": {{\"baseUrl\": {url}}}\n}}\n"
    );
    write(&dir.path().join("http-client.env.json"), &env);
    (dir, srv)
}

/// Serves the demo API, writes the demo project against it, and builds a
/// sized model on top.
pub(crate) fn new_fixture(opts: Options) -> Fixture {
    let (dir, srv) = demo_project();
    let rebuilt = Rc::new(RefCell::new(Vec::new()));
    let runner = build_runner(dir.path(), &opts, "local").expect("runner");
    let mut cfg = Config::new(runner);
    cfg.root = dir.path().to_str().expect("utf-8 path").to_string();
    cfg.env = "local".to_string();
    cfg.editor = "vim".to_string();
    cfg.redact = opts.redact;
    cfg.theme = Theme::default();
    let (root, log) = (dir.path().to_path_buf(), rebuilt.clone());
    cfg.new_runner = Some(Box::new(move |env| {
        log.borrow_mut().push(env.to_string());
        build_runner(&root, &opts, env)
    }));
    let mut m = Model::new(cfg);
    m.update(Msg::Size {
        width: 120,
        height: 40,
    });
    Fixture {
        dir,
        _srv: srv,
        m,
        rebuilt,
    }
}

/// A pipe in memory: what is written to one end is read from the other,
/// and dropping the writer ends the reader's input.
pub(crate) fn memory_pipe() -> (PipeReader, PipeWriter) {
    let (tx, rx) = mpsc::channel();
    (
        PipeReader {
            rx,
            rest: Vec::new(),
        },
        PipeWriter(tx),
    )
}

pub(crate) struct PipeReader {
    rx: mpsc::Receiver<Vec<u8>>,
    rest: Vec<u8>,
}

pub(crate) struct PipeWriter(mpsc::Sender<Vec<u8>>);

impl Read for PipeReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.rest.is_empty() {
            match self.rx.recv() {
                Ok(chunk) => self.rest = chunk,
                Err(_) => return Ok(0),
            }
        }
        let n = self.rest.len().min(buf.len());
        buf[..n].copy_from_slice(&self.rest[..n]);
        self.rest.drain(..n);
        Ok(n)
    }
}

impl Write for PipeWriter {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0
            .send(b.to_vec())
            .map_err(|_| std::io::Error::from(std::io::ErrorKind::BrokenPipe))?;
        Ok(b.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub(crate) fn kp(s: &str) -> Key {
    parse_key(s)
}

impl Fixture {
    /// Feeds keys to the model and returns what the last one scheduled,
    /// unresolved.
    pub(crate) fn press(&mut self, keys: &[&str]) -> Option<Cmd> {
        let mut cmd = None;
        for k in keys {
            cmd = self.m.update(Msg::Key(kp(k)));
        }
        cmd
    }

    /// Feeds the characters of `text` as keys.
    pub(crate) fn type_text(&mut self, text: &str) {
        for r in text.chars() {
            self.m.update(Msg::Key(Key::rune(r)));
        }
    }

    /// Resolves one command: work scheduled off the loop is run inline
    /// and its message fed back to the model. It returns the next
    /// command, if any.
    pub(crate) fn step(&mut self, cmd: Option<Cmd>) -> Option<Cmd> {
        match cmd?() {
            None | Some(Msg::Quit) => None,
            Some(Msg::Async(work)) => self.m.update(work()),
            Some(msg) => self.m.update(msg),
        }
    }

    /// Resolves a whole command chain, such as a flow running request by
    /// request.
    pub(crate) fn drain(&mut self, cmd: Option<Cmd>) {
        self.m.settle(cmd);
    }

    pub(crate) fn view(&mut self) -> String {
        self.m.view()
    }

    pub(crate) fn selected_id(&self) -> String {
        self.m.selected().map(|r| r.id()).unwrap_or_default()
    }

    pub(crate) fn size(&mut self, width: i32, height: i32) {
        self.m.update(Msg::Size { width, height });
    }
}
