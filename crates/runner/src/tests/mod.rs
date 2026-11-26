//! Tests of the runner against local servers.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex, RwLock, RwLockReadGuard, RwLockWriteGuard};

use apic_httpc::CancelToken;
use apic_httpfile::Request;
use tempfile::TempDir;

use crate::{Error, Options, Result, Runner};

mod auth;
mod cookies;
mod errors;
mod flow;
mod graphql;
mod history;
mod jbauth;
mod json_shape;
mod multipart;
mod predicates;
mod prefix;
mod proto;
mod proxy;
mod refs;
mod retry;
mod run;
mod save;
mod timings;
mod tls;
mod vars;

/// Writes the files of a project into a fresh directory.
pub(crate) fn write_project(files: &[(&str, &str)]) -> TempDir {
    let dir = tempfile::tempdir().expect("a temporary directory");
    write_files(dir.path(), files);
    dir
}

pub(crate) fn write_files(dir: &Path, files: &[(&str, &str)]) {
    hold_env();
    for (name, content) in files {
        let p = dir.join(name);
        std::fs::create_dir_all(p.parent().expect("a parent")).expect("mkdir");
        std::fs::write(&p, content).expect("write");
    }
}

pub(crate) fn load(dir: &Path) -> Arc<apic_project::Project> {
    hold_env();
    apic_project::load(dir).expect("the project loads")
}

pub(crate) fn new_runner(dir: &Path, opts: Options) -> Runner {
    Runner::new(load(dir), opts).expect("a runner")
}

pub(crate) fn lookup(r: &Runner, name: &str) -> Arc<Request> {
    r.project.lookup(name).expect("the request exists")
}

pub(crate) fn ctx() -> CancelToken {
    CancelToken::new()
}

/// Runs a request by name and returns what `run` gave.
pub(crate) fn run(r: &Runner, name: &str) -> (Option<Result>, Option<Error>) {
    r.run(&ctx(), &lookup(r, name))
}

/// Runs a request that must not end in an error.
pub(crate) fn run_ok(r: &Runner, name: &str) -> Result {
    let (res, err) = run(r, name);
    if let Some(err) = err {
        panic!("{name}: {err}");
    }
    res.expect("a result")
}

/// Runs a request that must end in an error.
pub(crate) fn run_err(r: &Runner, name: &str) -> Error {
    run(r, name).1.unwrap_or_else(|| panic!("{name}: no error"))
}

pub(crate) fn vars(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

pub(crate) fn exit(err: Option<&Error>) -> i32 {
    crate::exit_code(err)
}

pub(crate) fn memory_session() -> crate::SharedSession {
    crate::shared_session(apic_session::new_memory())
}

/// The process environment belongs to every test at once: a test that
/// changes it holds this lock for writing, and every other test holds it
/// for reading from its first step to its end.
static ENV_LOCK: RwLock<()> = RwLock::new(());

thread_local! {
    static ENV_READ: RefCell<Option<RwLockReadGuard<'static, ()>>> = const { RefCell::new(None) };
}

/// Keeps the environment as it is until the calling test ends.
pub(crate) fn hold_env() {
    ENV_READ.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.is_none() && !ENV_WRITE.with(|w| w.get()) {
            *slot = Some(ENV_LOCK.read().unwrap_or_else(|e| e.into_inner()));
        }
    });
}

thread_local! {
    static ENV_WRITE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Sets environment variables for one test and puts them back after it.
pub(crate) struct EnvGuard {
    saved: Vec<(String, Option<String>)>,
    _lock: RwLockWriteGuard<'static, ()>,
}

impl EnvGuard {
    pub(crate) fn set(pairs: &[(&str, &str)]) -> EnvGuard {
        ENV_READ.with(|slot| slot.borrow_mut().take());
        let lock = ENV_LOCK.write().unwrap_or_else(|e| e.into_inner());
        ENV_WRITE.with(|w| w.set(true));
        let mut saved = Vec::new();
        for (k, v) in pairs {
            saved.push((k.to_string(), std::env::var(k).ok()));
            std::env::set_var(k, v);
        }
        EnvGuard { saved, _lock: lock }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (k, v) in &self.saved {
            match v {
                Some(v) => std::env::set_var(k, v),
                None => std::env::remove_var(k),
            }
        }
        ENV_WRITE.with(|w| w.set(false));
    }
}

/// A sleeper that records the waits it is asked for and returns at once.
#[derive(Default)]
pub(crate) struct FakeSleeper {
    pub waits: Mutex<Vec<std::time::Duration>>,
    pub err: Mutex<Option<crate::SleepError>>,
}

impl FakeSleeper {
    pub(crate) fn install(r: &mut Runner) -> Arc<FakeSleeper> {
        let f = Arc::new(FakeSleeper::default());
        let inner = f.clone();
        r.sleep = Box::new(move |_, d| {
            inner.waits.lock().unwrap().push(d);
            match *inner.err.lock().unwrap() {
                Some(e) => Err(e),
                None => Ok(()),
            }
        });
        f
    }

    pub(crate) fn waits(&self) -> Vec<std::time::Duration> {
        self.waits.lock().unwrap().clone()
    }
}

#[test]
fn runner_is_shareable() {
    fn check<T: Send + Sync>() {}
    check::<Runner>();
    check::<Result>();
    check::<Error>();
}
