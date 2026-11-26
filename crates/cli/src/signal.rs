//! Interrupts: the first Ctrl-C or SIGTERM cancels the run in progress
//! instead of ending the process. This is the one module that talks to
//! the operating system directly.
#![allow(unsafe_code)]

use apic_httpc::CancelToken;

/// What [`signal_context`] hands back: the token the commands run under,
/// and the means to stop listening.
pub struct SignalGuard {
    token: CancelToken,
}

impl SignalGuard {
    /// The token an interrupt cancels.
    pub fn token(&self) -> &CancelToken {
        &self.token
    }

    /// Puts signal handling back to the default.
    pub fn stop(&self) {
        imp::restore();
    }
}

impl Drop for SignalGuard {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Returns a token cancelled by the first interrupt (Ctrl-C) or SIGTERM,
/// so a run in progress stops where it is, prints what it has, keeps the
/// session and history of the requests that completed, and exits 3 with
/// E305 rather than dying mid-write. Once that first signal has been
/// taken, handling reverts to the default, so a second Ctrl-C ends the
/// process at once if the shutdown itself hangs.
pub fn signal_context() -> SignalGuard {
    let token = CancelToken::new();
    imp::install(token.clone());
    SignalGuard { token }
}

/// Ends the process as a write to a closed pipe does when nothing
/// handles the signal.
#[cfg(unix)]
pub(crate) fn die_of_broken_pipe() -> ! {
    imp::die_of_broken_pipe()
}

#[cfg(unix)]
mod imp {
    use std::sync::atomic::{AtomicI32, Ordering};
    use std::sync::Mutex;

    use apic_httpc::CancelToken;

    /// The write end of the pipe the handler pokes; -1 when none is open.
    static PIPE_WRITE: AtomicI32 = AtomicI32::new(-1);
    /// Serialises installing: one watcher at a time owns the pipe.
    static INSTALL: Mutex<()> = Mutex::new(());

    /// The handler does the one thing a handler safely can: it writes a
    /// byte for the watcher thread to read.
    extern "C" fn on_signal(sig: libc::c_int) {
        let fd = PIPE_WRITE.load(Ordering::Relaxed);
        if fd >= 0 {
            let byte = if sig == libc::SIGTERM { b't' } else { b'i' };
            // SAFETY: write(2) is async-signal-safe and the buffer is one
            // live byte on this stack.
            unsafe {
                libc::write(fd, std::ptr::addr_of!(byte).cast(), 1);
            }
        }
    }

    pub fn install(token: CancelToken) {
        let _guard = INSTALL
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut fds = [0 as libc::c_int; 2];
        // SAFETY: pipe(2) fills the two descriptors of the array it is given.
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            return;
        }
        let (read_fd, write_fd) = (fds[0], fds[1]);
        // SAFETY: both descriptors were just opened by this function.
        unsafe {
            libc::fcntl(read_fd, libc::F_SETFD, libc::FD_CLOEXEC);
            libc::fcntl(write_fd, libc::F_SETFD, libc::FD_CLOEXEC);
        }
        let old = PIPE_WRITE.swap(write_fd, Ordering::SeqCst);
        if old >= 0 {
            // An earlier watcher is still waiting: closing its write end
            // lets it go.
            // SAFETY: the descriptor was opened by an earlier install and
            // is no longer reachable by the handler.
            unsafe {
                libc::close(old);
            }
        }
        set_handlers(on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t);
        std::thread::spawn(move || {
            let mut byte = 0u8;
            let got = loop {
                // SAFETY: read(2) into one live byte on this stack.
                let n = unsafe { libc::read(read_fd, std::ptr::addr_of_mut!(byte).cast(), 1) };
                if n < 0
                    && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
                {
                    continue;
                }
                break n;
            };
            // SAFETY: the read end belongs to this thread alone.
            unsafe {
                libc::close(read_fd);
            }
            if got == 1 {
                // The first signal is taken: the next one is the system's.
                restore();
                token.cancel_with(if byte == b't' {
                    "terminated signal received"
                } else {
                    "interrupt signal received"
                });
            }
        });
    }

    fn set_handlers(handler: libc::sighandler_t) {
        // SAFETY: signal(2) with a handler that only calls write(2), or
        // with the default disposition.
        unsafe {
            libc::signal(libc::SIGINT, handler);
            libc::signal(libc::SIGTERM, handler);
        }
    }

    pub fn restore() {
        let fd = PIPE_WRITE.swap(-1, Ordering::SeqCst);
        if fd < 0 {
            return;
        }
        set_handlers(libc::SIG_DFL);
        // SAFETY: the write end is no longer reachable by the handler;
        // closing it ends the watcher's read.
        unsafe {
            libc::close(fd);
        }
    }

    pub fn die_of_broken_pipe() -> ! {
        // SAFETY: restores the default disposition and raises the signal
        // at this process, which ends it.
        unsafe {
            libc::signal(libc::SIGPIPE, libc::SIG_DFL);
            libc::raise(libc::SIGPIPE);
        }
        std::process::exit(141)
    }
}

#[cfg(windows)]
mod imp {
    use std::sync::Mutex;

    use apic_httpc::CancelToken;

    #[link(name = "kernel32")]
    extern "system" {
        fn SetConsoleCtrlHandler(
            handler: Option<unsafe extern "system" fn(u32) -> i32>,
            add: i32,
        ) -> i32;
    }

    static TOKEN: Mutex<Option<CancelToken>> = Mutex::new(None);

    /// Runs on a thread the console starts, so it may take locks. It
    /// claims the first event and leaves the next to the default handler.
    unsafe extern "system" fn on_ctrl(_event: u32) -> i32 {
        let token = TOKEN
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        match token {
            Some(token) => {
                // SAFETY: removes the handler this module registered.
                unsafe {
                    SetConsoleCtrlHandler(Some(on_ctrl), 0);
                }
                token.cancel_with("interrupt signal received");
                1
            }
            None => 0,
        }
    }

    pub fn install(token: CancelToken) {
        let mut slot = TOKEN
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let fresh = slot.is_none();
        *slot = Some(token);
        if fresh {
            // SAFETY: registers a handler that lives as long as the program.
            unsafe {
                SetConsoleCtrlHandler(Some(on_ctrl), 1);
            }
        }
    }

    pub fn restore() {
        let had = TOKEN
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if had.is_some() {
            // SAFETY: removes the handler this module registered.
            unsafe {
                SetConsoleCtrlHandler(Some(on_ctrl), 0);
            }
        }
    }
}

#[cfg(not(any(unix, windows)))]
mod imp {
    use apic_httpc::CancelToken;

    pub fn install(_token: CancelToken) {}

    pub fn restore() {}
}

/// Sends this process an interrupt, for the test of the handler.
#[cfg(all(test, unix))]
pub(crate) fn interrupt_self() {
    // SAFETY: kill(2) with this process's own id.
    unsafe {
        libc::kill(libc::getpid(), libc::SIGINT);
    }
}
