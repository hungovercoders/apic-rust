//! Creating a directory and its missing parents, with the error naming
//! the path that could not be made rather than the one asked for.

use std::io;
use std::path::Path;

use crate::ioerror::path_error;

/// Creates `path` and any parents it lacks. Nothing happens when it is
/// already a directory. The error is the message to show, and names the
/// first path that stood in the way: `mkdir out/file: not a directory`
/// when `out/file/sub` was asked for and `out/file` is a file.
pub fn mkdir_all(path: &Path) -> Result<(), String> {
    match std::fs::metadata(path) {
        Ok(meta) if meta.is_dir() => return Ok(()),
        Ok(_) => {
            let err = io::Error::from(io::ErrorKind::NotADirectory);
            return Err(path_error("mkdir", path, &err));
        }
        Err(_) => {}
    }
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() && parent != path {
            mkdir_all(parent)?;
        }
    }
    match std::fs::create_dir(path) {
        Ok(()) => Ok(()),
        // Made by someone else in the meantime, or `path` ends in a dot.
        Err(_) if path.is_dir() => Ok(()),
        Err(err) => Err(path_error("mkdir", path, &err)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn makes_parents_and_names_what_is_in_the_way() {
        let base = std::env::temp_dir().join(format!("apic-mkdir-all-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let deep = base.join("a").join("b");
        mkdir_all(&deep).unwrap();
        mkdir_all(&deep).unwrap();
        assert!(deep.is_dir());
        let file = deep.join("file");
        std::fs::write(&file, "x").unwrap();
        assert_eq!(
            mkdir_all(&file.join("sub")).unwrap_err(),
            format!("mkdir {}: not a directory", file.display())
        );
        assert_eq!(
            mkdir_all(&file).unwrap_err(),
            format!("mkdir {}: not a directory", file.display())
        );
        std::fs::remove_dir_all(&base).unwrap();
    }
}
