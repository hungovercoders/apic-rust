//! Scaffolds `.http` files from an OpenAPI 3 document: one file per tag,
//! one named request per operation, path parameters as `{{vars}}` and an
//! example body derived from the schema. The same reader serves the
//! document back as a stateless mock, for `apic mock`.
//!
//! The reader is a hand-written walker over the document's YAML nodes
//! (JSON is YAML), resolving local `$ref` pointers as it goes; it reads
//! only what the generator and the mock need.
//!
//! Paths are text: [`Options::out_dir`] and the paths in an [`ImportResult`] are
//! `String`s, joined the way `apic_project::path::join` joins. A [`Mock`]
//! is immutable once built and is `Send + Sync`: share it behind an `Arc`
//! and call [`Mock::serve`], or hand it to a server with
//! [`Mock::into_handler`].

#![deny(unsafe_code)]

mod import;
mod mock;
mod model;
mod value;
mod yaml;

pub use import::{import, Error, ImportResult, Options};
pub use mock::{Mock, Route};
