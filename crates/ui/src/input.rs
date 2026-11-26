//! Where keys are read from: a source whose pending read can be
//! interrupted, so that the editor can have the terminal to itself.

use std::fmt;
use std::fs::File;
use std::io::{self, Read};
use std::sync::Arc;

/// Why a [`KeySource`] read returned nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ReadError {
    /// `cancel` was called on the source.
    Cancelled,
    /// The input ended or failed, with the reason; `EOF` at its end.
    Closed(String),
}

impl ReadError {
    pub(crate) fn eof() -> ReadError {
        ReadError::Closed("EOF".to_string())
    }
}

impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReadError::Cancelled => f.write_str("read cancelled"),
            ReadError::Closed(why) => f.write_str(why),
        }
    }
}

/// The terminal `read_keys` reads from. Its pending read can be
/// interrupted, which is what lets the editor have the terminal to itself:
/// the program cancels the read before it suspends the UI and resumes it
/// afterwards, so a byte typed into the editor is never taken by apic
/// instead and replayed as a key when the editor closes.
pub(crate) trait KeySource: Send + Sync {
    /// Reads what has been typed, waiting for some.
    fn read(&self, buf: &mut [u8]) -> Result<usize, ReadError>;
    /// Makes the pending read, or the next one, return
    /// [`ReadError::Cancelled`]. A second cancel before `resume` does
    /// nothing.
    fn cancel(&self);
    /// Lets `read` work again. Input that arrived in between is dropped.
    fn resume(&self);
}

/// A [`KeySource`] as a reader, for `read_keys`.
pub(crate) struct SourceReader(pub(crate) Arc<dyn KeySource>);

impl Read for SourceReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0
            .read(buf)
            .map_err(|e| io::Error::other(e.to_string()))
    }
}

/// The source for the terminal `f`: `poll` where there is one.
#[cfg(unix)]
pub(crate) fn new_key_source(f: Arc<File>) -> io::Result<Arc<dyn KeySource>> {
    Ok(Arc::new(crate::input_unix::PollSource::new(f)?))
}

/// The source for the terminal `f`. It is read from a thread: there is no
/// poll to interrupt a console read with, so a key typed into the editor
/// just as it opens can be lost, but `resume` keeps it from being acted on
/// afterwards.
#[cfg(not(unix))]
pub(crate) fn new_key_source(f: Arc<File>) -> io::Result<Arc<dyn KeySource>> {
    Ok(Arc::new(bg::BgSource::new(FileReader(f))))
}

#[cfg(not(unix))]
struct FileReader(Arc<File>);

#[cfg(not(unix))]
impl Read for FileReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        (&*self.0).read(buf)
    }
}

#[cfg(test)]
pub(crate) use bg::BgSource;

#[cfg(any(test, not(unix)))]
mod bg {
    use std::collections::VecDeque;
    use std::io::Read;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

    use super::{KeySource, ReadError};

    /// The [`KeySource`] for platforms without `poll`: one thread reads
    /// the file for the life of the program and `read` waits on it or on
    /// `cancel`. The read that is pending when `cancel` is called still
    /// completes in the background, so on these platforms the first bytes
    /// typed into the editor can be lost to it. Each chunk records whether
    /// its read returned while paused, and `read` drops those, so what the
    /// editor was meant to get never reaches the model as keys, however
    /// late the chunk arrives.
    pub(crate) struct BgSource {
        shared: Arc<Shared>,
    }

    struct Shared {
        state: Mutex<State>,
        cond: Condvar,
        paused: AtomicBool,
    }

    #[derive(Default)]
    struct State {
        chunks: VecDeque<Chunk>,
        /// The reader's final error, until a read has taken it.
        failed: Option<String>,
        /// Set by `cancel`, cleared by `resume`.
        cancelled: bool,
        /// The reader's final error, once a read has reported it.
        err: Option<String>,
        /// The tail of a chunk a short read left over.
        rest: Vec<u8>,
    }

    /// One read's bytes. `stale` marks a read that returned while the
    /// source was paused: its bytes were typed at the editor, not the UI.
    struct Chunk {
        b: Vec<u8>,
        stale: bool,
    }

    impl Shared {
        fn lock(&self) -> MutexGuard<'_, State> {
            self.state.lock().unwrap_or_else(PoisonError::into_inner)
        }
    }

    impl BgSource {
        pub(crate) fn new<R: Read + Send + 'static>(mut r: R) -> BgSource {
            let shared = Arc::new(Shared {
                state: Mutex::new(State::default()),
                cond: Condvar::new(),
                paused: AtomicBool::new(false),
            });
            let s = shared.clone();
            std::thread::spawn(move || {
                let mut buf = [0u8; 256];
                loop {
                    let failed = match r.read(&mut buf) {
                        Ok(0) => Some("EOF".to_string()),
                        Ok(n) => {
                            let stale = s.paused.load(Ordering::SeqCst);
                            s.lock().chunks.push_back(Chunk {
                                b: buf[..n].to_vec(),
                                stale,
                            });
                            None
                        }
                        Err(e) => Some(e.to_string()),
                    };
                    if let Some(why) = failed {
                        s.lock().failed = Some(why);
                        s.cond.notify_all();
                        return;
                    }
                    s.cond.notify_all();
                }
            });
            BgSource { shared }
        }

        /// How many chunks the reader has queued and no read has taken.
        #[cfg(test)]
        pub(crate) fn queued(&self) -> usize {
            self.shared.lock().chunks.len()
        }
    }

    fn take(st: &mut State, from: Vec<u8>, buf: &mut [u8]) -> usize {
        let n = from.len().min(buf.len());
        buf[..n].copy_from_slice(&from[..n]);
        st.rest = from[n..].to_vec();
        n
    }

    impl KeySource for BgSource {
        fn read(&self, buf: &mut [u8]) -> Result<usize, ReadError> {
            let mut st = self.shared.lock();
            if !st.rest.is_empty() {
                let rest = std::mem::take(&mut st.rest);
                return Ok(take(&mut st, rest, buf));
            }
            if let Some(err) = &st.err {
                return Err(ReadError::Closed(err.clone()));
            }
            loop {
                if st.cancelled {
                    return Err(ReadError::Cancelled);
                }
                if let Some(c) = st.chunks.pop_front() {
                    if c.stale {
                        continue;
                    }
                    return Ok(take(&mut st, c.b, buf));
                }
                if let Some(err) = st.failed.take() {
                    st.err = Some(err.clone());
                    return Err(ReadError::Closed(err));
                }
                st = self
                    .shared
                    .cond
                    .wait(st)
                    .unwrap_or_else(PoisonError::into_inner);
            }
        }

        fn cancel(&self) {
            let mut st = self.shared.lock();
            self.shared.paused.store(true, Ordering::SeqCst);
            st.cancelled = true;
            self.shared.cond.notify_all();
        }

        fn resume(&self) {
            let mut st = self.shared.lock();
            st.cancelled = false;
            st.rest.clear();
            self.shared.paused.store(false, Ordering::SeqCst);
        }
    }
}
