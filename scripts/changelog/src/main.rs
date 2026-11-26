//! Makes CHANGELOG.md the one place a release is written down, and
//! everything else that names a release follow from it:
//!
//! ```text
//! cargo run -q -p changelog -- 0.2.0           # print a version's notes (Unreleased with none)
//! cargo run -q -p changelog -- -write          # the site's changelog page, and the docs' version pins
//! cargo run -q -p changelog -- -latest         # release.yml: the version to release from main
//! cargo run -q -p changelog -- -release-commit v0.2.0 # release.yml: the commit on main to tag
//! cargo run -q -p changelog -- -release v0.2.0 # release.yml: the notes, after checking the tag
//! cargo run -q -p changelog -- -publish v0.2.0 # release.yml: after a release, move the docs to it
//! ```
//!
//! The newest dated section is the release to cut. The site follows the
//! newest published one, recorded in website/released-version: -write
//! renders the file as the site's changelog page, its sections up to that
//! release, and rewrites every place the docs pin a release (the install
//! commands, setup-apic's version input, the image tags, the course's
//! sample output) to the newest stable release at or below it. A test fails
//! when either is stale. A release pull request only dates its section;
//! once the release has published, release.yml runs -publish, which records
//! the release and regenerates, and commits the result to main, so the site
//! never names a version whose archives do not exist yet. -release is what
//! release.yml runs before building anything: it fails unless the tag is
//! the newest dated section, no higher version is tagged and the docs are
//! current, then prints the section as the release's notes. Without a flag
//! the command prints a version's notes, exiting 1 when there is none or it
//! is empty. Each paragraph and bullet is one line in the file, which
//! `check` enforces: GitHub shows a release body's single newlines as line
//! breaks, so a hard-wrapped bullet would break mid-sentence there.

#![forbid(unsafe_code)]

mod release;
mod scan;
mod site;
#[cfg(test)]
mod tests;

use std::cmp::Ordering;
use std::collections::HashSet;
use std::fmt;
use std::path::Path;
use std::process::exit;
use std::sync::LazyLock;

use apic_gocompat::{path_error, quote, FlagSet};
use regex::Regex;

use release::{newer_tags, release_commit};
use scan::{link_ref, scan, Line, LIST_ITEM};
use site::{from_disk, publish, stale, write, Pin, Reader, PAGE, PINS, VER};

/// What went wrong. The release walk tells the three apart: a file a
/// commit does not have ends it, git failing aborts it, and anything else
/// is a commit that fails the release check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Error {
    /// A file that is not there, on disk or at a commit.
    NotExist(String),
    /// Git failing, as against a file that is not at a commit: the walk in
    /// `release_commit` stops on it rather than reading it as "this commit
    /// fails the release check" and tagging a later one.
    Git(String),
    Other(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NotExist(msg) | Error::Git(msg) | Error::Other(msg) => f.write_str(msg),
        }
    }
}

impl std::error::Error for Error {}

fn other<T>(msg: String) -> Result<T, Error> {
    Err(Error::Other(msg))
}

fn main() {
    let mut flags = FlagSet::new("changelog");
    flags.string("file", "CHANGELOG.md", "the changelog to read");
    flags.bool(
        "write",
        false,
        "write the site's changelog page and the docs' version pins",
    );
    flags.string("release", "", "check that this tag is the newest dated section, that no higher version is tagged and that the docs are current, then print its notes");
    flags.bool(
        "latest",
        false,
        "print the newest dated section's version, which a merge to main releases",
    );
    flags.string("release-commit", "", "print the commit on HEAD's first-parent history to tag for this tag: the first since it was dated that passes the release check");
    flags.string("publish", "", "record this release as published and bring the site's page and the docs' pins in line (release.yml, after a release)");
    flags.parse_env();
    let (file, release_tag, commit, publish_tag) = (
        flags.get("file"),
        flags.get("release"),
        flags.get("release-commit"),
        flags.get("publish"),
    );
    let (do_write, do_latest) = (flags.get_bool("write"), flags.get_bool("latest"));
    let args = flags.args();
    let modes = [
        do_write,
        !release_tag.is_empty(),
        do_latest,
        !commit.is_empty(),
        !publish_tag.is_empty(),
        !args.is_empty(),
    ]
    .iter()
    .filter(|set| **set)
    .count();
    if modes > 1 || args.len() > 1 {
        eprintln!("usage: changelog [-file CHANGELOG.md] [-write | -latest | -release <tag> | -release-commit <tag> | -publish <tag> | <version>]");
        exit(2);
    }
    let changelog = match std::fs::read(file) {
        Ok(data) => String::from_utf8_lossy(&data).into_owned(),
        Err(err) => fail(&path_error("open", Path::new(file), &err)),
    };
    let path = Path::new(file);
    let root = match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir.to_string_lossy().into_owned(),
        _ => ".".to_string(),
    };
    let name = path
        .file_name()
        .map_or_else(|| file.to_string(), |n| n.to_string_lossy().into_owned());
    let bare = |tag: &str| tag.strip_prefix('v').unwrap_or(tag).to_string();
    let result = if do_latest {
        latest(&changelog)
    } else if do_write {
        write(&root, &PINS, &changelog).map(|()| String::new())
    } else if !publish_tag.is_empty() {
        publish(&root, &PINS, &changelog, &bare(publish_tag)).map(|()| String::new())
    } else if !release_tag.is_empty() {
        let tag = bare(release_tag);
        release(&from_disk(&root), &PINS, &changelog, &tag)
            .and_then(|body| no_newer_tags(&root, &tag).map(|()| body))
    } else if !commit.is_empty() {
        let tag = bare(commit);
        no_newer_tags(&root, &tag).and_then(|()| release_commit(&root, &PINS, &name, "HEAD", &tag))
    } else {
        let version = args.first().map_or(UNRELEASED.to_string(), |v| bare(v));
        section(&changelog, &version)
    };
    match result {
        // Writing reports each file as it goes and has nothing to print.
        Ok(_) if do_write || !publish_tag.is_empty() => {}
        Ok(text) => println!("{text}"),
        Err(err) => fail(&err.to_string()),
    }
}

/// Refuses a release of `tag` while a higher version is tagged.
fn no_newer_tags(root: &str, tag: &str) -> Result<(), Error> {
    let newer = newer_tags(root, tag)?;
    if !newer.is_empty() {
        return other(format!(
            "{} is tagged above v{tag}: releasing it again would move latest, the cask and v0 back to it",
            newer.join(", ")
        ));
    }
    Ok(())
}

fn fail(msg: &str) -> ! {
    eprintln!("{msg}");
    exit(1);
}

/// The name of the section a change goes under until its release is cut.
pub(crate) const UNRELEASED: &str = "Unreleased";

/// Matches a release's version: three numbers, and a prerelease suffix for
/// a release candidate (0.3.0-rc.1).
pub(crate) static SEMVER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!("^{VER}$")).expect("a valid pattern"));

/// The newest release: the version of the newest dated section, the upper
/// end of a range heading. It is what release.yml checks the tag against.
/// The changelog must pass [`check`] first: a section dated below an older
/// one would otherwise hide it, and the release would be skipped with a
/// green run.
pub(crate) fn latest(changelog: &str) -> Result<String, Error> {
    newest(changelog, |_| true)
}

/// The newest release that is not a prerelease, at or below `up_to` when
/// that is set: what the docs pin, a release candidate being for those who
/// ask for it by name.
pub(crate) fn stable(changelog: &str, up_to: &str) -> Result<String, Error> {
    newest(changelog, |v| {
        is_stable(v) && (up_to.is_empty() || compare_versions(v, up_to).is_le())
    })
}

/// Reports whether a version is a release rather than a prerelease.
fn is_stable(v: &str) -> bool {
    !v.contains('-')
}

fn newest(changelog: &str, want: impl Fn(&str) -> bool) -> Result<String, Error> {
    let problems = check(changelog);
    if !problems.is_empty() {
        return other(format!("CHANGELOG.md:\n  {}", problems.join("\n  ")));
    }
    for l in scan(changelog) {
        let Some(h) = &l.heading else { continue };
        if h.date.is_empty() || h.from == UNRELEASED {
            continue;
        }
        if want(h.version()) {
            return Ok(h.version().to_string());
        }
    }
    other("CHANGELOG.md has no dated release section".to_string())
}

/// Returns the notes for `tag` after checking that it is the release to
/// cut: the newest dated section, with the site's page and pins current
/// for the release published before it (they move to `tag` only once it is
/// out, by -publish). A tag cut before the release pull request merged, or
/// on a commit whose docs a hand edit left stale, fails here with nothing
/// built.
pub(crate) fn release(
    read: &Reader,
    pins: &[Pin],
    changelog: &str,
    tag: &str,
) -> Result<String, Error> {
    let latest = latest(changelog)?;
    if tag != latest {
        return other(format!("the tag is {tag} but CHANGELOG.md's newest dated section is {latest}: date this release's section (## [{tag}] - YYYY-MM-DD) before tagging"));
    }
    let problems: Vec<String> = stale(read, pins, changelog)?
        .into_iter()
        .map(|f| {
            if f == PAGE {
                format!("{f} does not match CHANGELOG.md")
            } else {
                format!("{f} does not pin the newest stable release at or below the published one")
            }
        })
        .collect();
    if !problems.is_empty() {
        return other(format!(
            "{} (website/released-version): run task docs:changelog on main and merge the result",
            problems.join("; ")
        ));
    }
    section(changelog, tag)
}

/// Returns the body under `version`'s heading: the lines up to the next
/// section or the link references that end the file, with the blank lines
/// around them removed. A range heading serves every version it names and
/// the ones between. The body must not be empty: a release with nothing to
/// say has nothing to attach.
pub(crate) fn section(changelog: &str, version: &str) -> Result<String, Error> {
    let mut body: Vec<String> = Vec::new();
    let mut found = false;
    for l in scan(changelog) {
        if found {
            if l.h2 || l.trailing {
                break;
            }
            body.push(l.text);
            continue;
        }
        if l.heading
            .as_ref()
            .is_some_and(|h| covers(&h.from, &h.to, version))
        {
            found = true;
        }
    }
    if !found {
        return other(format!("CHANGELOG.md has no section for {version}: add one under its own heading before tagging"));
    }
    let joined = body.join("\n");
    let text = joined.trim();
    if text.is_empty() {
        return other(format!(
            "CHANGELOG.md's section for {version} is empty: write what changed before tagging"
        ));
    }
    Ok(format!("{text}{}", used_refs(changelog, text)))
}

/// Returns the file's link reference definitions that a section's text
/// uses (`[label]` or `[text][label]`) without defining, so its notes
/// stand alone: a release body on GitHub has no file around it to resolve
/// them. Matching is case-insensitive, as Markdown's is.
fn used_refs(changelog: &str, text: &str) -> String {
    let mut defined: HashSet<String> = scan(text)
        .iter()
        .filter_map(|l| link_ref(&l.text))
        .map(|label| label.to_lowercase())
        .collect();
    let lower = text.to_lowercase();
    let mut out: Vec<String> = Vec::new();
    for l in scan(changelog) {
        let Some(label) = link_ref(&l.text) else {
            continue;
        };
        if l.fenced {
            continue;
        }
        let label = label.to_lowercase();
        if defined.contains(&label) || !lower.contains(&format!("[{label}]")) {
            continue;
        }
        defined.insert(label);
        out.push(l.text);
    }
    if out.is_empty() {
        return String::new();
    }
    format!("\n\n{}", out.join("\n"))
}

/// Reports whether a heading for `from` (and, for a range, up to `to`)
/// names `version`: the same string, or one between the two bounds in
/// version order.
fn covers(from: &str, to: &str, version: &str) -> bool {
    if from == version || to == version {
        return true;
    }
    if to.is_empty() {
        return false;
    }
    compare_versions(from, version).is_le() && compare_versions(version, to).is_le()
}

/// Orders versions: numerically by their dotted parts, so "0.10.0" is
/// above "0.9.1"; a prerelease below the release it leads to, so
/// "0.3.0-rc.1" is below "0.3.0" and above "0.2.0"; and a word such as
/// Unreleased above everything.
pub(crate) fn compare_versions(a: &str, b: &str) -> Ordering {
    let (a, pre_a) = a.split_once('-').unwrap_or((a, ""));
    let (b, pre_b) = b.split_once('-').unwrap_or((b, ""));
    let c = compare_parts(a, b);
    if c != Ordering::Equal || pre_a == pre_b {
        return c;
    }
    if pre_a.is_empty() {
        return Ordering::Greater;
    }
    if pre_b.is_empty() {
        return Ordering::Less;
    }
    compare_prerelease(pre_a, pre_b)
}

/// Orders two prerelease suffixes as semver does: dot by dot, numbers
/// numerically, words by their bytes, a number below a word, and a suffix
/// that runs out first below a longer one with the same start. So alpha.1
/// < alpha.2 < beta.1 < rc.1.
fn compare_prerelease(a: &str, b: &str) -> Ordering {
    let (parts_a, parts_b): (Vec<&str>, Vec<&str>) =
        (a.split('.').collect(), b.split('.').collect());
    for (x, y) in parts_a.iter().zip(&parts_b) {
        let c = match (number(x).is_some(), number(y).is_some()) {
            (true, true) => compare_parts(x, y),
            (true, false) => return Ordering::Less,
            (false, true) => return Ordering::Greater,
            (false, false) => x.as_bytes().cmp(y.as_bytes()),
        };
        if c != Ordering::Equal {
            return c;
        }
    }
    parts_a.len().cmp(&parts_b.len())
}

/// Reads a version part as a non-negative number: digits only, no sign,
/// and small enough for a 64-bit integer.
fn number(s: &str) -> Option<i64> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

fn compare_parts(a: &str, b: &str) -> Ordering {
    let (parts_a, parts_b): (Vec<&str>, Vec<&str>) =
        (a.split('.').collect(), b.split('.').collect());
    for i in 0..parts_a.len().max(parts_b.len()) {
        let x = parts_a.get(i).map_or(0, |p| numeric(p));
        let y = parts_b.get(i).map_or(0, |p| numeric(p));
        if x != y {
            return x.cmp(&y);
        }
    }
    Ordering::Equal
}

/// A version part as a number, with anything that is not one (a word such
/// as Unreleased) ranking above every number.
fn numeric(part: &str) -> i64 {
    number(part).unwrap_or(1 << 30)
}

/// Reports whether `cur` is prose carried on from the line above it, a
/// hard wrap of a paragraph or bullet: both lines hold text, outside a
/// fence and the closing references, the one above is not a heading, and
/// `cur` does not start a block of its own (an item, a heading, a table
/// row, a quote or a link reference).
fn wrapped(above: &Line, cur: &Line) -> bool {
    if cur.fenced || above.fenced || cur.trailing || above.trailing {
        return false;
    }
    let (a, c) = (above.text.trim(), cur.text.trim());
    if a.is_empty() || c.is_empty() || a.starts_with('#') {
        return false;
    }
    if LIST_ITEM.is_match(&cur.text) || scan::LINK_REF.is_match(c) {
        return false;
    }
    !c.starts_with('#') && !c.starts_with('|') && !c.starts_with('>')
}

/// Reports what is wrong with a changelog's shape: the first section must
/// be Unreleased, every other heading must carry a date and name a version
/// below the one before it, and each version named must have a link
/// reference at the end of the file.
pub(crate) fn check(changelog: &str) -> Vec<String> {
    let mut problems: Vec<String> = Vec::new();
    let lines = scan(changelog);
    let refs: HashSet<&str> = lines
        .iter()
        .filter_map(|l| l.reference.as_deref())
        .collect();
    for pair in lines.windows(2) {
        if wrapped(&pair[0], &pair[1]) {
            problems.push(format!("line {} continues the line above: write each paragraph and bullet on one line, since GitHub shows a release's single newlines as breaks", pair[1].n));
        }
    }
    let mut prev = String::new();
    let mut first = true;
    for l in lines.iter().filter(|l| l.h2) {
        let n = l.n;
        let Some(h) = &l.heading else {
            problems.push(format!(
                "line {n}: {} is not a release heading (## [version] - YYYY-MM-DD)",
                quote(&l.text)
            ));
            continue;
        };
        let (from, to, date) = (h.from.as_str(), h.to.as_str(), h.date.as_str());
        for v in [from, to] {
            if !v.is_empty() && v != UNRELEASED && !SEMVER.is_match(v) {
                problems.push(format!(
                    "line {n}: {} is not a version like 0.2.0 or 0.3.0-rc.1",
                    quote(v)
                ));
            }
        }
        if first {
            first = false;
            if from != UNRELEASED {
                problems.push(format!("line {n}: the first section must be Unreleased"));
            }
        } else if from == UNRELEASED || to == UNRELEASED {
            problems.push(format!("line {n}: only the first section is Unreleased"));
        }
        if from == UNRELEASED && !date.is_empty() {
            problems.push(format!(
                "line {n}: Unreleased has a date; a release's section is ## [x.y.z] - YYYY-MM-DD"
            ));
        }
        if !to.is_empty()
            && SEMVER.is_match(from)
            && SEMVER.is_match(to)
            && compare_versions(from, to).is_ge()
        {
            problems.push(format!(
                "line {n}: the range {from} to {to} runs backwards; the lower version comes first"
            ));
        }
        for v in [from, to] {
            if v.is_empty() {
                continue;
            }
            if v != UNRELEASED && date.is_empty() {
                problems.push(format!("line {n}: {v} has no release date"));
            }
            if !refs.contains(v) {
                problems.push(format!(
                    "line {n}: no link reference for [{v}] at the end of the file"
                ));
            }
        }
        let top = h.version();
        if !prev.is_empty() && compare_versions(top, &prev).is_ge() {
            problems.push(format!("line {n}: {top} is not below {prev}; newest first"));
        }
        prev = from.to_string();
    }
    if first {
        problems.push("no release headings".to_string());
    }
    problems
}
