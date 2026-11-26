//! The small HTTP server behind apic's local servers and its tests.
//!
//! A handler is a plain synchronous function from a [`Request`] to a
//! [`Response`]: `Fn(&Request) -> Response + Send + Sync + 'static`. Both
//! are owned values with public fields; a request's body is an
//! `Arc<[u8]>` so that cloning a request (the [`Mux`] does, to add the
//! path values) never copies it.
//!
//! [`Server`] serves on a background thread of its own and stops on
//! [`Server::shutdown`], on drop, or when its stop token (an
//! `apic_httpc::CancelToken`) is cancelled; [`serve`] is the blocking
//! form for a command that is a server. [`Mux`] routes by method, host
//! and path patterns with `{wildcards}`. [`TestServer`] is a server on a
//! free loopback port for a test, with an embedded certificate for TLS
//! ([`testcerts`]).
//!
//! HTTP/1.1 is always spoken; HTTP/2 is offered over TLS when asked for
//! ([`TlsOptions::http2`]). Cleartext HTTP/2 is not.

#![forbid(unsafe_code)]

mod mux;
mod request;
mod response;
mod server;
mod sniff;
mod testserver;

pub use apic_httpc::{canonical_header_key, status_text, CancelToken, Headers};
pub use mux::{handler, Handler, Lookup, Mux};
pub use request::{certificate_common_name, parse_query, path_unescape, Part, Request, TlsState};
pub use response::{BodySender, Response};
pub use server::{serve, BindError, Listener, Server, ServerBuilder, TlsOptions};
pub use sniff::detect_content_type;
pub use testserver::{testcerts, TestServer, TestServerBuilder};
