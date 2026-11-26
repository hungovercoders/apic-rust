//! Executes parsed requests: it resolves variables, sends the request,
//! evaluates captures and assertions, and persists captured values to the
//! session.
//!
//! # Sharing a runner
//!
//! A [`Runner`] is `Send + Sync` and every operation takes `&self`: the
//! terminal UI sends a request on one thread while it resolves and
//! describes others on its own, and the MCP and language servers answer
//! from whichever thread a call arrives on. Share one as `Arc<Runner>`.
//!
//! What a run changes (the captured values, the named responses, the
//! variables set with [`Runner::set_var`], the requests a target named,
//! the HTTP clients) sits behind locks inside the runner, each held only
//! for the read or the write, never across a request or a callback. What
//! configures a runner (`opts`, `stderr`, `interactive`, `progress`,
//! `on_result`, `sleep`, `now`, `zone`) is plain public fields: set them
//! after [`Runner::new`] and before the runner is shared. The callbacks
//! are `Fn + Send + Sync`; one that collects what it is told keeps its
//! own lock.
//!
//! The session is an `Arc<Mutex<apic_session::Store>>` ([`SharedSession`])
//! because a caller reads and clears it between runs; the cookie jar is
//! already a shared handle.
//!
//! # Requests, results and errors
//!
//! Requests are `Arc<apic_httpfile::Request>`, compared by identity, as
//! everywhere else. A [`Result`] is owned, plain data (cloning one shares
//! the response bytes); the results of the `# @ref` dependencies nest in
//! `deps`. [`Runner::run`] returns an [`Outcome`], the result and the
//! error side by side, since a request stopped by a dependency has both.
//! Every error is an [`Error`]; [`exit_code`], [`code_of`] and [`info`]
//! read its code and exit status from the [`CATALOGUE`].

#![forbid(unsafe_code)]

mod body;
mod describe;
mod errors;
mod gourl;
mod jbauth;
mod localtime;
mod proxy;
mod redact;
mod resolve;
mod result;
mod run;
mod runner;
mod stored;
mod tls;
mod vars;

#[cfg(test)]
mod tests;

pub use body::DEFAULT_MAX_BODY_BYTES;
pub use describe::Description;
pub use errors::*;
pub use jbauth::AuthConfigInfo;
pub use localtime::local_time;
pub use proxy::ProxyInfo;
pub use redact::redact_expr;
pub use result::{Body, FormPart, Iteration, Resolved, Response, Result, Timings, BASE64, MASKED};
pub use run::{history_key, Outcome};
pub use runner::{
    set_version, shared_session, sleep_cancel, version, NowFn, OnResultFn, Options, Progress,
    ProgressFn, Runner, SharedSession, SleepFn,
};
pub use stored::parse_result;
pub use tls::TlsInfo;
pub use vars::VarInfo;
