//! Lexical path helpers over strings. A project keeps its paths as text
//! (they are compared, sorted and printed), so the operations here work on
//! the text alone and never touch the file system, except [`abs`], which
//! reads the working directory, and [`eval_symlinks`].

use std::path::{Path, MAIN_SEPARATOR, MAIN_SEPARATOR_STR};

fn is_sep(b: u8) -> bool {
    b == b'/' || (cfg!(windows) && b == b'\\')
}

/// Whether the path is absolute.
pub fn is_abs(path: &str) -> bool {
    Path::new(path).is_absolute()
}

/// The shortest path naming the same place, worked out from the text
/// alone: repeated separators and `.` elements go, and each `..` removes
/// the element before it (at the root it is dropped). An empty path is
/// `.`.
pub fn clean(path: &str) -> String {
    let p = path.as_bytes();
    let n = p.len();
    if n == 0 {
        return ".".to_string();
    }
    let sep = MAIN_SEPARATOR as u8;
    let rooted = is_sep(p[0]);
    let mut out: Vec<u8> = Vec::with_capacity(n);
    let (mut r, mut dotdot) = (0, 0);
    if rooted {
        out.push(sep);
        r = 1;
        dotdot = 1;
    }
    while r < n {
        // An empty element, or a `.`: nothing to keep.
        if is_sep(p[r]) || (p[r] == b'.' && (r + 1 == n || is_sep(p[r + 1]))) {
            r += 1;
        } else if p[r] == b'.' && p[r + 1] == b'.' && (r + 2 == n || is_sep(p[r + 2])) {
            r += 2;
            if out.len() > dotdot {
                // Back up over the element before it.
                let mut w = out.len() - 1;
                while w > dotdot && !is_sep(out[w]) {
                    w -= 1;
                }
                out.truncate(w);
            } else if !rooted {
                // Nothing to back up over: keep the `..`.
                if !out.is_empty() {
                    out.push(sep);
                }
                out.extend_from_slice(b"..");
                dotdot = out.len();
            }
        } else {
            if (rooted && out.len() != 1) || (!rooted && !out.is_empty()) {
                out.push(sep);
            }
            while r < n && !is_sep(p[r]) {
                out.push(p[r]);
                r += 1;
            }
        }
    }
    if out.is_empty() {
        return ".".to_string();
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Joins two elements with a separator and cleans the result. An empty
/// element is ignored, and an absolute second element does not replace the
/// first: `join("/a", "/b")` is `/a/b`.
pub fn join(a: &str, b: &str) -> String {
    match (a.is_empty(), b.is_empty()) {
        (true, true) => String::new(),
        (true, false) => clean(b),
        (false, true) => clean(a),
        (false, false) => clean(&format!("{a}{MAIN_SEPARATOR}{b}")),
    }
}

/// Joins any number of elements: see [`join`].
pub fn join_all(parts: &[&str]) -> String {
    let parts: Vec<&str> = parts.iter().copied().filter(|p| !p.is_empty()).collect();
    if parts.is_empty() {
        return String::new();
    }
    clean(&parts.join(MAIN_SEPARATOR_STR))
}

/// The path made absolute against the working directory, and cleaned.
pub fn abs(path: &str) -> std::io::Result<String> {
    if is_abs(path) {
        return Ok(clean(path));
    }
    let cwd = std::env::current_dir()?;
    Ok(join(&cwd.to_string_lossy(), path))
}

/// Everything but the last element of the path, cleaned; `.` when the
/// path has no directory part.
pub fn dir(path: &str) -> String {
    let p = path.as_bytes();
    let mut i = p.len();
    while i > 0 && !is_sep(p[i - 1]) {
        i -= 1;
    }
    clean(&path[..i])
}

/// The last element of the path; `.` for an empty path.
pub fn base(path: &str) -> String {
    if path.is_empty() {
        return ".".to_string();
    }
    let trimmed = path.trim_end_matches(|c: char| c.is_ascii() && is_sep(c as u8));
    if trimmed.is_empty() {
        return MAIN_SEPARATOR.to_string();
    }
    let p = trimmed.as_bytes();
    let mut i = p.len();
    while i > 0 && !is_sep(p[i - 1]) {
        i -= 1;
    }
    trimmed[i..].to_string()
}

/// The extension of the last element, from its final dot: `.http`. A name
/// that is all extension (`.http`) is its own extension.
pub fn ext(path: &str) -> &str {
    let p = path.as_bytes();
    let mut i = p.len();
    while i > 0 && !is_sep(p[i - 1]) {
        if p[i - 1] == b'.' {
            return &path[i - 1..];
        }
        i -= 1;
    }
    ""
}

/// The path with forward slashes, the form a project keys its files by.
pub fn to_slash(path: &str) -> String {
    if MAIN_SEPARATOR == '/' {
        path.to_string()
    } else {
        path.replace(MAIN_SEPARATOR, "/")
    }
}

/// The path that leads from `base` to `target`, from the text alone. Both
/// must be absolute or both relative, and a relative `base` must not climb
/// with `..` past what `target` shares with it.
pub fn rel(base: &str, target: &str) -> Result<String, String> {
    let fail = || format!("Rel: can't make {target} relative to {base}");
    let base_clean = clean(base);
    let targ = clean(target);
    if targ == base_clean {
        return Ok(".".to_string());
    }
    let base_clean = if base_clean == "." {
        String::new()
    } else {
        base_clean
    };
    let (b, t) = (base_clean.as_bytes(), targ.as_bytes());
    let slashed = |p: &[u8]| p.first().is_some_and(|&c| is_sep(c));
    if slashed(b) != slashed(t) {
        return Err(fail());
    }
    // Position b[b0..bi] and t[t0..ti] at the first elements that differ.
    let (bl, tl) = (b.len(), t.len());
    let (mut b0, mut bi, mut t0, mut ti) = (0, 0, 0, 0);
    loop {
        while bi < bl && !is_sep(b[bi]) {
            bi += 1;
        }
        while ti < tl && !is_sep(t[ti]) {
            ti += 1;
        }
        if t[t0..ti] != b[b0..bi] {
            break;
        }
        if bi < bl {
            bi += 1;
        }
        if ti < tl {
            ti += 1;
        }
        b0 = bi;
        t0 = ti;
    }
    if &b[b0..bi] == b".." {
        return Err(fail());
    }
    if b0 != bl {
        // Elements of the base are left: go up before going down.
        let ups = 1 + b[b0..bl].iter().filter(|&&c| is_sep(c)).count();
        let mut out = vec![".."; ups].join(MAIN_SEPARATOR_STR);
        if t0 != tl {
            out.push(MAIN_SEPARATOR);
            out.push_str(&targ[t0..]);
        }
        return Ok(out);
    }
    Ok(targ[t0..].to_string())
}

/// The path with every symbolic link followed. The path must exist.
pub fn eval_symlinks(path: &str) -> std::io::Result<String> {
    std::fs::canonicalize(path).map(|p| p.to_string_lossy().into_owned())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn cleans() {
        let cases = [
            ("", "."),
            ("abc", "abc"),
            ("abc/def", "abc/def"),
            ("a/b/c", "a/b/c"),
            (".", "."),
            ("..", ".."),
            ("../..", "../.."),
            ("../../abc", "../../abc"),
            ("/abc", "/abc"),
            ("/", "/"),
            ("abc/", "abc"),
            ("abc/def/", "abc/def"),
            ("a/b/c/", "a/b/c"),
            ("./", "."),
            ("../", ".."),
            ("../../", "../.."),
            ("/abc/", "/abc"),
            ("abc//def//ghi", "abc/def/ghi"),
            ("//abc", "/abc"),
            ("///abc", "/abc"),
            ("//abc//", "/abc"),
            ("abc//", "abc"),
            ("abc/./def", "abc/def"),
            ("/./abc/def", "/abc/def"),
            ("abc/.", "abc"),
            ("abc/def/ghi/../jkl", "abc/def/jkl"),
            ("abc/def/../ghi/../jkl", "abc/jkl"),
            ("abc/def/..", "abc"),
            ("abc/def/../..", "."),
            ("/abc/def/../..", "/"),
            ("abc/def/../../..", ".."),
            ("/abc/def/../../..", "/"),
            ("abc/def/../../../ghi/jkl/../../../mno", "../../mno"),
            ("/../abc", "/abc"),
            ("abc/./../def", "def"),
            ("abc//./../def", "def"),
            ("abc/../../././../def", "../../def"),
        ];
        for (input, want) in cases {
            assert_eq!(clean(input), want, "clean({input:?})");
        }
    }

    #[test]
    fn joins() {
        assert_eq!(join("a", "b"), "a/b");
        assert_eq!(join("a", ""), "a");
        assert_eq!(join("", "b"), "b");
        assert_eq!(join("", ""), "");
        assert_eq!(join("/a", "/b"), "/a/b");
        assert_eq!(join("a/", "../b"), "b");
        assert_eq!(join("/", "a"), "/a");
        assert_eq!(join_all(&["/r", ".", "x.json"]), "/r/x.json");
        assert_eq!(join_all(&["", ""]), "");
    }

    #[test]
    fn splits() {
        assert_eq!(dir("a.http"), ".");
        assert_eq!(dir("api/a.http"), "api");
        assert_eq!(dir("/a"), "/");
        assert_eq!(dir(""), ".");
        assert_eq!(dir("a/b/"), "a/b");
        assert_eq!(base("a/b.http"), "b.http");
        assert_eq!(base("a/b/"), "b");
        assert_eq!(base(""), ".");
        assert_eq!(base("///"), "/");
        assert_eq!(ext("a/b.http"), ".http");
        assert_eq!(ext("a.b/c"), "");
        assert_eq!(ext(".http"), ".http");
        assert_eq!(ext("a.tar.gz"), ".gz");
        assert_eq!(ext("a."), ".");
    }

    #[test]
    fn relative_paths() {
        let good = [
            ("a/b", "a/b", "."),
            ("a/b/.", "a/b", "."),
            ("a/b", "a/b/.", "."),
            ("./a/b", "a/b", "."),
            ("ab/cd", "ab/cde", "../cde"),
            ("ab/cd", "ab/c", "../c"),
            ("a/b", "a/b/c/d", "c/d"),
            ("a/b", "a/b/../c", "../c"),
            ("a/b/../c", "a/b", "../b"),
            ("a/b/c", "a/c/d", "../../c/d"),
            ("a/b", "c/d", "../../c/d"),
            ("a/b/c/d", "a/b", "../.."),
            ("a/b/c/d", "a/b/", "../.."),
            ("../../a/b", "../../a/b/c/d", "c/d"),
            ("/a/b", "/a/b", "."),
            ("/a/b/.", "/a/b", "."),
            ("/ab/cd", "/ab/cde", "../cde"),
            ("/a/b", "/a/b/c/d", "c/d"),
            ("/a/b", "/a/b/../c", "../c"),
            ("/a/b/c", "/a/c/d", "../../c/d"),
            ("/a/b", "/c/d", "../../c/d"),
            ("/a/b/c/d", "/a/b", "../.."),
            ("/../../a/b", "/../../a/b/c/d", "c/d"),
            (".", "a/b", "a/b"),
            (".", "..", ".."),
            ("/", "/a", "a"),
            ("/a", "/", ".."),
        ];
        for (b, t, want) in good {
            assert_eq!(rel(b, t).as_deref(), Ok(want), "rel({b:?}, {t:?})");
        }
        for (b, t) in [
            ("..", "."),
            ("..", "a"),
            ("../..", ".."),
            ("a", "/a"),
            ("/a", "a"),
        ] {
            assert!(rel(b, t).is_err(), "rel({b:?}, {t:?}) should fail");
        }
        assert_eq!(
            rel("a", "/a").unwrap_err(),
            "Rel: can't make /a relative to a"
        );
    }
}
