//! Loads environment variable files:
//!
//! ```text
//! http-client.env.json          {"dev": {"baseUrl": "..."}, "$shared": {...}}
//! http-client.private.env.json  same shape, for secrets, gitignored
//! .env                          KEY=value lines
//! ```
//!
//! These are the conventions used by JetBrains HTTP Client, kulala.nvim and
//! httpyac, so one set of files serves every tool.
//!
//! Maps are `BTreeMap`s, so everything iterates in key order.

#![forbid(unsafe_code)]

pub mod dotenv;
mod env;

pub use env::{
    load, AuthConfig, AuthFields, Environments, Error, SslConfig, DOT_ENV_FILE, PRIVATE_FILE,
    PUBLIC_FILE,
};

#[cfg(test)]
mod env_tests;
