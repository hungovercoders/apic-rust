//! Helpers the local servers share: `apic demo` and `apic mock`.

use apic_httpc::CancelToken;
use apic_httpd::{Handler, Listener};
use apic_runner::Code;

use crate::root::{fail, CliError};

/// Binds 127.0.0.1:port. It runs before anything is written or printed,
/// so a port already in use fails first (E214 names the command).
pub(crate) fn listen_local(port: i64) -> Result<Listener, CliError> {
    if !(1..=65535).contains(&port) {
        return Err(fail(Code::Flag, "--port must be between 1 and 65535"));
    }
    Listener::bind(&format!("127.0.0.1:{port}")).map_err(|e| fail(Code::Server, e.to_string()))
}

/// Serves handler on the listener until `cancel` is cancelled (Ctrl-C),
/// then shuts down cleanly.
pub(crate) fn serve_local(cancel: &CancelToken, listener: Listener, handler: Handler) {
    apic_httpd::serve(listener, handler, cancel);
}
