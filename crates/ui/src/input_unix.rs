//! The key source for terminals with `poll`.

use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::AsFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::input::{KeySource, ReadError};
use crate::sys;

/// Reads the terminal through `poll`, waiting on it and on a pipe of its
/// own at once. `cancel` writes to the pipe, so a pending read returns
/// without having taken a byte from the terminal: whatever is typed next
/// goes to the editor, untouched.
pub(crate) struct PollSource {
    f: Arc<File>,
    pipe_read: File,
    pipe_write: File,
    cancelled: AtomicBool,
}

impl PollSource {
    pub(crate) fn new(f: Arc<File>) -> io::Result<PollSource> {
        let (pipe_read, pipe_write) = sys::cancel_pipe()?;
        Ok(PollSource {
            f,
            pipe_read,
            pipe_write,
            cancelled: AtomicBool::new(false),
        })
    }

    /// Empties the cancel pipe, which is non-blocking.
    fn drain(&self) {
        let mut b = [0u8; 16];
        while matches!((&self.pipe_read).read(&mut b), Ok(n) if n > 0) {}
    }
}

impl KeySource for PollSource {
    fn read(&self, buf: &mut [u8]) -> Result<usize, ReadError> {
        loop {
            let (terminal, pipe) = sys::poll_two(self.f.as_fd(), self.pipe_read.as_fd())
                .map_err(|e| ReadError::Closed(e.to_string()))?;
            if pipe {
                self.drain();
                return Err(ReadError::Cancelled);
            }
            if terminal {
                return match (&*self.f).read(buf) {
                    Ok(0) => Err(ReadError::eof()),
                    Ok(n) => Ok(n),
                    Err(e) => Err(ReadError::Closed(e.to_string())),
                };
            }
        }
    }

    fn cancel(&self) {
        if self
            .cancelled
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            let _ = (&self.pipe_write).write(&[0]);
        }
    }

    fn resume(&self) {
        self.drain();
        self.cancelled.store(false, Ordering::SeqCst);
    }
}
