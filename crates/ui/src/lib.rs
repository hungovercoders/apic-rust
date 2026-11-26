//! The interactive terminal UI behind `apic ui`: a request list on the
//! left, a tabbed detail pane on the right, and a status bar. The model is
//! driven entirely by messages and renders to a string, so tests call
//! [`Model::update`] and [`Model::view`] directly and need no terminal;
//! `program` is the only part that touches one.
//!
//! There is no TUI framework underneath. The terminal layer is apic's own:
//! `term` decodes keys, `input` reads them from a source whose pending
//! read can be interrupted, `program` is the event loop, and `sys` holds
//! the few calls into the operating system (raw mode, the window size,
//! `poll`), which is the one place in apic with `unsafe` code. Nothing
//! asks the terminal a question at start-up.
//!
//! # Ownership
//!
//! The model owns its state and is driven from one thread. The runner is
//! an `Arc<apic_runner::Runner>`: a request is sent on a worker thread
//! while the model goes on describing and resolving others on its own.
//! Requests are `Arc<apic_httpfile::Request>` compared by identity, as
//! everywhere else.
//!
//! # Without a terminal
//!
//! [`Model::resize`], [`Model::press`], [`Model::press_all`] and
//! [`Model::press_watch`] drive a model with no terminal and no event
//! loop, resolving the work a key starts inline. The tests use them, and
//! so does the screenshot generator, which is why a screenshot cannot
//! drift from what the UI draws.

#![deny(unsafe_code)]

mod editor;
mod headless;
mod history;
mod input;
#[cfg(unix)]
mod input_unix;
mod keys;
mod list;
mod panes;
mod program;
mod run;
mod sys;
mod term;
mod ui;
mod viewport;

pub use headless::parse_key;
pub use program::{quit, run, stdin_file, Cmd, Error, ExecMsg, Msg, PanicMsg};
pub use run::ResultMsg;
pub use term::{Key, KeyType};
pub use ui::{Config, Model, NewRunner};

#[cfg(test)]
mod tests;
