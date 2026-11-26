use std::fmt;
use std::io;

use apic_gocompat::io_error_text;

use crate::path;
use crate::project::within;

/// The message of [`ConfineError::OutsideRoot`].
pub const ERR_OUTSIDE_ROOT: &str = "resolves outside project root";

/// Why [`confine`] could not resolve a path.
#[derive(Debug)]
pub enum ConfineError {
    /// The path resolves outside the project root.
    OutsideRoot,
    /// The path, or the root, could not be resolved. `path` is the one
    /// that was being resolved.
    Io { path: String, source: io::Error },
}

impl ConfineError {
    /// Whether the path resolves outside the project root.
    pub fn is_outside_root(&self) -> bool {
        matches!(self, ConfineError::OutsideRoot)
    }
}

impl fmt::Display for ConfineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfineError::OutsideRoot => f.write_str(ERR_OUTSIDE_ROOT),
            ConfineError::Io { path, source } => {
                write!(f, "lstat {path}: {}", io_error_text(source))
            }
        }
    }
}

impl std::error::Error for ConfineError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ConfineError::OutsideRoot => None,
            ConfineError::Io { source, .. } => Some(source),
        }
    }
}

fn io_error(path: &str) -> impl FnOnce(io::Error) -> ConfineError + '_ {
    move |source| ConfineError::Io {
        path: path.to_string(),
        source,
    }
}

/// Resolves `rel` against `dir` (an absolute `rel` as it is), follows
/// symlinks, and returns the real path if it lies under `root`, else
/// [`ConfineError::OutsideRoot`]. The runner reads and writes files
/// through it, and validate checks them with it, so the two agree on what
/// is inside.
pub fn confine(root: &str, dir: &str, rel: &str) -> Result<String, ConfineError> {
    let root_real = path::eval_symlinks(root).map_err(io_error(root))?;
    let joined = if path::is_abs(rel) {
        rel.to_string()
    } else {
        path::join(dir, rel)
    };
    let abs = path::abs(&joined).map_err(io_error(&joined))?;
    let real = match path::eval_symlinks(&abs) {
        Ok(real) => real,
        Err(err) if err.kind() == io::ErrorKind::NotFound => real_prefix(&abs),
        Err(err) => return Err(io_error(&abs)(err)),
    };
    if within(&root_real, &real).is_none() {
        return Err(ConfineError::OutsideRoot);
    }
    Ok(real)
}

/// Resolves the symlinks of the longest existing ancestor of a path that
/// does not exist yet (a `>> file` into a new directory) and keeps the
/// rest as written, so a root under a symlink (macOS's `/var` is
/// `/private/var`) still contains what it should.
pub fn real_prefix(abs: &str) -> String {
    let mut rest = String::new();
    let mut cur = abs.to_string();
    loop {
        if let Ok(real) = path::eval_symlinks(&cur) {
            return path::join(&real, &rest);
        }
        let parent = path::dir(&cur);
        if parent == cur {
            return abs.to_string();
        }
        rest = path::join(&path::base(&cur), &rest);
        cur = parent;
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn root() -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        let real = path::eval_symlinks(&dir.path().to_string_lossy()).unwrap();
        (dir, real)
    }

    #[test]
    fn keeps_paths_inside_the_root() {
        let (_guard, root) = root();
        std::fs::create_dir(format!("{root}/api")).unwrap();
        std::fs::write(format!("{root}/api/body.json"), "{}").unwrap();
        let api = format!("{root}/api");
        assert_eq!(
            confine(&root, &api, "body.json").unwrap(),
            format!("{root}/api/body.json")
        );
        // A file that is not there yet resolves through what is.
        assert_eq!(
            confine(&root, &api, "new/out.json").unwrap(),
            format!("{root}/api/new/out.json")
        );
        assert_eq!(confine(&root, &api, "..").unwrap(), root);
        assert!(confine(&root, &api, "../..").unwrap_err().is_outside_root());
        assert!(confine(&root, &api, "/etc/hosts")
            .unwrap_err()
            .is_outside_root());
        assert_eq!(
            confine(&root, &api, "../..").unwrap_err().to_string(),
            "resolves outside project root"
        );
    }

    #[test]
    fn follows_symlinks_out_of_the_root() {
        let (_guard, root) = root();
        let (_other_guard, other) = self::root();
        std::fs::write(format!("{other}/secret"), "x").unwrap();
        std::os::unix::fs::symlink(&other, format!("{root}/link")).unwrap();
        assert!(confine(&root, &root, "link/secret")
            .unwrap_err()
            .is_outside_root());
        assert!(confine(&root, &root, "link/new")
            .unwrap_err()
            .is_outside_root());
    }

    #[test]
    fn a_missing_root_is_an_error() {
        let (_guard, root) = root();
        let missing = format!("{root}/nope");
        let err = confine(&missing, &missing, "x").unwrap_err();
        assert!(!err.is_outside_root());
        assert_eq!(
            err.to_string(),
            format!("lstat {missing}: no such file or directory")
        );
    }

    #[test]
    fn real_prefix_keeps_what_does_not_exist() {
        let (_guard, root) = root();
        assert_eq!(real_prefix(&format!("{root}/a/b")), format!("{root}/a/b"));
        assert_eq!(real_prefix(&root), root);
    }
}
