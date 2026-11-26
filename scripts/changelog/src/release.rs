//! What the release workflow asks of history: a file as it was at a
//! commit, the commit to tag, and the tags above a release.

use std::cell::RefCell;
use std::collections::HashSet;
use std::process::Command;

use crate::site::{Pin, Reader};
use crate::{compare_versions, latest, release, Error, SEMVER};

/// Runs git in `dir` with its messages in English whatever the locale, and
/// returns its output or an [`Error::Git`] carrying what it wrote to
/// stderr.
pub(crate) fn git(dir: &str, args: &[&str]) -> Result<String, Error> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("LC_ALL", "C")
        .output()
        .map_err(|err| Error::Git(format!("git {}: {err}", args.join(" "))))?;
    if !out.status.success() {
        let status = match out.status.code() {
            Some(code) => format!("exit status {code}"),
            None => out.status.to_string(),
        };
        return Err(Error::Git(format!(
            "git {}: {status}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Reads files as they were at `commit`. Paths are relative to `dir`, the
/// directory holding the changelog, as they are on disk, whichever
/// directory of the repository that is.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn from_git(dir: &str, commit: &str) -> Reader {
    match git(dir, &["rev-parse", "--show-prefix"]) {
        Ok(prefix) => tree_reader(dir, prefix.trim(), commit),
        Err(err) => Box::new(move |_| Err(err.clone())),
    }
}

/// Reads files at `commit`, `prefix` being `dir`'s path from the
/// repository's top. The commit's tree is listed once, on the first read,
/// so a path it does not have is [`Error::NotExist`] without asking git
/// about each one or reading its message; each file present costs one
/// `git show`. Anything git fails at is an [`Error::Git`].
fn tree_reader(dir: &str, prefix: &str, commit: &str) -> Reader {
    let (dir, prefix, commit) = (dir.to_string(), prefix.to_string(), commit.to_string());
    let files: RefCell<Option<HashSet<String>>> = RefCell::new(None);
    Box::new(move |path| {
        if files.borrow().is_none() {
            let listed = git(
                &dir,
                &["ls-tree", "-r", "--full-tree", "--name-only", &commit],
            )?;
            *files.borrow_mut() = Some(listed.split('\n').map(str::to_string).collect());
        }
        let full = format!("{prefix}{path}");
        if !files.borrow().as_ref().is_some_and(|f| f.contains(&full)) {
            return Err(Error::NotExist(format!(
                "read {commit}:{full}: file does not exist"
            )));
        }
        git(&dir, &["show", &format!("{commit}:{full}")])
    })
}

/// Picks the commit to tag for `tag`: walking `head`'s first-parent
/// history (main's own commits, a merge counting as one) back to where
/// `tag` stopped being the newest dated section, it returns the oldest
/// commit at which the release check passes. That is the commit the
/// release pull request left on main, not whichever later push ran the
/// workflow, so changes merged since, still under Unreleased, are not
/// shipped under this version's notes. When a pull request dated the
/// section in one commit and caught the docs up in the next, and both
/// landed on main as they were, the second is the one that passes. A
/// commit whose changelog is malformed is skipped, not taken for the start
/// of the release: the walk goes on to the commit that dated it. A git
/// failure anywhere ends the walk with an error rather than narrowing it.
/// `name` is the changelog's file name in `dir`.
pub(crate) fn release_commit(
    dir: &str,
    pins: &[Pin],
    name: &str,
    head: &str,
    tag: &str,
) -> Result<String, Error> {
    let out = git(dir, &["rev-list", "--first-parent", head])?;
    let prefix = git(dir, &["rev-parse", "--show-prefix"])?;
    let prefix = prefix.trim();
    struct Candidate {
        commit: String,
        changelog: String,
        read: Reader,
    }
    let mut candidates: Vec<Candidate> = Vec::new(); // newest first
    for c in out.split_whitespace() {
        let read = tree_reader(dir, prefix, c);
        let changelog = match read(name) {
            Ok(text) => text,
            Err(Error::NotExist(_)) => break, // before the changelog existed
            Err(err) => return Err(err),
        };
        match latest(&changelog) {
            // Malformed here: not a candidate, and not the start either.
            Err(_) => continue,
            Ok(v) if v != tag => break, // before this version was dated
            Ok(_) => {}
        }
        candidates.push(Candidate {
            commit: c.to_string(),
            changelog,
            read,
        });
    }
    if candidates.is_empty() {
        return Err(Error::Other(format!(
            "{tag} is not the newest dated section of CHANGELOG.md at {head}"
        )));
    }
    let mut last = String::new();
    for c in candidates.iter().rev() {
        match release(&c.read, pins, &c.changelog, tag) {
            Ok(_) => return Ok(c.commit.clone()),
            Err(err @ Error::Git(_)) => return Err(err),
            Err(err) => last = err.to_string(),
        }
    }
    Err(Error::Other(format!(
        "no commit on main since {tag} was dated passes the release check; at {head}: {last}"
    )))
}

/// Lists the version tags in the repository at `dir` that are above `tag`,
/// lowest first. Releasing `tag` again while one exists would move the
/// image's latest, the cask and setup-apic's v0 back to an older release,
/// so the release check refuses it.
pub(crate) fn newer_tags(dir: &str, tag: &str) -> Result<Vec<String>, Error> {
    let out = git(dir, &["tag", "--list", "v[0-9]*"])?;
    let mut newer: Vec<String> = out
        .split_whitespace()
        .filter(|t| {
            let v = t.strip_prefix('v').unwrap_or(t);
            SEMVER.is_match(v) && compare_versions(v, tag).is_gt()
        })
        .map(str::to_string)
        .collect();
    newer.sort_by(|a, b| compare_versions(&a[1..], &b[1..]));
    Ok(newer)
}
