#![allow(unsafe_code)]

use std::fs::File;
use std::io;
use std::os::windows::io::AsRawHandle;

use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Console::{
    GetConsoleMode, GetConsoleScreenBufferInfo, GetStdHandle, SetConsoleMode,
    CONSOLE_SCREEN_BUFFER_INFO, ENABLE_ECHO_INPUT, ENABLE_LINE_INPUT, ENABLE_PROCESSED_INPUT,
    ENABLE_VIRTUAL_TERMINAL_INPUT, ENABLE_VIRTUAL_TERMINAL_PROCESSING, STD_OUTPUT_HANDLE,
};

/// The console input mode to put back.
pub(crate) struct RawState(u32);

fn handle(f: &File) -> HANDLE {
    f.as_raw_handle() as HANDLE
}

fn console_mode(h: HANDLE) -> io::Result<u32> {
    let mut mode = 0u32;
    // SAFETY: GetConsoleMode writes one u32 through the pointer; a handle
    // that is not a console makes it fail, which is reported.
    if unsafe { GetConsoleMode(h, &mut mode) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(mode)
}

fn set_console_mode(h: HANDLE, mode: u32) -> io::Result<()> {
    // SAFETY: SetConsoleMode takes the handle and the mode by value.
    if unsafe { SetConsoleMode(h, mode) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Puts the console input into raw mode: no echo, no line editing,
/// ctrl+c as a key, and keys sent as escape sequences. It returns the mode
/// it replaced.
pub(crate) fn make_raw(f: &File) -> io::Result<RawState> {
    let h = handle(f);
    let old = console_mode(h)?;
    let raw = (old & !(ENABLE_ECHO_INPUT | ENABLE_PROCESSED_INPUT | ENABLE_LINE_INPUT))
        | ENABLE_VIRTUAL_TERMINAL_INPUT;
    set_console_mode(h, raw)?;
    Ok(RawState(old))
}

/// Puts back the mode `make_raw` replaced.
pub(crate) fn restore(f: &File, state: &RawState) -> io::Result<()> {
    set_console_mode(handle(f), state.0)
}

fn stdout_handle() -> io::Result<HANDLE> {
    // SAFETY: GetStdHandle takes a constant and returns a handle or an
    // error value, both checked below.
    let h = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
    if h == INVALID_HANDLE_VALUE || h.is_null() {
        return Err(io::Error::last_os_error());
    }
    Ok(h)
}

fn buffer_size(h: HANDLE) -> io::Result<(i32, i32)> {
    // SAFETY: the struct is plain integers, for which zero is a value.
    let mut info: CONSOLE_SCREEN_BUFFER_INFO = unsafe { std::mem::zeroed() };
    // SAFETY: GetConsoleScreenBufferInfo fills the struct through the
    // pointer; a handle that is not a screen buffer makes it fail.
    if unsafe { GetConsoleScreenBufferInfo(h, &mut info) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let w = info.srWindow;
    Ok((
        i32::from(w.Right) - i32::from(w.Left) + 1,
        i32::from(w.Bottom) - i32::from(w.Top) + 1,
    ))
}

/// The console window's size in columns and rows. The window belongs to
/// the screen buffer, so when `f` is the console's input the answer comes
/// from standard output.
pub(crate) fn size(f: &File) -> io::Result<(i32, i32)> {
    buffer_size(handle(f)).or_else(|_| buffer_size(stdout_handle()?))
}

/// Turns on virtual terminal processing for the console, which the legacy
/// console (cmd.exe, and PowerShell outside Windows Terminal) leaves off:
/// without it the alternate-screen and cursor sequences the program
/// writes before its first frame would print as text. The mode is put
/// back when the guard is dropped.
pub(crate) fn enable_vt() -> VtGuard {
    let enable = || -> io::Result<u32> {
        let h = stdout_handle()?;
        let mode = console_mode(h)?;
        if mode & ENABLE_VIRTUAL_TERMINAL_PROCESSING != ENABLE_VIRTUAL_TERMINAL_PROCESSING {
            set_console_mode(h, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING)?;
        }
        Ok(mode)
    };
    VtGuard(enable().ok())
}

/// Puts the console output mode back when dropped.
pub(crate) struct VtGuard(Option<u32>);

impl Drop for VtGuard {
    fn drop(&mut self) {
        if let Some(mode) = self.0 {
            if let Ok(h) = stdout_handle() {
                let _ = set_console_mode(h, mode);
            }
        }
    }
}
