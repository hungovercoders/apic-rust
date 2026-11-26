//! File-system errors worded as `open /path/file: permission denied`: the
//! operation, the path and the system's reason in lower case.

use std::io;
use std::path::Path;

/// The system's reason for an I/O error, without the `(os error N)` suffix
/// and starting in lower case: `no such file or directory`.
pub fn io_error_text(err: &io::Error) -> String {
    let text = err.to_string();
    let text = match text.find(" (os error") {
        Some(i) => &text[..i],
        None => &text,
    };
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_lowercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// An I/O error on a path, as `open /path/file: permission denied`. `op`
/// is the operation that failed: `open`, `mkdir`, `remove`, `stat`.
pub fn path_error(op: &str, path: &Path, err: &io::Error) -> String {
    format!("{} {}: {}", op, path.display(), io_error_text(err))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_errors() {
        let err = std::fs::read("/no/such/dir/file").unwrap_err();
        assert_eq!(
            path_error("open", Path::new("/no/such/dir/file"), &err),
            "open /no/such/dir/file: no such file or directory"
        );
        assert_eq!(io_error_text(&io::Error::other("Custom")), "custom");
    }
}
