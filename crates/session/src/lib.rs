//! Persists values captured from responses between apic invocations, so
//! `apic run login` followed by `apic run get-user` works the way it does
//! in a GUI client. State lives in `.apic/session.json` under the project
//! root; the directory ignores itself via `.apic/.gitignore`. The cookie
//! jar is beside it, in `.apic/cookies.json`.
//!
//! A [`Jar`] is a handle: cloning it gives another handle on the same
//! cookies, and the [`EnvJar`] of an environment stays attached to the
//! jar it came from. The HTTP side is plain data, with no HTTP crate
//! behind it: a request is a [`RequestUrl`], what a response set is its
//! `Set-Cookie` lines (or [`SetCookie`] values), and what to send is the
//! `Cookie` header value.

#![forbid(unsafe_code)]

mod cookiejar;
mod cookies;
mod session;
mod setcookie;

pub use apic_gocompat::Time;
pub use cookiejar::{RequestCookie, RequestUrl};
pub use cookies::{Cookie, EnvJar, Jar, COOKIE_FILE};
pub use session::{ensure_dir, Error, Store, DEFAULT_ENV, DIR, FILE};
pub use setcookie::{parse_set_cookie, read_set_cookies, SetCookie};

use std::path::Path;

/// Loads the session for a project root, or an empty one.
pub fn open(root: &Path) -> Result<Store, Error> {
    Store::open(root)
}

/// A session that is never written to disk.
pub fn new_memory() -> Store {
    Store::new_memory()
}

/// Loads the cookie jar for a project root, or an empty one.
pub fn open_jar(root: &Path) -> Result<Jar, Error> {
    Jar::open(root)
}

/// A cookie jar that is never written to disk.
pub fn new_memory_jar() -> Jar {
    Jar::new_memory()
}

#[cfg(test)]
mod cookies_tests;
#[cfg(test)]
mod session_tests;
