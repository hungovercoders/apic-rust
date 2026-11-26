//! Renders a resolved request as code that sends the same thing without
//! apic: a curl command, an HTTPie command, PowerShell's
//! Invoke-RestMethod, Python requests, JavaScript fetch or Go net/http.
//!
//! Every language follows curl's rules. Without redact the snippet runs as
//! printed, credentials included. With redact the header values, body and
//! query values are masked and credentials come from the environment:
//! TOKEN for a bearer or OAuth2 token, APIC_USER and APIC_PASSWORD for
//! basic and digest, APIC_API_KEY for an API key. What a language cannot
//! do on its own (AWS SigV4 signing, digest in fetch) is said in a comment.
//!
//! The renderers borrow the [`apic_runner::Resolved`] they are given and
//! return owned text; nothing is shared or kept.

#![forbid(unsafe_code)]

mod curl;
mod gocode;
mod httpie;
mod js;
mod powershell;
mod python;
mod snippet;

#[cfg(test)]
mod curl_tests;
#[cfg(test)]
mod snippet_tests;
#[cfg(test)]
mod testutil;

pub use curl::curl;
pub use snippet::{render, Error, LANGUAGES};
