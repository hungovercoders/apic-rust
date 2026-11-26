//! Wires the apic commands together: the command tree, the flags every
//! command shares, and one module per command.
//!
//! [`App`] holds the streams a command reads and writes, so a test swaps
//! them and runs a command in-process; [`main`] is what the binary calls.
//! The tree itself is data ([`CommandSpec`], from [`App::spec`]), which is
//! what the parser, the help, the completion scripts and the
//! documentation generators are all built from.

#![deny(unsafe_code)]

mod completion;
mod curl;
mod demo;
mod describe;
mod env;
mod fmt;
mod help;
mod history;
mod import;
mod init;
mod io;
mod list;
mod lsp;
mod mcp;
mod mock;
mod parse;
mod root;
mod run;
mod select;
mod serve;
mod session;
mod shared;
mod signal;
mod skill;
mod snippet;
mod spec;
mod test;
mod ui;
mod validate;

pub use io::{Buffer, Input, Output, SharedWriter};
pub use root::{build_info, main, version, App, VersionInfo, VERSION};
pub use signal::{signal_context, SignalGuard};
pub use skill::skill_dirs;
pub use spec::{
    command_path, effective_flags, inherited_flags, use_line, ArgRule, CommandSpec, FlagKind,
    FlagSpec,
};

#[cfg(test)]
mod tests;
