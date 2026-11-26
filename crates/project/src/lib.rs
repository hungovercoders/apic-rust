//! Discovers the `.http` files under a directory, reads the project's
//! `apic.yaml`, and indexes the requests by name.
//!
//! # Sharing and identity
//!
//! A [`Project`] is immutable once loaded and is handed out behind an
//! [`Arc`](std::sync::Arc): [`load`] and [`load_overlay`] return an
//! `Arc<Project>`, and the runner, the UI and the servers each keep a
//! clone of it. Its files and requests are the `Arc<File>` and
//! `Arc<Request>` of `apic_httpfile`: [`Project::lookup`],
//! [`Project::resolve`] and [`Project::requests`] return clones of those
//! `Arc`s, never copies of the requests, so a request found twice is the
//! same allocation and compares equal with `Arc::ptr_eq`.
//!
//! # Paths
//!
//! Paths are text. [`Project::root`] is the absolute path of the project
//! as a `String`; a file's `path` is relative to it and written with
//! forward slashes on every platform, and that is the form diagnostics
//! and request ids carry. The [`path`] module holds the operations on
//! such text (join, clean, relative-to) that the crate and its callers
//! share, so that they agree on what a path means.
//!
//! # Errors
//!
//! Loading and looking up return [`Error`]: a [`NotFoundError`], which
//! carries the names the user may have meant, or a plain message.
//! [`confine`] has its own [`ConfineError`], since callers tell a path
//! that leaves the root from one that cannot be read.

#![forbid(unsafe_code)]

mod config;
mod confine;
pub mod path;
mod project;
mod validate;
mod yaml;

pub use config::{
    AuthConfig, Config, ConfigField, FieldKind, TestConfig, TlsConfig, TlsHost, CONFIG_FIELDS,
    CONFIG_FILE,
};
pub use confine::{confine, real_prefix, ConfineError, ERR_OUTSIDE_ROOT};
pub use project::{discover, load, load_overlay, within, Error, NotFoundError, Project};
pub use yaml::{resolve as resolve_yaml_scalar, Resolved as YamlScalar};
