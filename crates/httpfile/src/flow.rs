use std::time::Duration;

use apic_gocompat::{parse_duration, quote};

use crate::Request;

/// The HTTP/1.1 version a request line can ask for.
pub const HTTP1: &str = "HTTP/1.1";
/// The HTTP/2 version a request line can ask for.
pub const HTTP2: &str = "HTTP/2";

impl Request {
    /// Reports `# @disabled`: the request is skipped when its file runs as
    /// a flow, and still sent when it is asked for by name.
    pub fn disabled(&self) -> bool {
        self.directive("disabled").is_some()
    }

    /// How long `# @sleep <duration>` waits before the request is sent;
    /// zero without the directive.
    pub fn sleep(&self) -> Result<Duration, String> {
        match self.directive("sleep") {
            Some(v) => parse_sleep(v),
            None => Ok(Duration::ZERO),
        }
    }

    /// The HTTP version the request line asks for: `""` when it names
    /// none, [`HTTP1`] (HTTP/1.0 is sent as 1.1) or [`HTTP2`]. HTTP/3 and
    /// anything else is an error, which validate reports as
    /// bad-http-version.
    pub fn protocol(&self) -> Result<&'static str, String> {
        match self.http_version.trim().to_uppercase().as_str() {
            "" => Ok(""),
            "HTTP/1.1" | "HTTP/1.0" | "HTTP/1" => Ok(HTTP1),
            "HTTP/2" | "HTTP/2.0" => Ok(HTTP2),
            _ => Err(format!(
                "HTTP version {} is not supported: write HTTP/1.1 or HTTP/2, or leave it out",
                quote(&self.http_version)
            )),
        }
    }
}

/// Reads the value of `# @sleep`: a duration such as 500ms or 2s, not
/// negative.
pub fn parse_sleep(s: &str) -> Result<Duration, String> {
    parse_duration(s.trim())
        .map_err(|_| format!("want a duration such as 500ms or 2s, got {}", quote(s)))
}
