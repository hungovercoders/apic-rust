//! Servers for tests: on a free loopback port, with an embedded
//! certificate for TLS.

use std::net::SocketAddr;

use crate::request::Request;
use crate::response::Response;
use crate::server::{Listener, Server, TlsOptions};

/// The certificates the test servers use, all valid until 2052. They are
/// for tests only: the keys are public.
pub mod testcerts {
    /// The test certificate authority. Trust this to trust `SERVER_CERT_PEM`.
    pub const CA_PEM: &str = include_str!("../testdata/ca.pem");
    /// The authority's private key, for a test that signs its own
    /// certificates.
    pub const CA_KEY_PEM: &str = include_str!("../testdata/ca-key.pem");
    /// The server certificate, signed by the authority, for `localhost`,
    /// `example.com`, `*.example.com`, `127.0.0.1` and `::1`.
    pub const SERVER_CERT_PEM: &str = include_str!("../testdata/server.pem");
    /// The server certificate's key.
    pub const SERVER_KEY_PEM: &str = include_str!("../testdata/server-key.pem");
    /// A client certificate signed by the authority, common name `alice`.
    pub const CLIENT_CERT_PEM: &str = include_str!("../testdata/client.pem");
    /// The client certificate's key.
    pub const CLIENT_KEY_PEM: &str = include_str!("../testdata/client-key.pem");
    /// A self-signed client certificate the authority knows nothing of,
    /// common name `mallory`.
    pub const STRANGER_CERT_PEM: &str = include_str!("../testdata/stranger.pem");
    /// The stranger's key.
    pub const STRANGER_KEY_PEM: &str = include_str!("../testdata/stranger-key.pem");
}

/// Settings for a `TestServer`.
#[derive(Clone, Debug)]
pub struct TestServerBuilder {
    addr: String,
    tls: Option<TlsOptions>,
}

impl Default for TestServerBuilder {
    fn default() -> TestServerBuilder {
        TestServerBuilder {
            addr: "127.0.0.1:0".to_string(),
            tls: None,
        }
    }
}

impl TestServerBuilder {
    /// Serves over TLS with the embedded test certificate, HTTP/1.1 only.
    pub fn tls(mut self) -> TestServerBuilder {
        let http2 = self.tls.as_ref().is_some_and(|t| t.http2);
        self.tls = Some(TlsOptions {
            http2,
            ..test_tls()
        });
        self
    }

    /// Offers HTTP/2 as well as HTTP/1.1 (or stops offering it). Turns
    /// TLS on, since HTTP/2 is only spoken over it.
    pub fn http2(mut self, on: bool) -> TestServerBuilder {
        let mut tls = self.tls.take().unwrap_or_else(test_tls);
        tls.http2 = on;
        self.tls = Some(tls);
        self
    }

    /// Requires every client to present a certificate signed by the test
    /// authority (`testcerts::CLIENT_CERT_PEM` is one). Turns TLS on.
    pub fn require_client_cert(mut self) -> TestServerBuilder {
        let mut tls = self.tls.take().unwrap_or_else(test_tls);
        tls.client_ca_pem = Some(testcerts::CA_PEM.as_bytes().to_vec());
        self.tls = Some(tls);
        self
    }

    /// Serves over TLS with a certificate and settings of the test's own.
    pub fn tls_options(mut self, tls: TlsOptions) -> TestServerBuilder {
        self.tls = Some(tls);
        self
    }

    /// Listens on another address than `127.0.0.1:0`.
    pub fn addr(mut self, addr: &str) -> TestServerBuilder {
        self.addr = addr.to_string();
        self
    }

    /// Starts the server. Panics when it cannot: a test has nothing
    /// better to do.
    pub fn start<H>(self, handler: H) -> TestServer
    where
        H: Fn(&Request) -> Response + Send + Sync + 'static,
    {
        let listener = match Listener::bind(&self.addr) {
            Ok(l) => l,
            Err(e) => panic!("test server: {e}"),
        };
        let mut builder = Server::builder();
        if let Some(tls) = self.tls {
            builder = builder.tls(tls);
        }
        match builder.start(listener, handler) {
            Ok(server) => TestServer { server },
            Err(e) => panic!("test server: {e}"),
        }
    }
}

fn test_tls() -> TlsOptions {
    TlsOptions {
        cert_pem: testcerts::SERVER_CERT_PEM.as_bytes().to_vec(),
        key_pem: testcerts::SERVER_KEY_PEM.as_bytes().to_vec(),
        client_ca_pem: None,
        http2: false,
    }
}

/// A server on a free loopback port for a test. It stops when dropped.
#[derive(Debug)]
pub struct TestServer {
    server: Server,
}

impl TestServer {
    /// Starts a plain HTTP server.
    pub fn new<H>(handler: H) -> TestServer
    where
        H: Fn(&Request) -> Response + Send + Sync + 'static,
    {
        TestServer::builder().start(handler)
    }

    /// Starts an HTTPS server with the embedded test certificate. It
    /// speaks HTTP/1.1 only; `TestServer::builder().http2(true)` offers
    /// HTTP/2 too.
    pub fn new_tls<H>(handler: H) -> TestServer
    where
        H: Fn(&Request) -> Response + Send + Sync + 'static,
    {
        TestServer::builder().tls().start(handler)
    }

    /// Starts building a test server with other settings.
    pub fn builder() -> TestServerBuilder {
        TestServerBuilder::default()
    }

    /// The base URL, without a trailing slash: `http://127.0.0.1:54321`.
    pub fn url(&self) -> String {
        self.server.url()
    }

    /// The address the server listens on.
    pub fn addr(&self) -> SocketAddr {
        self.server.addr()
    }

    /// The PEM a client must trust to verify a TLS test server: the test
    /// authority's certificate.
    pub fn cert_pem(&self) -> &'static str {
        testcerts::CA_PEM
    }

    /// Stops the server; dropping it does the same.
    pub fn close(&self) {
        self.server.shutdown();
    }
}
