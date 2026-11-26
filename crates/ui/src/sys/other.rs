use std::fs::File;
use std::io;

/// The terminal modes to put back; there are none to take here.
pub(crate) struct RawState;

fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "terminals are not supported on this platform",
    )
}

pub(crate) fn make_raw(_f: &File) -> io::Result<RawState> {
    Err(unsupported())
}

pub(crate) fn restore(_f: &File, _state: &RawState) -> io::Result<()> {
    Ok(())
}

pub(crate) fn size(_f: &File) -> io::Result<(i32, i32)> {
    Err(unsupported())
}

pub(crate) fn enable_vt() -> VtGuard {
    VtGuard
}

pub(crate) struct VtGuard;
