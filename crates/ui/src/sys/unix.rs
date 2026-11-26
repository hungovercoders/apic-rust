#![allow(unsafe_code)]

use std::fs::File;
use std::io;
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd};

/// The terminal modes to put back.
pub(crate) struct RawState(libc::termios);

/// Puts the terminal into raw mode: no echo, no line editing, no signals
/// from keys, bytes passed through as typed. It returns the modes it
/// replaced.
pub(crate) fn make_raw(f: &File) -> io::Result<RawState> {
    let fd = f.as_raw_fd();
    let mut termios = MaybeUninit::<libc::termios>::uninit();
    // SAFETY: fd is an open descriptor for the life of `f`, and tcgetattr
    // writes a whole termios through the pointer when it succeeds.
    if unsafe { libc::tcgetattr(fd, termios.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: tcgetattr succeeded, so the value is initialised.
    let old = unsafe { termios.assume_init() };
    let mut raw = old;
    raw.c_iflag &= !(libc::IGNBRK
        | libc::BRKINT
        | libc::PARMRK
        | libc::ISTRIP
        | libc::INLCR
        | libc::IGNCR
        | libc::ICRNL
        | libc::IXON);
    raw.c_oflag &= !libc::OPOST;
    raw.c_lflag &= !(libc::ECHO | libc::ECHONL | libc::ICANON | libc::ISIG | libc::IEXTEN);
    raw.c_cflag &= !(libc::CSIZE | libc::PARENB);
    raw.c_cflag |= libc::CS8;
    raw.c_cc[libc::VMIN] = 1;
    raw.c_cc[libc::VTIME] = 0;
    // SAFETY: fd is open and `raw` is a valid termios.
    if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(RawState(old))
}

/// Puts back the modes `make_raw` replaced.
pub(crate) fn restore(f: &File, state: &RawState) -> io::Result<()> {
    // SAFETY: the descriptor is open for the life of `f` and the termios
    // is the one tcgetattr filled in.
    if unsafe { libc::tcsetattr(f.as_raw_fd(), libc::TCSANOW, &state.0) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// The terminal's size in columns and rows.
pub(crate) fn size(f: &File) -> io::Result<(i32, i32)> {
    let mut ws = MaybeUninit::<libc::winsize>::uninit();
    // SAFETY: the descriptor is open for the life of `f`, and TIOCGWINSZ
    // writes a whole winsize through the pointer when it succeeds.
    if unsafe { libc::ioctl(f.as_raw_fd(), libc::TIOCGWINSZ, ws.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the ioctl succeeded, so the value is initialised.
    let ws = unsafe { ws.assume_init() };
    Ok((i32::from(ws.ws_col), i32::from(ws.ws_row)))
}

/// Has nothing to do outside Windows: every other terminal processes
/// escape sequences as a matter of course.
pub(crate) fn enable_vt() -> VtGuard {
    VtGuard
}

/// Puts the console mode back when dropped; nothing to put back here.
pub(crate) struct VtGuard;

/// A pipe with both ends closed on exec: the read end, then the write
/// end.
pub(crate) fn pipe() -> io::Result<(File, File)> {
    let mut fds = [0 as libc::c_int; 2];
    // SAFETY: pipe writes two descriptors into the array when it succeeds.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: pipe succeeded, so both are open descriptors nothing else
    // owns.
    let (r, w) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
    for fd in [&r, &w] {
        // SAFETY: the descriptor is open and owned by `fd`; F_SETFD takes
        // a plain integer.
        if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok((File::from(r), File::from(w)))
}

/// A pipe with both ends non-blocking and closed on exec: the read end,
/// then the write end.
pub(crate) fn cancel_pipe() -> io::Result<(File, File)> {
    let (r, w) = pipe()?;
    for f in [&r, &w] {
        let raw = f.as_raw_fd();
        // SAFETY: raw is an open descriptor owned by `f`; F_GETFL and
        // F_SETFL take and return plain integers.
        let ok = unsafe {
            let flags = libc::fcntl(raw, libc::F_GETFL);
            flags >= 0 && libc::fcntl(raw, libc::F_SETFL, flags | libc::O_NONBLOCK) >= 0
        };
        if !ok {
            return Err(io::Error::last_os_error());
        }
    }
    Ok((r, w))
}

/// Waits until either descriptor has something to read (or has hung up)
/// and reports which.
pub(crate) fn poll_two(a: BorrowedFd<'_>, b: BorrowedFd<'_>) -> io::Result<(bool, bool)> {
    let mut fds = [
        libc::pollfd {
            fd: a.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: b.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    loop {
        // SAFETY: fds is a valid array of two pollfd for the whole call,
        // and both descriptors are borrowed, so open.
        if unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) } >= 0 {
            return Ok((fds[0].revents != 0, fds[1].revents != 0));
        }
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::Interrupted {
            return Err(err);
        }
    }
}
