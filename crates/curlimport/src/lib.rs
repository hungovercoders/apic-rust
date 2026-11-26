//! Turns a curl command line into a `.http` request block, the reverse of
//! `apic curl`: the common flags map onto the request line, headers, body
//! and auth; the rest is reported, never fatal.
//!
//! [`parse`] reads the command into a [`Request`], an owned value with no
//! sharing; [`name`] derives a request name from it and [`render`] writes
//! the `###` block. [`splits_block`] tells the caller when an inline body
//! has to go to a side file instead.

#![forbid(unsafe_code)]

mod curl;
mod urlpath;

pub use curl::{name, parse, render, splits_block, Error, Header, Part, Request};
