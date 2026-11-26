//! TLS settings and what a failed handshake is reported as.

use std::fmt;
use std::io;
use std::sync::{Arc, OnceLock};

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::{
    AlertDescription, CertificateError, ClientConfig, DigitallySignedStruct, RootCertStore,
    SignatureScheme,
};

use crate::error::{io_message, ErrorKind, Fail, TlsFailure};

/// The TLS settings of a client. The default trusts the system's roots,
/// verifies the server and presents no client certificate.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TlsConfig {
    /// PEM certificates trusted in addition to the system's roots.
    pub ca_pem: Option<Vec<u8>>,
    /// The client certificate (and any chain) in PEM, for mutual TLS.
    pub cert_pem: Option<Vec<u8>>,
    /// The client certificate's private key in PEM (PKCS #8, SEC 1 or
    /// PKCS #1). The key may sit in the same PEM as the certificate.
    pub key_pem: Option<Vec<u8>>,
    /// Skip verifying the server's certificate and its name.
    pub insecure: bool,
}

/// TLS settings that cannot be used: the caller's configuration is wrong,
/// not the network.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TlsConfigError {
    /// `ca_pem` holds no certificate that can be read.
    NoCaCertificates,
    /// The client certificate or its key cannot be used; the message says
    /// why (`tls: private key does not match public key`).
    ClientCertificate(String),
}

impl fmt::Display for TlsConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TlsConfigError::NoCaCertificates => f.write_str("no PEM certificates found"),
            TlsConfigError::ClientCertificate(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for TlsConfigError {}

/// A `TlsConfig` with its PEM read and checked.
pub(crate) struct TlsParts {
    extra_roots: Vec<CertificateDer<'static>>,
    client: Option<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)>,
    insecure: bool,
}

fn provider() -> Arc<CryptoProvider> {
    static PROVIDER: OnceLock<Arc<CryptoProvider>> = OnceLock::new();
    PROVIDER
        .get_or_init(|| Arc::new(rustls::crypto::ring::default_provider()))
        .clone()
}

/// The system's trusted roots, read once for the process.
fn system_roots() -> &'static RootCertStore {
    static ROOTS: OnceLock<RootCertStore> = OnceLock::new();
    ROOTS.get_or_init(|| {
        let mut store = RootCertStore::empty();
        let found = rustls_native_certs::load_native_certs();
        store.add_parsable_certificates(found.certs);
        store
    })
}

impl TlsParts {
    /// Reads and checks the PEM of a configuration.
    pub fn new(cfg: &TlsConfig) -> Result<TlsParts, TlsConfigError> {
        let mut extra_roots = Vec::new();
        if let Some(pem) = &cfg.ca_pem {
            let mut probe = RootCertStore::empty();
            for cert in rustls_pemfile::certs(&mut &pem[..]).flatten() {
                if probe.add(cert.clone()).is_ok() {
                    extra_roots.push(cert);
                }
            }
            if extra_roots.is_empty() {
                return Err(TlsConfigError::NoCaCertificates);
            }
        }
        let mut client = None;
        if let Some(cert_pem) = &cfg.cert_pem {
            let bad = |m: &str| TlsConfigError::ClientCertificate(m.to_string());
            let chain: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut &cert_pem[..])
                .flatten()
                .collect();
            if chain.is_empty() {
                return Err(bad("tls: failed to find any PEM data in certificate input"));
            }
            let key_pem = cfg.key_pem.as_ref().unwrap_or(cert_pem);
            let key = match rustls_pemfile::private_key(&mut &key_pem[..]) {
                Ok(Some(key)) => key,
                Ok(None) => return Err(bad("tls: failed to find any PEM data in key input")),
                Err(_) => return Err(bad("tls: failed to parse private key")),
            };
            let signer = provider()
                .key_provider
                .load_private_key(key.clone_key())
                .map_err(|_| bad("tls: failed to parse private key"))?;
            let certified = rustls::sign::CertifiedKey::new(chain.clone(), signer);
            if let Err(rustls::Error::InconsistentKeys(rustls::InconsistentKeys::KeyMismatch)) =
                certified.keys_match()
            {
                return Err(bad("tls: private key does not match public key"));
            }
            client = Some((chain, key));
        }
        Ok(TlsParts {
            extra_roots,
            client,
            insecure: cfg.insecure,
        })
    }

    /// Builds the rustls configuration offering the given ALPN protocols.
    pub fn client_config(&self, alpn: &[&str]) -> Result<ClientConfig, Fail> {
        let fail =
            |e: rustls::Error| Fail::new(ErrorKind::Tls(TlsFailure::Other), format!("tls: {e}"));
        let builder = ClientConfig::builder_with_provider(provider())
            .with_safe_default_protocol_versions()
            .map_err(fail)?;
        let builder = if self.insecure {
            builder
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(NoVerify(provider())))
        } else {
            let mut roots = system_roots().clone();
            for cert in &self.extra_roots {
                roots.add(cert.clone()).map_err(fail)?;
            }
            builder.with_root_certificates(roots)
        };
        let mut config = match &self.client {
            Some((chain, key)) => builder
                .with_client_auth_cert(chain.clone(), key.clone_key())
                .map_err(fail)?,
            None => builder.with_no_client_auth(),
        };
        config.alpn_protocols = alpn.iter().map(|p| p.as_bytes().to_vec()).collect();
        Ok(config)
    }
}

/// Accepts any server certificate, still checking the handshake's
/// signatures: what `insecure` asks for.
#[derive(Debug)]
struct NoVerify(Arc<CryptoProvider>);

impl ServerCertVerifier for NoVerify {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

/// Finds the TLS error inside an I/O error, if that is what it carries.
pub(crate) fn tls_error_in(err: &io::Error) -> Option<&rustls::Error> {
    err.get_ref()
        .and_then(|e| e.downcast_ref::<rustls::Error>())
}

/// The failure for an I/O error from a TLS stream: a TLS failure when it
/// carries one, else a broken connection.
pub(crate) fn io_fail(err: &io::Error, host: &str) -> Fail {
    if let Some(e) = tls_error_in(err) {
        return tls_fail(e, host);
    }
    if err.kind() == io::ErrorKind::UnexpectedEof {
        return Fail::other("EOF");
    }
    let text = err.to_string();
    if let Some(fail) = alert_in_text(&text) {
        return fail;
    }
    Fail::new(ErrorKind::Connect, io_message(err))
}

/// Finds a TLS alert in the text of an error. An HTTP/2 connection keeps
/// only the text of the I/O error that broke it, so that is where an
/// alert that arrived mid-connection has to be read from.
pub(crate) fn alert_in_text(text: &str) -> Option<Fail> {
    const MARK: &str = "received fatal alert: ";
    let name = text[text.find(MARK)? + MARK.len()..].trim();
    let alert = (0..=255u8)
        .map(AlertDescription::from)
        .find(|a| format!("{a:?}") == name);
    let message = match alert {
        Some(a) => alert_text(a),
        None => lower_first(name),
    };
    Some(Fail::new(
        ErrorKind::Tls(TlsFailure::Other),
        format!("remote error: tls: {message}"),
    ))
}

/// The failure for a TLS error, worded as apic's output has always had it.
pub(crate) fn tls_fail(err: &rustls::Error, host: &str) -> Fail {
    const VERIFY: &str = "tls: failed to verify certificate: x509: ";
    let (failure, message) = match err {
        rustls::Error::InvalidCertificate(cert) => match cert {
            CertificateError::UnknownIssuer => (
                TlsFailure::UnknownIssuer,
                format!("{VERIFY}certificate signed by unknown authority"),
            ),
            CertificateError::Expired
            | CertificateError::ExpiredContext { .. }
            | CertificateError::NotValidYet
            | CertificateError::NotValidYetContext { .. } => (
                TlsFailure::Expired,
                format!("{VERIFY}certificate has expired or is not yet valid"),
            ),
            CertificateError::NotValidForName => (
                TlsFailure::HostnameMismatch,
                format!("{VERIFY}certificate is not valid for {host}"),
            ),
            CertificateError::NotValidForNameContext { presented, .. } => (
                TlsFailure::HostnameMismatch,
                format!("{VERIFY}{}", name_mismatch(presented, host)),
            ),
            other => (
                TlsFailure::Other,
                format!("{VERIFY}{}", lower_first(&format!("{other:?}"))),
            ),
        },
        rustls::Error::AlertReceived(alert) => (
            TlsFailure::Other,
            format!("remote error: tls: {}", alert_text(*alert)),
        ),
        other => (
            TlsFailure::Other,
            format!("tls: {}", lower_first(&other.to_string())),
        ),
    };
    Fail::new(ErrorKind::Tls(failure), message)
}

/// Says which names a certificate is for when the one addressed is not
/// among them: only the IP addresses for an IP, only the names for a name.
fn name_mismatch(presented: &[String], host: &str) -> String {
    let is_ip = host.parse::<std::net::IpAddr>().is_ok();
    let mut names = Vec::new();
    for p in presented {
        // Each is shown as `DnsName("localhost")` or `IpAddress(127.0.0.1)`.
        let (kind, rest) = p.split_once('(').unwrap_or(("", p));
        let value = rest.trim_end_matches(')').trim_matches('"');
        if kind.starts_with("Ip") == is_ip {
            // An address in its usual short form: `::1`, not `0::1`.
            names.push(match value.parse::<std::net::IpAddr>() {
                Ok(ip) => ip.to_string(),
                Err(_) => value.to_string(),
            });
        }
    }
    match (names.is_empty(), is_ip) {
        (true, true) => {
            format!("cannot validate certificate for {host} because it doesn't contain any IP SANs")
        }
        (true, false) => {
            format!("certificate is not valid for any names, but wanted to match {host}")
        }
        (false, _) => format!("certificate is valid for {}, not {host}", names.join(", ")),
    }
}

fn lower_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_lowercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// The text of each TLS alert apic names.
const ALERTS: &[(AlertDescription, &str)] = &[
    (AlertDescription::CloseNotify, "close notify"),
    (AlertDescription::UnexpectedMessage, "unexpected message"),
    (AlertDescription::BadRecordMac, "bad record MAC"),
    (AlertDescription::DecryptionFailed, "decryption failed"),
    (AlertDescription::RecordOverflow, "record overflow"),
    (
        AlertDescription::DecompressionFailure,
        "decompression failure",
    ),
    (AlertDescription::HandshakeFailure, "handshake failure"),
    (AlertDescription::BadCertificate, "bad certificate"),
    (
        AlertDescription::UnsupportedCertificate,
        "unsupported certificate",
    ),
    (AlertDescription::CertificateRevoked, "revoked certificate"),
    (AlertDescription::CertificateExpired, "expired certificate"),
    (AlertDescription::CertificateUnknown, "unknown certificate"),
    (AlertDescription::IllegalParameter, "illegal parameter"),
    (AlertDescription::UnknownCA, "unknown certificate authority"),
    (AlertDescription::AccessDenied, "access denied"),
    (AlertDescription::DecodeError, "error decoding message"),
    (AlertDescription::DecryptError, "error decrypting message"),
    (AlertDescription::ExportRestriction, "export restriction"),
    (
        AlertDescription::ProtocolVersion,
        "protocol version not supported",
    ),
    (
        AlertDescription::InsufficientSecurity,
        "insufficient security level",
    ),
    (AlertDescription::InternalError, "internal error"),
    (
        AlertDescription::InappropriateFallback,
        "inappropriate fallback",
    ),
    (AlertDescription::UserCanceled, "user canceled"),
    (AlertDescription::NoRenegotiation, "no renegotiation"),
    (AlertDescription::MissingExtension, "missing extension"),
    (
        AlertDescription::UnsupportedExtension,
        "unsupported extension",
    ),
    (
        AlertDescription::CertificateUnobtainable,
        "certificate unobtainable",
    ),
    (AlertDescription::UnrecognisedName, "unrecognized name"),
    (
        AlertDescription::BadCertificateStatusResponse,
        "bad certificate status response",
    ),
    (
        AlertDescription::BadCertificateHashValue,
        "bad certificate hash value",
    ),
    (AlertDescription::UnknownPSKIdentity, "unknown PSK identity"),
    (
        AlertDescription::CertificateRequired,
        "certificate required",
    ),
    (
        AlertDescription::NoApplicationProtocol,
        "no application protocol",
    ),
];

/// The text of a TLS alert the peer sent.
fn alert_text(alert: AlertDescription) -> String {
    ALERTS
        .iter()
        .find(|(known, _)| *known == alert)
        .map_or_else(
            || format!("alert({alert:?})"),
            |(_, text)| (*text).to_string(),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mismatch_names() {
        let presented = vec![
            "DnsName(\"localhost\")".to_string(),
            "DnsName(\"example.com\")".to_string(),
            "IpAddress(127.0.0.1)".to_string(),
        ];
        assert_eq!(
            name_mismatch(&presented, "foo.test"),
            "certificate is valid for localhost, example.com, not foo.test"
        );
        assert_eq!(
            name_mismatch(&presented, "127.0.0.2"),
            "certificate is valid for 127.0.0.1, not 127.0.0.2"
        );
        let alert =
            alert_in_text("connection error: received fatal alert: CertificateRequired").unwrap();
        assert_eq!(alert.message, "remote error: tls: certificate required");
        assert!(alert_in_text("broken pipe").is_none());
        assert_eq!(
            name_mismatch(&presented[..2], "10.0.0.1"),
            "cannot validate certificate for 10.0.0.1 because it doesn't contain any IP SANs"
        );
    }

    #[test]
    fn bad_configuration() {
        let cfg = TlsConfig {
            ca_pem: Some(b"not pem".to_vec()),
            ..TlsConfig::default()
        };
        assert_eq!(
            TlsParts::new(&cfg).err(),
            Some(TlsConfigError::NoCaCertificates)
        );
        let cfg = TlsConfig {
            cert_pem: Some(b"nothing".to_vec()),
            ..TlsConfig::default()
        };
        assert_eq!(
            TlsParts::new(&cfg).err().map(|e| e.to_string()),
            Some("tls: failed to find any PEM data in certificate input".to_string())
        );
    }
}
