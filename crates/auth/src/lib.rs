//! Authentication for outgoing requests, driven by the `# @auth <type>
//! [args]` directive or the `auth.default` setting in apic.yaml. Supported
//! types: none, bearer, basic, apikey, digest, aws, oauth2, exec.
//!
//! A [`Spec`] is parsed with [`parse`], rendered with [`Spec::render`] and
//! applied to an [`apic_httpc::Request`] with [`apply`]. Digest is the one
//! type answered on the wire: [`DigestTransport`] is a layer over the
//! client that answers the server's challenge.
//!
//! Sharing: a `Spec` is plain owned data, cloned freely. What [`Env`]
//! borrows from its caller is held behind `Arc` (the token cache, the
//! client for token endpoints, the writer for sign-in prompts) or as a
//! boxed closure (the clock, the browser opener, the environment lookup),
//! so no public type carries a lifetime. A [`DigestState`] is shared as
//! `Arc<DigestState>` between the transports of one invocation.

#![forbid(unsafe_code)]

mod apply;
mod authcode;
mod awscreds;
mod command;
mod digest;
mod error;
mod jetbrains;
mod oauth2;
mod sigv4;
mod spec;
mod urlutil;

pub use apply::{apply, Cache, Env, Stderr};
pub use authcode::{open_browser, ERR_NOT_INTERACTIVE};
pub use digest::{digest_host, DigestState, DigestTransport};
pub use error::Error;
pub use jetbrains::{from_jetbrains, JetBrains};
pub use oauth2::{cache_key, describe_cached, oauth2_tokens, Tokens};
pub use sigv4::{sign_sig_v4, AwsCredentials};
pub use spec::{parse, type_description, type_names, Spec, TYPES};

#[cfg(test)]
mod auth_tests;
#[cfg(test)]
mod authcode_tests;
#[cfg(test)]
mod digest_tests;
#[cfg(test)]
mod jetbrains_tests;
