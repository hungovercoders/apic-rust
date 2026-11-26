//! The event loop: the one part of the UI that touches a terminal. It
//! puts the terminal into raw mode on the alternate screen, feeds keys,
//! timers and finished work to the model, and paints what the model
//! draws.

use std::any::Any;
use std::backtrace::Backtrace;
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::fmt;
use std::fs::File;
use std::io::{self, IsTerminal, Read, Write};
use std::panic::{self, AssertUnwindSafe};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Once, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use apic_httpc::CancelToken;

use crate::input::{new_key_source, KeySource, SourceReader};
use crate::run::ResultMsg;
use crate::sys;
use crate::term::{read_keys, Key};
use crate::ui::Model;

/// An event delivered to the model, or an instruction a command hands
/// back to whatever is driving it.
pub enum Msg {
    /// Reports the terminal size.
    Size { width: i32, height: i32 },
    /// Advances the spinner while a run is in flight.
    Tick,
    /// One keypress.
    Key(Key),
    /// One finished request.
    Result(Box<ResultMsg>),
    /// The editor closed, with why it failed when it did.
    EditorDone(Option<String>),
    /// Clears the transient status, if it is still the one numbered.
    ClearStatus(u64),
    /// Ends the program.
    Quit,
    /// Asks the program to suspend and run a command (the editor).
    Exec(ExecMsg),
    /// A command to follow.
    Cmd(Cmd),
    /// Several commands to run in order.
    Batch(Vec<Cmd>),
    /// Work that must not block the event loop; what it returns comes
    /// back as a message.
    Async(Box<dyn FnOnce() -> Msg + Send>),
    /// Asks the program to deliver the message once the time has passed.
    Delay(Duration, Box<Msg>),
    /// A panic caught on a worker thread.
    Panic(PanicMsg),
}

impl fmt::Debug for Msg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Msg::Size { width, height } => write!(f, "Size({width}x{height})"),
            Msg::Tick => f.write_str("Tick"),
            Msg::Key(k) => write!(f, "Key({k})"),
            Msg::Result(r) => write!(f, "Result({})", r.req.id()),
            Msg::EditorDone(err) => write!(f, "EditorDone({err:?})"),
            Msg::ClearStatus(seq) => write!(f, "ClearStatus({seq})"),
            Msg::Quit => f.write_str("Quit"),
            Msg::Exec(e) => write!(f, "Exec({:?})", e.cmd),
            Msg::Cmd(_) => f.write_str("Cmd"),
            Msg::Batch(cmds) => write!(f, "Batch({})", cmds.len()),
            Msg::Async(_) => f.write_str("Async"),
            Msg::Delay(d, msg) => write!(f, "Delay({d:?}, {msg:?})"),
            Msg::Panic(p) => write!(f, "Panic({})", p.value),
        }
    }
}

/// Work run off the event loop; its message comes back to the model.
pub type Cmd = Box<dyn FnOnce() -> Option<Msg> + Send>;

/// Asks the program to suspend and run a command (the editor). `done`
/// turns how it ended, `None` for success, into the message the model
/// gets.
pub struct ExecMsg {
    pub(crate) cmd: Command,
    pub(crate) done: Option<Box<dyn FnOnce(Option<String>) -> Msg + Send>>,
}

/// The command that ends the UI.
pub fn quit() -> Option<Msg> {
    Some(Msg::Quit)
}

/// Runs several commands, so one key can both start a run and set a
/// status.
pub(crate) fn batch(cmds: Vec<Cmd>) -> Cmd {
    Box::new(move || Some(Msg::Batch(cmds)))
}

/// Schedules work off the loop; its result comes back as a message.
pub(crate) fn async_cmd(run: impl FnOnce() -> Msg + Send + 'static) -> Cmd {
    Box::new(move || Some(Msg::Async(Box::new(run))))
}

/// Schedules a message once `d` has passed.
pub(crate) fn after(d: Duration, msg: Msg) -> Cmd {
    Box::new(move || Some(Msg::Delay(d, Box::new(msg))))
}

/// Why the UI could not start.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error(String);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

pub(crate) const ALT_SCREEN_ON: &str = "\x1b[?1049h";
pub(crate) const ALT_SCREEN_OFF: &str = "\x1b[?1049l";
pub(crate) const HIDE_CURSOR: &str = "\x1b[?25l";
pub(crate) const SHOW_CURSOR: &str = "\x1b[?25h";
pub(crate) const CLEAR_SCREEN: &str = "\x1b[2J\x1b[H";
pub(crate) const CURSOR_HOME: &str = "\x1b[H";
pub(crate) const ERASE_LINE: &str = "\x1b[K";
pub(crate) const ERASE_BELOW: &str = "\x1b[J";

/// How often the loop asks the terminal its size.
const SIZE_EVERY: Duration = Duration::from_millis(250);
/// How often the spinner moves while the model is live.
const TICK_EVERY: Duration = Duration::from_millis(110);

/// The process's standard input as a file, for [`run`]: a second handle
/// on the same terminal, so closing it leaves standard input open.
pub fn stdin_file() -> io::Result<File> {
    #[cfg(unix)]
    {
        use std::os::fd::AsFd;
        Ok(File::from(io::stdin().as_fd().try_clone_to_owned()?))
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsHandle;
        Ok(File::from(io::stdin().as_handle().try_clone_to_owned()?))
    }
    #[cfg(not(any(unix, windows)))]
    {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "terminals are not supported on this platform",
        ))
    }
}

/// Drives the model on a real terminal: raw mode, the alternate screen, a
/// size poller and a spinner ticker. `input` is the terminal the keys
/// come from and `out` is where the screen is written; the editor the o
/// key opens reads `input` and inherits the process's standard output and
/// error. Cancelling `cancel` ends the UI as q does. The terminal is
/// restored on the way out, including when the model panics.
pub fn run(
    cancel: &CancelToken,
    m: &mut Model,
    input: File,
    out: &mut dyn Write,
) -> Result<(), Error> {
    if !input.is_terminal() {
        return Err(Error("stdin is not a terminal".to_string()));
    }
    let _vt = sys::enable_vt();
    let state = sys::make_raw(&input).map_err(|e| {
        Error(format!(
            "raw mode: {e} (on Windows use Windows Terminal, or winpty in Git Bash)"
        ))
    })?;
    let input = Arc::new(input);
    let src = match new_key_source(input.clone()) {
        Ok(src) => src,
        Err(e) => {
            let _ = sys::restore(&input, &state);
            return Err(Error(format!("input: {e}")));
        }
    };
    let mut p = Program::new(m, input, out, src);
    p.state = Some(state);
    // A panic on this thread is caught, so that the terminal is put back
    // before the crash prints rather than after.
    let outcome = guard(|| p.run_loop(cancel));
    p.stop_keys();
    p.restore();
    if let Err(pm) = outcome {
        drop(p);
        panic!("{pm}");
    }
    Ok(())
}

/// What the loop waits on: keys from the terminal and messages from work
/// that has finished. Keys are kept apart so that those typed before the
/// editor opened can be dropped on their own.
#[derive(Default)]
pub(crate) struct Mailbox {
    queues: Mutex<Queues>,
    cond: Condvar,
}

#[derive(Default)]
struct Queues {
    keys: VecDeque<Key>,
    msgs: VecDeque<Msg>,
}

pub(crate) enum Event {
    Key(Key),
    Msg(Box<Msg>),
}

impl Mailbox {
    fn lock(&self) -> MutexGuard<'_, Queues> {
        self.queues.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn push_key(&self, k: Key) {
        self.lock().keys.push_back(k);
        self.cond.notify_all();
    }

    pub(crate) fn push_msg(&self, msg: Msg) {
        self.lock().msgs.push_back(msg);
        self.cond.notify_all();
    }

    fn clear_keys(&self) {
        self.lock().keys.clear();
    }

    /// The next key or message, waiting until `deadline` for one.
    fn next(&self, deadline: Instant) -> Option<Event> {
        let mut q = self.lock();
        loop {
            if let Some(k) = q.keys.pop_front() {
                return Some(Event::Key(k));
            }
            if let Some(msg) = q.msgs.pop_front() {
                return Some(Event::Msg(Box::new(msg)));
            }
            let wait = deadline.checked_duration_since(Instant::now())?;
            q = self
                .cond
                .wait_timeout(q, wait)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// The next key, waiting up to `wait` for one.
    #[cfg(test)]
    pub(crate) fn key(&self, wait: Duration) -> Option<Key> {
        let deadline = Instant::now() + wait;
        let mut q = self.lock();
        loop {
            if let Some(k) = q.keys.pop_front() {
                return Some(k);
            }
            let left = deadline.checked_duration_since(Instant::now())?;
            q = self
                .cond
                .wait_timeout(q, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// The next message, waiting up to `wait` for one.
    #[cfg(test)]
    pub(crate) fn msg(&self, wait: Duration) -> Option<Msg> {
        let deadline = Instant::now() + wait;
        let mut q = self.lock();
        loop {
            if let Some(msg) = q.msgs.pop_front() {
                return Some(msg);
            }
            let left = deadline.checked_duration_since(Instant::now())?;
            q = self
                .cond
                .wait_timeout(q, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }
}

pub(crate) struct Program<'a> {
    pub(crate) m: &'a mut Model,
    input: Arc<File>,
    out: &'a mut dyn Write,
    src: Arc<dyn KeySource>,
    state: Option<sys::RawState>,
    pub(crate) mail: Arc<Mailbox>,
    /// Cancelled when the loop ends, so that work still out is dropped.
    pub(crate) done: CancelToken,
    /// The key reader, while it is running.
    reader: Option<JoinHandle<()>>,
    last_size: (i32, i32),
    frame: String,
    restored: bool,
    /// Copies what the editor prints into `out` instead of letting it
    /// write to the process's standard output; tests set it.
    pub(crate) capture_child: bool,
}

impl<'a> Program<'a> {
    pub(crate) fn new(
        m: &'a mut Model,
        input: Arc<File>,
        out: &'a mut dyn Write,
        src: Arc<dyn KeySource>,
    ) -> Program<'a> {
        Program {
            m,
            input,
            out,
            src,
            state: None,
            mail: Arc::new(Mailbox::default()),
            done: CancelToken::new(),
            reader: None,
            last_size: (0, 0),
            frame: String::new(),
            restored: false,
            capture_child: false,
        }
    }

    fn write(&mut self, s: &str) {
        let _ = self.out.write_all(s.as_bytes());
        let _ = self.out.flush();
    }

    pub(crate) fn restore(&mut self) {
        if self.restored {
            return;
        }
        self.restored = true;
        self.write(&format!("{SHOW_CURSOR}{ALT_SCREEN_OFF}"));
        self.restore_terminal();
    }

    /// Puts the terminal modes back; a program built without them (tests)
    /// has nothing to restore.
    fn restore_terminal(&mut self) {
        if let Some(state) = &self.state {
            let _ = sys::restore(&self.input, state);
        }
    }

    /// The loop. It returns when the model quits or `cancel` is
    /// cancelled, and raises a worker's panic on this thread.
    pub(crate) fn run_loop(&mut self, cancel: &CancelToken) {
        self.write(&format!("{ALT_SCREEN_ON}{HIDE_CURSOR}{CLEAR_SCREEN}"));
        self.start_keys();
        let done = self.done.clone();
        let _done = OnDrop(move || done.cancel());

        let mut size_at = Instant::now() + SIZE_EVERY;
        let mut tick_at = Instant::now() + TICK_EVERY;

        self.resize();
        self.render();
        loop {
            if cancel.is_cancelled() {
                return;
            }
            let cmd = match self.mail.next(size_at.min(tick_at)) {
                Some(Event::Key(k)) => self.m.update(Msg::Key(k)),
                Some(Event::Msg(msg)) => match *msg {
                    // Raised here rather than on the worker so that the
                    // terminal is put back before the crash prints.
                    Msg::Panic(pm) => panic::panic_any(pm),
                    msg => self.m.update(msg),
                },
                None => {
                    let now = Instant::now();
                    if now >= size_at {
                        size_at = now + SIZE_EVERY;
                        if self.resize() {
                            self.render();
                        }
                        continue;
                    }
                    tick_at = now + TICK_EVERY;
                    if !self.m.live() {
                        continue;
                    }
                    self.m.update(Msg::Tick)
                }
            };
            if self.dispatch(cmd) {
                return;
            }
            self.render();
        }
    }

    /// Runs a command off the loop and reports whether to quit.
    pub(crate) fn dispatch(&mut self, cmd: Option<Cmd>) -> bool {
        let Some(cmd) = cmd else {
            return false;
        };
        match cmd() {
            None => false,
            Some(Msg::Quit) => true,
            Some(Msg::Exec(e)) => {
                self.exec(e);
                false
            }
            Some(Msg::Cmd(next)) => self.dispatch(Some(next)),
            Some(Msg::Batch(cmds)) => {
                for c in cmds {
                    if self.dispatch(Some(c)) {
                        return true;
                    }
                }
                false
            }
            Some(Msg::Async(work)) => {
                let (mail, done) = (self.mail.clone(), self.done.clone());
                std::thread::spawn(move || {
                    let out = match guard(work) {
                        Ok(msg) => msg,
                        Err(pm) => Msg::Panic(pm),
                    };
                    if !done.is_cancelled() {
                        mail.push_msg(out);
                    }
                });
                false
            }
            Some(Msg::Delay(d, msg)) => {
                let (mail, done) = (self.mail.clone(), self.done.clone());
                std::thread::spawn(move || {
                    if !done.wait_timeout(d) {
                        mail.push_msg(*msg);
                    }
                });
                false
            }
            // A plain message goes to the model, whose answer may be more work.
            Some(msg) => {
                let next = self.m.update(msg);
                self.dispatch(next)
            }
        }
    }

    /// Begins decoding keys from the terminal on a thread of their own.
    pub(crate) fn start_keys(&mut self) {
        self.src.resume();
        let (src, mail, done) = (self.src.clone(), self.mail.clone(), self.done.clone());
        self.reader = Some(std::thread::spawn(move || {
            read_keys(
                SourceReader(src),
                |k| {
                    mail.push_key(k);
                    true
                },
                &done,
            );
        }));
    }

    /// Interrupts the key reader and waits for it to return, so that no
    /// read is outstanding on the terminal while another program has it.
    /// Keys still queued are dropped: they were typed before the editor
    /// opened, and acting on them once it closes would be a surprise.
    pub(crate) fn stop_keys(&mut self) {
        let Some(reader) = self.reader.take() else {
            return;
        };
        self.src.cancel();
        let _ = reader.join();
        self.mail.clear_keys();
    }

    /// Suspends the UI, runs a command attached to the terminal, and
    /// restores the UI afterwards. The key reader is stopped first, so
    /// every byte typed while the command runs reaches it and not the UI.
    pub(crate) fn exec(&mut self, mut e: ExecMsg) {
        self.stop_keys();
        self.write(&format!("{SHOW_CURSOR}{ALT_SCREEN_OFF}"));
        self.restore_terminal();
        let err = self.run_child(&mut e.cmd);
        if let Ok(state) = sys::make_raw(&self.input) {
            self.state = Some(state);
        }
        self.write(&format!("{ALT_SCREEN_ON}{HIDE_CURSOR}{CLEAR_SCREEN}"));
        self.frame.clear();
        self.start_keys();
        if let Some(done) = e.done {
            if !self.done.is_cancelled() {
                self.mail.push_msg(done(err));
            }
        }
    }

    /// Runs the command with the terminal as its input and waits for it,
    /// returning why it failed, if it did.
    fn run_child(&mut self, cmd: &mut Command) -> Option<String> {
        let stdin = match self.input.try_clone() {
            Ok(f) => f,
            Err(e) => return Some(e.to_string()),
        };
        cmd.stdin(Stdio::from(stdin)).stderr(Stdio::inherit());
        cmd.stdout(if self.capture_child {
            Stdio::piped()
        } else {
            Stdio::inherit()
        });
        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(e) => return Some(spawn_error(cmd, &e)),
        };
        if let Some(mut stdout) = child.stdout.take() {
            let mut buf = [0u8; 1024];
            while let Ok(n) = stdout.read(&mut buf) {
                if n == 0 {
                    break;
                }
                let _ = self.out.write_all(&buf[..n]);
                let _ = self.out.flush();
            }
        }
        match child.wait() {
            Ok(status) => exit_error(status),
            Err(e) => Some(e.to_string()),
        }
    }

    pub(crate) fn resize(&mut self) -> bool {
        let (w, h) = match sys::size(&self.input) {
            Ok((w, h)) if w > 0 && h > 0 => (w, h),
            _ => (80, 24),
        };
        if self.last_size == (w, h) {
            return false;
        }
        self.last_size = (w, h);
        self.m.update(Msg::Size {
            width: w,
            height: h,
        });
        self.frame.clear();
        true
    }

    /// Paints the frame. Every line is erased to the end so a shorter
    /// line cannot leave debris from the previous frame.
    pub(crate) fn render(&mut self) {
        let frame = self.m.view();
        if frame == self.frame {
            return;
        }
        let mut b = String::from(CURSOR_HOME);
        let mut lines = frame.split('\n').peekable();
        while let Some(line) = lines.next() {
            b.push_str(line);
            b.push_str(ERASE_LINE);
            if lines.peek().is_some() {
                b.push_str("\r\n");
            }
        }
        b.push_str(ERASE_BELOW);
        self.frame = frame;
        self.write(&b);
    }
}

/// Runs its closure when dropped.
struct OnDrop<F: FnMut()>(F);

impl<F: FnMut()> Drop for OnDrop<F> {
    fn drop(&mut self) {
        (self.0)();
    }
}

/// Why a command could not be started, worded for the status bar.
fn spawn_error(cmd: &Command, e: &io::Error) -> String {
    let program = cmd.get_program().to_string_lossy();
    if e.kind() == io::ErrorKind::NotFound {
        let path = if cfg!(windows) { "%PATH%" } else { "$PATH" };
        return format!(
            "exec: {}: executable file not found in {path}",
            apic_gocompat::quote(&program)
        );
    }
    format!("exec: {}: {e}", apic_gocompat::quote(&program))
}

/// How a command that did not succeed ended.
fn exit_error(status: ExitStatus) -> Option<String> {
    if status.success() {
        return None;
    }
    match status.code() {
        Some(code) => Some(format!("exit status {code}")),
        None => Some(status.to_string()),
    }
}

/// A panic caught on a worker thread, carried to the loop so that the
/// terminal is restored before it is raised again. A panic that escaped
/// the worker would be reported while the terminal is still in raw mode
/// on the alternate screen, where nobody would read it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PanicMsg {
    /// What the panic said.
    pub value: String,
    /// Where it was raised, and the stack there.
    pub stack: String,
}

impl fmt::Display for PanicMsg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}\n\n{}", self.value, self.stack)
    }
}

thread_local! {
    /// Set while this thread runs under `guard`: its panics are kept for
    /// the loop rather than printed.
    static GUARDED: Cell<bool> = const { Cell::new(false) };
    /// Where the panic `guard` is about to catch was raised.
    static RAISED: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Wraps the panic hook, once, so that a panic under `guard` records
/// where it was raised and prints nothing; every other panic goes to the
/// hook that was there before.
fn install_hook() {
    static HOOK: Once = Once::new();
    HOOK.call_once(|| {
        let previous = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            if !GUARDED.get() {
                return previous(info);
            }
            let at = match info.location() {
                Some(l) => format!("panicked at {l}"),
                None => "panicked".to_string(),
            };
            let stack = format!("{at}\n{}", Backtrace::force_capture());
            RAISED.with(|r| *r.borrow_mut() = Some(stack));
        }));
    });
}

/// Runs `f`, turning a panic into a [`PanicMsg`].
pub(crate) fn guard<T>(f: impl FnOnce() -> T) -> Result<T, PanicMsg> {
    install_hook();
    let nested = GUARDED.replace(true);
    let out = panic::catch_unwind(AssertUnwindSafe(f));
    GUARDED.set(nested);
    out.map_err(|payload| {
        let stack = RAISED.with(|r| r.borrow_mut().take()).unwrap_or_default();
        match payload.downcast::<PanicMsg>() {
            // A worker's panic the loop raised again: it already says
            // where it came from.
            Ok(pm) => *pm,
            Err(payload) => PanicMsg {
                value: panic_text(payload.as_ref()),
                stack,
            },
        }
    })
}

fn panic_text(payload: &(dyn Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        return (*s).to_string();
    }
    if let Some(s) = payload.downcast_ref::<String>() {
        return s.clone();
    }
    "panic".to_string()
}
