//! Reads the runnable blocks out of the course pages: the fenced code
//! blocks under `website/src/content/docs/learn` (and the migration
//! guides) that an `<!-- learn -->` comment marks as something a reader
//! types. The lesson checker runs them so a lesson cannot rot, and the
//! tape recorder runs the ones a tape expects to have happened before it
//! starts recording.

#![forbid(unsafe_code)]

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;

/// Where the course pages live, relative to the repository root.
pub const DIR: &str = "website/src/content/docs/learn";

/// Why a lesson's page or blocks could not be read.
#[derive(Debug)]
pub enum Error {
    /// The course directory or the page could not be read.
    Io(std::io::Error),
    /// There is not exactly one page with the lesson's number prefix.
    Pages {
        /// The number prefix asked for, "02".
        lesson: String,
        /// How many pages carry it.
        found: usize,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(err) => err.fmt(f),
            Error::Pages { lesson, found } => {
                write!(f, "want one lesson page {lesson}-*.md, found {found}")
            }
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(err) => Some(err),
            Error::Pages { .. } => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(err: std::io::Error) -> Self {
        Error::Io(err)
    }
}

/// Finds the one lesson page with that number prefix, "02".
pub fn page(root: impl AsRef<Path>, lesson: &str) -> Result<PathBuf, Error> {
    let dir = root.as_ref().join(DIR);
    let prefix = format!("{lesson}-");
    let mut pages: Vec<PathBuf> = match std::fs::read_dir(&dir) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .filter(|e| {
                let name = e.file_name();
                let name = name.to_string_lossy();
                name.starts_with(&prefix) && name.ends_with(".md")
            })
            .map(|e| e.path())
            .collect(),
        // A missing directory holds no pages.
        Err(_) => Vec::new(),
    };
    pages.sort();
    if pages.len() != 1 {
        return Err(Error::Pages {
            lesson: lesson.to_string(),
            found: pages.len(),
        });
    }
    Ok(pages.remove(0))
}

/// Returns the runnable blocks of the lesson with that number prefix, in
/// page order.
pub fn blocks(root: impl AsRef<Path>, lesson: &str) -> Result<Vec<Block>, Error> {
    let page = page(root, lesson)?;
    let data = std::fs::read(&page)?;
    Ok(extract(
        &page.to_string_lossy(),
        &String::from_utf8_lossy(&data),
    ))
}

/// One runnable fence: where it came from and what it holds.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Block {
    pub file: String,
    /// Line of the opening fence, 1-based.
    pub line: usize,
    pub code: String,
}

static RE_MARKER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[\t\n\f\r ]*<!--[\t\n\f\r ]*learn[\t\n\f\r ]*-->[\t\n\f\r ]*$")
        .expect("a valid pattern")
});
static RE_FENCE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^([\t\n\f\r ]*)(```+|~~~+)[\t\n\f\r ]*([^\t\n\f\r ]*)").expect("a valid pattern")
});

/// Returns the runnable blocks of one Markdown page in order.
pub fn extract(file: &str, content: &str) -> Vec<Block> {
    let mut out = Vec::new();
    let content = content.replace("\r\n", "\n");
    let lines: Vec<&str> = content.split('\n').collect();
    let mut armed = false;
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        i += 1;
        if RE_MARKER.is_match(line) {
            armed = true;
            continue;
        }
        if line.trim().is_empty() {
            continue; // a blank line between the marker and the fence is fine
        }
        let Some(m) = RE_FENCE.captures(line) else {
            armed = false; // something else came before a fence
            continue;
        };
        let (indent, fence) = (&m[1], &m[2]);
        let start = i; // the fence's 1-based line
        let mut body: Vec<&str> = Vec::new();
        while i < lines.len() {
            let inner = lines[i];
            i += 1;
            if closes_fence(inner, fence) {
                break;
            }
            body.push(inner.strip_prefix(indent).unwrap_or(inner));
        }
        if armed {
            out.push(Block {
                file: file.to_string(),
                line: start,
                code: body.join("\n") + "\n",
            });
        }
        armed = false;
    }
    out
}

/// Reports whether `line` closes a block opened with `fence`: the same
/// character, at least as many of them (CommonMark allows more), and
/// nothing else on the line.
fn closes_fence(line: &str, fence: &str) -> bool {
    let t = line.trim();
    if !t.starts_with(fence) {
        return false;
    }
    let mark = fence.chars().next().unwrap_or('`');
    t.trim_matches(mark).is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");

    #[test]
    fn extract_takes_only_marked_fences() {
        let src = "# Title\n\nProse.\n\n<!-- learn -->\n```sh\napic list\n```\n\n```sh\nnot marked\n```\n\n<!-- learn -->\n\n```sh\necho two\necho three\n```\n\n<!-- learn -->\nsome prose in between\n```sh\nnot this one either\n```\n\n    <!-- learn -->\n    ```sh\n    indented\n    ```\n";
        let got = extract("x.md", src);
        assert_eq!(got.len(), 3, "{got:?}");
        assert!(
            got[0].line == 6 && got[0].code == "apic list\n",
            "{:?}",
            got[0]
        );
        assert!(
            got[1].line == 16 && got[1].code == "echo two\necho three\n",
            "{:?}",
            got[1]
        );
        assert_eq!(
            got[2].code, "indented\n",
            "an indented block loses its indent"
        );
        for b in &got {
            assert_eq!(b.file, "x.md");
        }
    }

    /// Pins CommonMark's rule that a closing fence may be longer than the
    /// opening one: ``` closed by ```` must end the block rather than
    /// swallow the rest of the page.
    #[test]
    fn extract_accepts_longer_closing_fences() {
        let src = "<!-- learn -->\n```sh\necho one\n````\n\nprose that is not code\n\n<!-- learn -->\n~~~\necho two\n~~~~~\n\n<!-- learn -->\n```\necho ``` inside is fine\n```  \n";
        let got = extract("x.md", src);
        assert_eq!(got.len(), 3, "{got:?}");
        assert_eq!(got[0].code, "echo one\n");
        assert_eq!(got[1].code, "echo two\n");
        assert_eq!(got[2].code, "echo ``` inside is fine\n");
        let cases = [
            ("```", "```", true),
            ("````", "```", true),
            ("  ```  ", "```", true),
            ("~~~~", "~~~", true),
            ("``", "```", false),
            ("```sh", "```", false),
            ("~~~", "```", false),
            ("echo ```", "```", false),
        ];
        for (line, fence, want) in cases {
            assert_eq!(
                closes_fence(line, fence),
                want,
                "closes_fence({line:?}, {fence:?})"
            );
        }
    }

    #[test]
    fn extract_handles_crlf() {
        let got = extract("x.md", "<!-- learn -->\r\n```sh\r\necho hi\r\n```\r\n");
        assert!(got.len() == 1 && got[0].code == "echo hi\n", "{got:?}");
    }

    /// `page` finds a lesson by its number and `blocks` reads its runnable
    /// blocks, which the recorders' setups count on.
    #[test]
    fn page_and_blocks() {
        let found = page(ROOT, "01").unwrap();
        assert!(
            found.to_string_lossy().ends_with("01-first-request.md"),
            "page(01) = {found:?}"
        );
        let got = blocks(ROOT, "01").unwrap();
        assert!(!got.is_empty());
        let err = page(ROOT, "99").unwrap_err();
        assert_eq!(err.to_string(), "want one lesson page 99-*.md, found 0");
    }
}
