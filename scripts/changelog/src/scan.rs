//! One reading of the changelog's lines, shared by everything that looks
//! at the file.

use std::sync::LazyLock;

use regex::Regex;

/// Matches a Markdown link reference definition, `[0.2.0]: url`.
pub(crate) static LINK_REF: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\[([^\]]+)\]: ").expect("a valid pattern"));

/// Matches the start of a list item, nested or not.
pub(crate) static LIST_ITEM: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[\t\n\f\r ]*(?:[-*+]|[0-9]{1,9}[.)])[\t\n\f\r ]").expect("a valid pattern")
});

/// Matches a release heading: `## [0.2.0] - 2026-10-27`, the
/// `## [Unreleased]` section, or a range such as `## [0.1.1] to [0.1.3] -
/// 2026-10-10` for releases that changed nothing in the binary.
static HEADING: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^## \[([^\]]+)\](?: to \[([^\]]+)\])?(?: - ([0-9]{4}-[0-9]{2}-[0-9]{2}))?[\t\n\f\r ]*$",
    )
    .expect("a valid pattern")
});

/// The parts of a well-formed release heading. `to` is empty unless the
/// heading is a range, and `date` is empty when it carries none.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Heading {
    pub from: String,
    pub to: String,
    pub date: String,
}

impl Heading {
    /// The heading's version, the upper end of a range.
    pub fn version(&self) -> &str {
        if self.to.is_empty() {
            &self.from
        } else {
            &self.to
        }
    }
}

/// One line of the changelog as every reader of it sees it, so `section`,
/// `check`, the site page and the release checks cannot disagree on where
/// a section starts or the file's link references begin.
#[derive(Clone, Debug, Default)]
pub(crate) struct Line {
    /// 1-based.
    pub n: usize,
    /// As written.
    pub text: String,
    /// A `## ` line outside a fenced block before the link references,
    /// which ends the section above it.
    pub h2: bool,
    /// The parts of an `h2` line when it is a well-formed release heading.
    pub heading: Option<Heading>,
    /// A line of the block of link references (and the blank lines among
    /// them) that ends the file. A reference written inside a section's
    /// body is part of that body.
    pub trailing: bool,
    /// The label of a trailing reference.
    pub reference: Option<String>,
    /// A line of a fenced code block, its fences included.
    pub fenced: bool,
}

/// The label of a link reference definition, if the line is one.
pub(crate) fn link_ref(text: &str) -> Option<String> {
    LINK_REF.captures(text).map(|m| m[1].to_string())
}

/// Splits a changelog into lines. A fenced code block (``` or ~~~) is body
/// text, whatever its lines look like: a release note can show a Markdown
/// or .http example with a `## ` line in it.
pub(crate) fn scan(changelog: &str) -> Vec<Line> {
    let mut lines: Vec<Line> = Vec::new();
    let mut fence = String::new(); // the open fence's run of backticks or tildes
    for (i, text) in changelog.split('\n').enumerate() {
        let mut line = Line {
            n: i + 1,
            text: text.to_string(),
            ..Line::default()
        };
        let trimmed = text.trim_start_matches(' ');
        let run = fence_run(trimmed);
        if !fence.is_empty() {
            line.fenced = true;
            // Closed only by a line of the same character, at least as
            // long, and nothing else: "```go" inside a ```` block is text.
            if run.len() >= fence.len()
                && run.as_bytes()[0] == fence.as_bytes()[0]
                && trimmed[run.len()..].trim().is_empty()
            {
                fence.clear();
            }
        } else if !run.is_empty() {
            fence = run.to_string();
            line.fenced = true;
        } else if text.starts_with("## ") {
            line.h2 = true;
            line.heading = HEADING.captures(text).map(|m| Heading {
                from: m[1].to_string(),
                to: m.get(2).map_or("", |g| g.as_str()).to_string(),
                date: m.get(3).map_or("", |g| g.as_str()).to_string(),
            });
        }
        lines.push(line);
    }
    // The trailing block: back from the end over blank lines and link
    // references, stopping at the first line that is neither. A fence left
    // open runs to the end of the file, so it has no trailing block.
    if !fence.is_empty() {
        return lines;
    }
    for line in lines.iter_mut().rev() {
        let reference = link_ref(&line.text);
        if !line.text.trim().is_empty() && reference.is_none() {
            break;
        }
        line.trailing = true;
        line.reference = reference;
        line.h2 = false;
        line.heading = None;
    }
    lines
}

/// The run of three or more backticks or tildes a line opens with, or ""
/// when it opens with no fence.
fn fence_run(trimmed: &str) -> &str {
    for c in *b"`~" {
        let n = trimmed.bytes().take_while(|b| *b == c).count();
        if n >= 3 {
            return &trimmed[..n];
        }
    }
    ""
}
