//! Turns a Postman collection (v2.1, or v2.0 where the shapes coincide)
//! and its environments into `.http` files and env files: folders become
//! files, requests become named requests, variables become
//! `http-client.env.json`, and the trivial forms of `pm.test` scripts
//! become `# @assert` and `# @capture` lines.
//!
//! The model is hand-written from the collection format, on purpose: it
//! reads only what the generator needs and tolerates the two shapes
//! (string or object) the format allows for several fields.
//!
//! Paths are text: [`Options::out_dir`], [`Options::env_files`] and the
//! paths in an [`ImportResult`] are `String`s, joined the way
//! `apic_project::path::join` joins. The model's types are plain owned
//! values; an absent object (`auth`, `body`, a folder's `request`) is
//! `None`.

#![forbid(unsafe_code)]

mod import;
mod json;
mod model;
mod scripts;

pub use import::{import, Error, ImportResult, Options, Unsupported};
pub use model::{
    looks_like_collection, looks_like_environment, Auth, AuthParam, Body, BodyFile, BodyOptions,
    Collection, Environment, Event, FormField, GraphQL, Header, Info, Item, Query, RawOptions,
    Request, Script, Url, Variable,
};
