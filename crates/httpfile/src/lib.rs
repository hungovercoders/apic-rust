//! Parses the common subset of the `.http` request file format used by
//! VS Code REST Client, JetBrains HTTP Client, kulala.nvim and httpyac,
//! plus apic's own `# @capture` / `# @assert` comment directives, and
//! formats such files in their canonical form.
//!
//! # Sharing and identity
//!
//! The AST is immutable once [`parse`] returns, and it is shared widely:
//! a project keeps every file, the runner keeps the requests it has run,
//! the UI keeps the ones it shows. So the two types that are passed around
//! are handed out behind [`Arc`](std::sync::Arc):
//!
//! - [`parse`] and [`parse_file`] return an `Arc<File>`;
//! - [`File::requests`] is a `Vec<Arc<Request>>`: clone the `Arc` to keep
//!   a request, never the `Request` itself;
//! - a request reaches its file through [`Request::file`], an
//!   `Arc<FileInfo>` holding the file's path and variables. [`File`]
//!   dereferences to the same [`FileInfo`], so `file.path` and
//!   `request.file.path` read the same value and there is no reference
//!   cycle to leak.
//!
//! Two requests are *the same request* when they are the same allocation,
//! not when their fields are equal (two files may hold identical blocks):
//! compare with `Arc::ptr_eq`, and key maps and sets with [`RequestKey`],
//! which hashes and compares by that identity. `Request` deliberately has
//! no `PartialEq`.
//!
//! Lines and columns are 1-based `usize` values; a column counts bytes
//! from the start of the line, and zero means "none".
//!
//! Errors from the small helpers ([`parse_retry`], [`parse_sleep`],
//! [`Request::protocol`], [`Request::multipart`], [`Request::graphql`],
//! [`graphql_envelope`]) are the message as a `String`: callers only ever
//! report them.

#![forbid(unsafe_code)]

mod ast;
mod flow;
mod format;
mod graphql;
mod mime;
mod multipart;
mod parse;
mod retry;

pub use ast::{
    Assert, Capture, Directive, File, FileInfo, Header, Ref, Request, RequestKey, SaveTo, Var,
};
pub use flow::{parse_sleep, HTTP1, HTTP2};
pub use format::{directive_order, format, format_request};
pub use graphql::{graphql_envelope, split_graphql, GRAPHQL_HEADER};
pub use mime::parse_media_type;
pub use multipart::{quote_param, FileRef, Multipart, Part};
pub use parse::{
    code_description, is_known_directive, known_directive, parse, parse_file, span,
    valid_capture_name, Diagnostic, CODES, KNOWN_DIRECTIVES,
};
pub use retry::{parse_retry, DEFAULT_RETRY_INTERVAL};
