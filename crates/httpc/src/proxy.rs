//! Proxies: the URL a caller names one with, the CONNECT tunnel of an HTTP
//! proxy and the SOCKS5 handshake.

use base64::Engine;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::error::{io_message, ErrorKind, Fail};
use crate::target::{percent_decode, split_host_port};

/// The kinds of proxy the client can go through.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ProxyScheme {
    /// An HTTP proxy reached in the clear.
    Http,
    /// An HTTP proxy reached over TLS.
    Https,
    /// A SOCKS5 proxy. The proxy resolves the target's name, as for
    /// `socks5h`.
    Socks5,
    /// A SOCKS5 proxy that resolves the target's name.
    Socks5h,
}

/// A proxy to send one request through.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Proxy {
    /// The kind of proxy.
    pub scheme: ProxyScheme,
    /// The proxy's host, an IPv6 address without brackets.
    pub host: String,
    /// The proxy's port: the one written, else 80, 443 or 1080 by scheme.
    pub port: u16,
    /// The user name of the URL's userinfo, decoded.
    pub username: Option<String>,
    /// The password of the URL's userinfo, decoded.
    pub password: Option<String>,
}

impl Proxy {
    /// Parses a proxy URL: `http://`, `https://`, `socks5://` or
    /// `socks5h://`, with optional `user:password@`.
    pub fn parse(raw: &str) -> Result<Proxy, String> {
        let bad = || format!("bad proxy URL {raw:?}");
        let (scheme, rest) = raw.trim().split_once("://").ok_or_else(bad)?;
        let scheme = match scheme.to_ascii_lowercase().as_str() {
            "http" => ProxyScheme::Http,
            "https" => ProxyScheme::Https,
            "socks5" => ProxyScheme::Socks5,
            "socks5h" => ProxyScheme::Socks5h,
            other => {
                return Err(format!(
                    "proxy scheme {other:?} not supported (http, https, socks5 or socks5h)"
                ))
            }
        };
        let authority = &rest[..rest.find(['/', '?', '#']).unwrap_or(rest.len())];
        let (userinfo, hostport) = match authority.rfind('@') {
            Some(i) => (Some(&authority[..i]), &authority[i + 1..]),
            None => (None, authority),
        };
        let (username, password) = match userinfo {
            None => (None, None),
            Some(u) => match u.split_once(':') {
                Some((user, pass)) => (Some(percent_decode(user)), Some(percent_decode(pass))),
                None => (Some(percent_decode(u)), None),
            },
        };
        let (host, port) = split_host_port(hostport).map_err(|_| bad())?;
        if host.is_empty() {
            return Err(bad());
        }
        let default_port = match scheme {
            ProxyScheme::Http => 80,
            ProxyScheme::Https => 443,
            ProxyScheme::Socks5 | ProxyScheme::Socks5h => 1080,
        };
        Ok(Proxy {
            scheme,
            host: host.to_ascii_lowercase(),
            port: port.unwrap_or(default_port),
            username,
            password,
        })
    }

    /// The `Proxy-Authorization` value for the proxy's credentials.
    pub(crate) fn basic_auth(&self) -> Option<String> {
        let user = self.username.as_ref()?;
        let pair = format!("{user}:{}", self.password.as_deref().unwrap_or(""));
        Some(format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(pair)
        ))
    }

    pub(crate) fn is_socks(&self) -> bool {
        matches!(self.scheme, ProxyScheme::Socks5 | ProxyScheme::Socks5h)
    }
}

/// Opens a tunnel to `target` (`host:port`) through an HTTP proxy that is
/// already connected.
pub(crate) async fn http_connect<S>(io: &mut S, target: &str, proxy: &Proxy) -> Result<(), Fail>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let broken = |e: std::io::Error| {
        Fail::new(
            ErrorKind::Connect,
            format!("proxyconnect tcp: {}", io_message(&e)),
        )
    };
    let mut head = format!(
        "CONNECT {target} HTTP/1.1\r\nHost: {target}\r\nUser-Agent: Go-http-client/1.1\r\n"
    );
    if let Some(auth) = proxy.basic_auth() {
        head.push_str(&format!("Proxy-Authorization: {auth}\r\n"));
    }
    head.push_str("\r\n");
    io.write_all(head.as_bytes()).await.map_err(broken)?;
    io.flush().await.map_err(broken)?;
    // Read a byte at a time: whatever follows the blank line belongs to
    // the tunnel.
    let mut reply = Vec::new();
    let mut byte = [0u8; 1];
    while !reply.ends_with(b"\r\n\r\n") {
        if reply.len() > 64 << 10 {
            return Err(Fail::new(
                ErrorKind::Proxy,
                "proxy sent an oversized CONNECT reply",
            ));
        }
        match io.read(&mut byte).await {
            Ok(0) => {
                return Err(Fail::new(
                    ErrorKind::Connect,
                    "proxyconnect tcp: unexpected EOF",
                ))
            }
            Ok(_) => reply.push(byte[0]),
            Err(e) => return Err(broken(e)),
        }
    }
    let text = String::from_utf8_lossy(&reply);
    let status_line = text.lines().next().unwrap_or("");
    let mut parts = status_line.splitn(3, ' ');
    let (_, code, reason) = (parts.next(), parts.next().unwrap_or(""), parts.next());
    if code == "200" {
        return Ok(());
    }
    // The proxy's reason is the error, as curl and browsers show it.
    Err(Fail::new(
        ErrorKind::Proxy,
        reason
            .filter(|r| !r.is_empty())
            .unwrap_or("unknown status code"),
    ))
}

/// Words the failures of one SOCKS5 exchange, which all name the proxy and
/// the target.
struct SocksFailure<'a> {
    proxy_addr: &'a str,
    target: String,
}

impl SocksFailure<'_> {
    fn fail(&self, why: &str) -> Fail {
        Fail::new(
            ErrorKind::Connect,
            format!(
                "socks connect tcp {}->{}: {why}",
                self.proxy_addr, self.target
            ),
        )
    }

    fn io(&self, e: std::io::Error) -> Fail {
        if e.kind() == std::io::ErrorKind::UnexpectedEof {
            self.fail("EOF")
        } else {
            self.fail(&io_message(&e))
        }
    }
}

/// Asks a SOCKS5 proxy that is already connected to connect to the target.
/// The host goes to the proxy as a name unless it is an IP address.
pub(crate) async fn socks5_connect<S>(
    io: &mut S,
    host: &str,
    port: u16,
    proxy: &Proxy,
    proxy_addr: &str,
) -> Result<(), Fail>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let failure = SocksFailure {
        proxy_addr,
        target: crate::target::join_host_port(host, port),
    };
    socks5_negotiate(io, proxy, &failure).await?;
    let msg = socks5_request(host, port).map_err(|why| failure.fail(why))?;
    io.write_all(&msg).await.map_err(|e| failure.io(e))?;
    io.flush().await.map_err(|e| failure.io(e))?;
    socks5_read_reply(io, &failure).await
}

/// Greets the proxy and, when it asks for them, sends the credentials.
async fn socks5_negotiate<S>(
    io: &mut S,
    proxy: &Proxy,
    failure: &SocksFailure<'_>,
) -> Result<(), Fail>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let with_auth = proxy.username.is_some();
    let greeting: &[u8] = if with_auth { &[5, 2, 0, 2] } else { &[5, 1, 0] };
    io.write_all(greeting).await.map_err(|e| failure.io(e))?;
    let mut choice = [0u8; 2];
    io.read_exact(&mut choice)
        .await
        .map_err(|e| failure.io(e))?;
    if choice[0] != 5 {
        return Err(failure.fail(&format!("unexpected protocol version {}", choice[0])));
    }
    match choice[1] {
        0 => Ok(()),
        2 if with_auth => socks5_authenticate(io, proxy, failure).await,
        _ => Err(failure.fail("no acceptable authentication methods")),
    }
}

/// The username and password exchange of RFC 1929.
async fn socks5_authenticate<S>(
    io: &mut S,
    proxy: &Proxy,
    failure: &SocksFailure<'_>,
) -> Result<(), Fail>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let user = proxy.username.as_deref().unwrap_or("");
    let pass = proxy.password.as_deref().unwrap_or("");
    if user.is_empty() || user.len() > 255 || pass.len() > 255 {
        return Err(failure.fail("invalid username/password"));
    }
    let mut msg = vec![1, user.len() as u8];
    msg.extend_from_slice(user.as_bytes());
    msg.push(pass.len() as u8);
    msg.extend_from_slice(pass.as_bytes());
    io.write_all(&msg).await.map_err(|e| failure.io(e))?;
    let mut reply = [0u8; 2];
    io.read_exact(&mut reply).await.map_err(|e| failure.io(e))?;
    if reply[1] != 0 {
        return Err(failure.fail("username/password authentication failed"));
    }
    Ok(())
}

/// The connect request for a target, or why it cannot be written.
fn socks5_request(host: &str, port: u16) -> Result<Vec<u8>, &'static str> {
    let mut msg = vec![5, 1, 0];
    match host.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(ip)) => {
            msg.push(1);
            msg.extend_from_slice(&ip.octets());
        }
        Ok(std::net::IpAddr::V6(ip)) => {
            msg.push(4);
            msg.extend_from_slice(&ip.octets());
        }
        Err(_) => {
            if host.len() > 255 {
                return Err("FQDN too long");
            }
            msg.push(3);
            msg.push(host.len() as u8);
            msg.extend_from_slice(host.as_bytes());
        }
    }
    msg.extend_from_slice(&port.to_be_bytes());
    Ok(msg)
}

/// What a SOCKS5 reply code other than success means.
fn socks5_reply_text(code: u8) -> &'static str {
    match code {
        1 => "general SOCKS server failure",
        2 => "connection not allowed by ruleset",
        3 => "network unreachable",
        4 => "host unreachable",
        5 => "connection refused",
        6 => "TTL expired",
        7 => "command not supported",
        8 => "address type not supported",
        _ => "unknown code",
    }
}

/// Reads the proxy's answer to the connect request.
async fn socks5_read_reply<S>(io: &mut S, failure: &SocksFailure<'_>) -> Result<(), Fail>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut reply = [0u8; 4];
    io.read_exact(&mut reply).await.map_err(|e| failure.io(e))?;
    if reply[0] != 5 {
        return Err(failure.fail(&format!("unexpected protocol version {}", reply[0])));
    }
    if reply[1] != 0 {
        let why = socks5_reply_text(reply[1]);
        return Err(failure.fail(&format!("unknown error {why}")));
    }
    // The bound address follows; read past it.
    let skip = match reply[3] {
        1 => 4,
        4 => 16,
        3 => {
            let mut len = [0u8; 1];
            io.read_exact(&mut len).await.map_err(|e| failure.io(e))?;
            len[0] as usize
        }
        other => return Err(failure.fail(&format!("unknown address type {other}"))),
    };
    let mut rest = vec![0u8; skip + 2];
    io.read_exact(&mut rest).await.map_err(|e| failure.io(e))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses() {
        let p = Proxy::parse("http://alice:s3cr%40t@Proxy.internal:3128").unwrap();
        assert_eq!(p.scheme, ProxyScheme::Http);
        assert_eq!((p.host.as_str(), p.port), ("proxy.internal", 3128));
        assert_eq!(p.basic_auth().unwrap(), "Basic YWxpY2U6czNjckB0");
        assert_eq!(Proxy::parse("socks5h://[::1]").unwrap().port, 1080);
        assert_eq!(Proxy::parse("https://p").unwrap().port, 443);
        assert!(Proxy::parse("ftp://p:21")
            .unwrap_err()
            .contains("not supported"));
        assert!(Proxy::parse("http://").is_err());
        assert!(Proxy::parse("proxy:3128").is_err());
    }
}
