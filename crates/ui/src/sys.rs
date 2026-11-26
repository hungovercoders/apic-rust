//! The calls into the operating system the terminal layer needs: raw
//! mode, the window size, escape-sequence processing on Windows, and the
//! `poll` behind the key source. Everything `unsafe` in apic is in the
//! modules below, behind safe functions.

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub(crate) use unix::*;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub(crate) use windows::*;

#[cfg(not(any(unix, windows)))]
mod other;
#[cfg(not(any(unix, windows)))]
pub(crate) use other::*;
