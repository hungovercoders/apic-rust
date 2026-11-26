//! Renders a self-contained HTML report of an `apic run` or an `apic
//! test`: one file, inline CSS, no JavaScript needed to read it, light and
//! dark through prefers-color-scheme. It is the artifact to attach to a CI
//! run or send to someone who does not read JUnit.
//!
//! The output is deterministic for a given input: no random ids, and the
//! time comes from the caller, so a report can be golden-tested.
//!
//! The page is `report.html.tmpl`, compiled into the binary and rendered
//! by the small template engine in `template`.

#![forbid(unsafe_code)]

mod html;
mod template;

pub use html::{features, run, Error, Meta};

#[cfg(test)]
mod html_tests;
