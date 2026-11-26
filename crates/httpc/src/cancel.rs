//! Cancelling a request that is in flight.

use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use tokio::sync::Notify;

/// A handle that aborts the requests it was given to. Clones share one
/// state: cancelling any of them cancels all, and a token stays cancelled.
#[derive(Clone, Debug, Default)]
pub struct CancelToken {
    inner: Arc<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    cancelled: Mutex<bool>,
    cond: Condvar,
    notify: Notify,
}

impl CancelToken {
    /// A token that has not been cancelled.
    pub fn new() -> CancelToken {
        CancelToken::default()
    }

    /// Cancels: every request holding the token fails with
    /// `ErrorKind::Cancelled`, and every `wait_timeout` returns.
    pub fn cancel(&self) {
        *self.inner.cancelled.lock().unwrap_or_else(|e| e.into_inner()) = true;
        self.inner.cond.notify_all();
        self.inner.notify.notify_waiters();
    }

    /// Reports whether the token has been cancelled.
    pub fn is_cancelled(&self) -> bool {
        *self.inner.cancelled.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Blocks for `d` or until the token is cancelled, whichever is first,
    /// and reports whether it was cancelled: a sleep that Ctrl-C ends.
    pub fn wait_timeout(&self, d: Duration) -> bool {
        let guard = self.inner.cancelled.lock().unwrap_or_else(|e| e.into_inner());
        let (guard, _) = self
            .inner
            .cond
            .wait_timeout_while(guard, d, |cancelled| !*cancelled)
            .unwrap_or_else(|e| e.into_inner());
        *guard
    }

    /// Blocks until the token is cancelled.
    pub fn wait(&self) {
        let guard = self.inner.cancelled.lock().unwrap_or_else(|e| e.into_inner());
        drop(
            self.inner
                .cond
                .wait_while(guard, |cancelled| !*cancelled)
                .unwrap_or_else(|e| e.into_inner()),
        );
    }

    /// Resolves once the token is cancelled: the same wait for
    /// asynchronous code, which is how the client and the server watch it.
    pub async fn cancelled(&self) {
        loop {
            let notified = self.inner.notify.notified();
            tokio::pin!(notified);
            // Register before checking, so a cancel between the check and
            // the wait is not missed.
            notified.as_mut().enable();
            if self.is_cancelled() {
                return;
            }
            notified.await;
        }
    }
}
