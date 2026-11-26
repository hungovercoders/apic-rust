//! The cap on the response body.

use crate::Runner;

/// Caps how much of a response apic reads into memory when apic.yaml does
/// not say otherwise.
///
/// apic buffers a whole response before it can select, assert or render it, and
/// holds it more than once while doing so, so an unbounded read turns one
/// oversized or hostile response into a dead process. 64 MiB is far above any
/// response a human reads in a terminal and still leaves the tool honest about
/// what it cannot handle.
pub const DEFAULT_MAX_BODY_BYTES: i64 = 64 << 20;

impl Runner {
    pub(crate) fn max_body_bytes(&self) -> i64 {
        if self.opts.max_body_bytes > 0 {
            return self.opts.max_body_bytes;
        }
        if self.project.config.max_body_bytes > 0 {
            return self.project.config.max_body_bytes;
        }
        DEFAULT_MAX_BODY_BYTES
    }
}

/// The error for a response larger than apic reads: reported rather than
/// silently truncated, since a half-read body would produce assertions
/// and captures that quietly disagree with what the server actually sent.
pub(crate) fn body_too_large(max: i64) -> String {
    format!("response body too large: it exceeds {max} bytes; raise maxBodyBytes in apic.yaml to read it")
}
