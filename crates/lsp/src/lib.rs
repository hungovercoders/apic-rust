//! apic's language server: `apic lsp` speaks the Language Server Protocol
//! over stdio, so any editor with an LSP client gets the diagnostics,
//! completion, hover, code lenses and formatting the VS Code extension
//! has, from the same code the CLI runs.
//!
//! The transport is JSON-RPC 2.0 with the protocol's Content-Length
//! framing, written here in a couple of hundred lines rather than pulled
//! in as a library, in the spirit of the SigV4 signer and the OpenAPI
//! reader.
//!
//! # Threads
//!
//! [`serve`] handles the client's messages one at a time, in the order
//! they arrive, on the thread that called it. Three things happen
//! elsewhere: a reader thread turns the input into messages (so a
//! cancelled token ends `serve` while the input is quiet), a request a
//! code lens runs is sent on a thread of its own (so the editor keeps its
//! completions meanwhile) and answers when it finishes, and a debounced
//! check waits on a thread of its own. Everything the server knows is
//! behind one lock; writes to the client are serialised by another.
//! `serve` returns only after the runs and checks in flight have ended.

#![forbid(unsafe_code)]

mod commands;
mod complete;
mod hover;
mod jsonrpc;
mod protocol;
mod server;

#[cfg(test)]
mod server_tests;

pub use complete::OPERATORS;
pub use server::{
    serve, Error, Options, COMMAND_CURL, COMMAND_DESCRIBE, COMMAND_RUN, COMMAND_VALIDATE,
    ERR_NO_SHUTDOWN,
};
