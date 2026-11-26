//! The HTTP client apic sends its requests with.
//!
//! The API is blocking: [`Client::send`] performs one round trip and
//! returns the response with its body read. Underneath, every client
//! shares one background thread that runs the connections, so a `Client`
//! is `Send + Sync`, cheap to build and cheap to clone; clones share a
//! connection pool, which is what makes a second request to the same
//! server report [`Timings::reused`].
//!
//! A client has one set of TLS settings ([`TlsConfig`]) and one choice of
//! protocols ([`Protocols`]); a caller that needs different settings per
//! host builds one client per distinct set and keeps them. Everything
//! else is per request: the proxy, the timeout, the cancellation token and
//! the cap on the body.
//!
//! `send` never follows a redirect. [`follow_redirects`] does, over
//! anything that implements [`RoundTrip`], with a policy of the caller's
//! own and an optional [`CookieJar`]; [`Client::execute`] is the same with
//! the default policy.
//!
//! Types are owned and plain: no lifetimes, no shared pointers in the
//! public API. [`Headers`] is the ordered multi-map used here and by the
//! `apic_httpd` server.

#![forbid(unsafe_code)]

mod cancel;
mod client;
mod error;
mod headers;
mod proxy;
mod redirect;
mod status;
mod target;
mod tls;

pub use cancel::CancelToken;
pub use client::{Client, ClientBuilder, Protocols, Request, Response, RoundTrip, Timings};
pub use error::{Error, ErrorKind, TlsFailure};
pub use headers::{canonical_header_key, Headers};
pub use proxy::{Proxy, ProxyScheme};
pub use redirect::{follow_redirects, CheckRedirect, CookieJar, RedirectAction, RedirectPolicy};
pub use status::status_text;
pub use tls::{TlsConfig, TlsConfigError};
