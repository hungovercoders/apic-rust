//! The streams a command reads and writes, swappable so a test can run a
//! command in-process and look at what it printed.

use std::io::{self, IsTerminal, Read, Write};
use std::sync::{Arc, Mutex, PoisonError};

/// A shared writer, the form the runner and the feature runner take for
/// the streams they are handed.
pub type SharedWriter = Arc<Mutex<dyn Write + Send>>;

/// Where a command writes: the process's standard output or error, or
/// anything else a caller hands in. Clones write to the same place.
#[derive(Clone)]
pub struct Output {
    w: SharedWriter,
    terminal: bool,
}

impl Output {
    /// The process's standard output.
    pub fn stdout() -> Output {
        Output {
            terminal: io::stdout().is_terminal(),
            w: Arc::new(Mutex::new(Std(io::stdout()))),
        }
    }

    /// The process's standard error.
    pub fn stderr() -> Output {
        Output {
            terminal: io::stderr().is_terminal(),
            w: Arc::new(Mutex::new(Std(io::stderr()))),
        }
    }

    /// Any other writer; it is never taken for a terminal.
    pub fn writer(w: impl Write + Send + 'static) -> Output {
        Output {
            w: Arc::new(Mutex::new(w)),
            terminal: false,
        }
    }

    /// The writer itself, for the crates that take a shared one.
    pub fn shared(&self) -> SharedWriter {
        self.w.clone()
    }

    /// Reports whether this is an interactive terminal; what is written
    /// to a file, a pipe or a buffer gets no colour.
    pub fn is_terminal(&self) -> bool {
        self.terminal
    }

    /// Writes text, ignoring a failure as a print does.
    pub fn print(&self, s: &str) {
        let _ = self.write_text(s);
    }

    /// Writes text and reports a failure.
    pub fn write_text(&self, s: &str) -> io::Result<()> {
        let mut w = self.w.lock().unwrap_or_else(PoisonError::into_inner);
        w.write_all(s.as_bytes())?;
        w.flush()
    }
}

impl Write for Output {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.w
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.w
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .flush()
    }
}

/// A standard stream. A reader that has gone away (`apic run … | head`)
/// ends the process the way the closed pipe's signal would, instead of
/// leaving a flow to carry on sending requests nobody reads about.
struct Std<W: Write>(W);

impl<W: Write> Write for Std<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        gone(self.0.write(buf))
    }

    fn flush(&mut self) -> io::Result<()> {
        gone(self.0.flush())
    }
}

/// Ends the process when a write found the pipe closed, where the system
/// has a signal for that; passes the result on otherwise.
fn gone<T>(result: io::Result<T>) -> io::Result<T> {
    #[cfg(unix)]
    if let Err(e) = &result {
        if e.kind() == io::ErrorKind::BrokenPipe {
            crate::signal::die_of_broken_pipe();
        }
    }
    result
}

/// A buffer to capture what a command prints: hand `output()` to an
/// [`crate::App`], then read `string()`.
#[derive(Clone, Default)]
pub struct Buffer(Arc<Mutex<Vec<u8>>>);

impl Buffer {
    pub fn new() -> Buffer {
        Buffer::default()
    }

    /// An output that appends to this buffer.
    pub fn output(&self) -> Output {
        Output::writer(self.clone())
    }

    /// Everything written so far.
    pub fn bytes(&self) -> Vec<u8> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Everything written so far, as text.
    pub fn string(&self) -> String {
        String::from_utf8_lossy(&self.bytes()).into_owned()
    }
}

impl Write for Buffer {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Where a command reads from: the process's standard input, or a reader
/// a caller hands in.
pub enum Input {
    /// The process's standard input.
    Stdin,
    /// Any other reader.
    Reader(Box<dyn Read + Send>),
    /// Nothing left: a server took the stream over.
    Taken,
}

impl Input {
    /// A reader over text held in memory.
    pub fn text(s: impl Into<Vec<u8>>) -> Input {
        Input::Reader(Box::new(io::Cursor::new(s.into())))
    }

    /// Any reader.
    pub fn reader(r: impl Read + Send + 'static) -> Input {
        Input::Reader(Box::new(r))
    }

    /// Reports whether this is the process's own standard input.
    pub fn is_stdin(&self) -> bool {
        matches!(self, Input::Stdin)
    }

    /// Reads everything there is.
    pub fn read_all(&mut self) -> io::Result<Vec<u8>> {
        let mut data = Vec::new();
        match self {
            Input::Stdin => io::stdin().lock().read_to_end(&mut data)?,
            Input::Reader(r) => r.read_to_end(&mut data)?,
            Input::Taken => 0,
        };
        Ok(data)
    }

    /// Hands the stream over to a server that reads it until it ends.
    pub fn take(&mut self) -> Box<dyn Read + Send> {
        match std::mem::replace(self, Input::Taken) {
            Input::Stdin => {
                *self = Input::Stdin;
                Box::new(io::stdin())
            }
            Input::Reader(r) => r,
            Input::Taken => Box::new(io::empty()),
        }
    }
}

/// Prints formatted text to an [`Output`], as `print!` does to stdout.
macro_rules! w {
    ($out:expr, $($arg:tt)*) => {
        $out.print(&format!($($arg)*))
    };
}

/// Prints a formatted line to an [`Output`].
macro_rules! wln {
    ($out:expr) => {
        $out.print("\n")
    };
    ($out:expr, $($arg:tt)*) => {
        $out.print(&(format!($($arg)*) + "\n"))
    };
}

pub(crate) use {w, wln};
