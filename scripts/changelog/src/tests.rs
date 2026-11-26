use std::path::{Path, PathBuf};
use std::process::Command;

use crate::release::{from_git, newer_tags, release_commit};
use crate::site::{from_disk, pin, pinned, publish, site_page, stale, write, PAGE, PINS, RELEASED};
use crate::{check, compare_versions, latest, release, section, stable, Error};

const SAMPLE: &str = "# Changelog

## [Unreleased]

## [0.2.0] - 2026-10-27

### Added

- A thing.

## [0.1.1] to [0.1.3] - 2026-10-10

Pipeline only.

## [0.1.0] - 2026-10-10

The first release.

[Unreleased]: https://example.com/compare/v0.2.0...HEAD
[0.2.0]: https://example.com/compare/v0.1.4...v0.2.0
[0.1.3]: https://example.com/compare/v0.1.2...v0.1.3
[0.1.2]: https://example.com/compare/v0.1.1...v0.1.2
[0.1.1]: https://example.com/compare/v0.1.0...v0.1.1
[0.1.0]: https://example.com/releases/tag/v0.1.0
";

fn repo_root() -> String {
    format!("{}/../..", env!("CARGO_MANIFEST_DIR"))
}

fn repo_changelog() -> String {
    std::fs::read_to_string(Path::new(&repo_root()).join("CHANGELOG.md")).expect("CHANGELOG.md")
}

fn err_text<T: std::fmt::Debug>(r: Result<T, Error>) -> String {
    r.expect_err("an error").to_string()
}

fn path_str(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

fn write_file(root: &Path, name: &str, content: &str) {
    let path = root.join(name);
    std::fs::create_dir_all(path.parent().expect("a parent")).expect("mkdir");
    std::fs::write(path, content).expect("write");
}

fn read_file(root: &Path, name: &str) -> String {
    std::fs::read_to_string(root.join(name)).expect("read")
}

fn have_git() -> bool {
    Command::new("git").arg("--version").output().is_ok()
}

#[test]
fn section_finds_a_version_and_refuses_what_is_not_there() {
    assert_eq!(section(SAMPLE, "0.2.0").unwrap(), "### Added\n\n- A thing.");
    // A range heading serves every version it names and those between.
    for v in ["0.1.1", "0.1.2", "0.1.3"] {
        assert_eq!(section(SAMPLE, v).unwrap(), "Pipeline only.", "{v}");
    }
    assert!(
        err_text(section(SAMPLE, "0.1.4")).contains("no section for 0.1.4"),
        "0.1.4 is not in the range"
    );
    assert!(
        err_text(section(SAMPLE, "Unreleased")).contains("empty"),
        "an empty section is refused"
    );
    assert!(
        section(SAMPLE, "9.9.9").is_err(),
        "a missing version should be an error"
    );
    // The oldest section ends at the link references, not the file's end.
    assert_eq!(section(SAMPLE, "0.1.0").unwrap(), "The first release.");
}

#[test]
fn check_names_every_shape_problem() {
    assert!(check(SAMPLE).is_empty(), "the sample is well formed");
    let bad =
        "# x\n\n## [0.2.0] - 2026-10-27\n\nA.\n\n## [0.3.0]\n\nB.\n\n## Notes\n\n[0.2.0]: u\n";
    let p = check(bad).join("\n");
    for want in [
        "first section must be Unreleased",
        "0.3.0 has no release date",
        "no link reference for [0.3.0]",
        "0.3.0 is not below 0.2.0",
        "is not a release heading",
    ] {
        assert!(p.contains(want), "missing {want:?} in:\n{p}");
    }
    assert!(
        compare_versions("0.10.0", "0.9.1").is_gt()
            && compare_versions("Unreleased", "1.0.0").is_gt()
            && compare_versions("1.0", "1.0.0").is_eq(),
        "version order"
    );
    // A prerelease sits between the release before it and the one it leads to.
    assert!(
        compare_versions("0.3.0-rc.1", "0.3.0").is_lt()
            && compare_versions("0.3.0-rc.1", "0.2.0").is_gt()
            && compare_versions("0.3.0-rc.2", "0.3.0-rc.1").is_gt(),
        "prerelease order"
    );
    // Semver's order for the suffix: words by their bytes, a number below a
    // word, a longer suffix above its own start.
    let order = [
        "0.3.0-1",
        "0.3.0-alpha",
        "0.3.0-alpha.1",
        "0.3.0-alpha.2",
        "0.3.0-alpha.10",
        "0.3.0-beta.1",
        "0.3.0-beta.2",
        "0.3.0-rc.1",
        "0.3.0",
    ];
    for pair in order.windows(2) {
        assert!(
            compare_versions(pair[0], pair[1]).is_lt(),
            "{} should be below {}",
            pair[0],
            pair[1]
        );
    }
    let ordered = "## [Unreleased]\n\n## [0.3.0-rc.1] - 2026-10-29\n\nC.\n\n## [0.3.0-beta.2] - 2026-10-28\n\nB.\n\n## [0.3.0-alpha.1] - 2026-10-27\n\nA.\n\n[Unreleased]: u\n[0.3.0-rc.1]: u\n[0.3.0-beta.2]: u\n[0.3.0-alpha.1]: u\n";
    assert!(
        check(ordered).is_empty(),
        "validly ordered prereleases: {:?}",
        check(ordered)
    );
    let p = check("## [Unreleased]\n\n## [1] - 2026-10-27\n\nA.\n\n[Unreleased]: u\n[1]: u\n")
        .join("\n");
    assert!(
        p.contains(r#""1" is not a version"#),
        "a malformed version: {p}"
    );
    // A release pull request that dates Unreleased instead of renaming it,
    // a second Unreleased, and a range written backwards are all refused,
    // so `latest` never answers "Unreleased" and `section` finds every
    // version a range covers.
    for (heading, want) in [
        ("## [Unreleased] - 2026-10-28", "Unreleased has a date"),
        (
            "## [Unreleased]\n\n- x\n\n## [Unreleased]",
            "only the first section is Unreleased",
        ),
        (
            "## [Unreleased]\n\n## [0.1.3] to [0.1.1] - 2026-10-10",
            "runs backwards",
        ),
    ] {
        let changelog = format!("{heading}\n\nA.\n\n[Unreleased]: u\n[0.1.1]: u\n[0.1.3]: u\n");
        let p = check(&changelog).join("\n");
        assert!(p.contains(want), "{heading:?}: want {want:?} in:\n{p}");
        assert!(
            latest(&changelog).is_err(),
            "{heading:?}: latest should be an error"
        );
    }
}

/// The repository's own changelog: well formed, with notes for every tag
/// release.yml would attach, which is how a tag without them fails here
/// before it fails on the runner.
#[test]
fn the_changelog_is_well_formed() {
    let data = repo_changelog();
    let p = check(&data);
    assert!(p.is_empty(), "CHANGELOG.md:\n{}", p.join("\n"));
    for v in ["0.2.0", "0.1.4", "0.1.3", "0.1.2", "0.1.1", "0.1.0"] {
        if let Err(err) = section(&data, v) {
            panic!("{err}");
        }
    }
}

/// The site's changelog page and every version pin in the docs follow the
/// published release in website/released-version, so a hand edit to the
/// intro, a published section or a pinned page fails here until
/// `task docs:changelog` has caught the docs up. Dating a new section does
/// not: it is above the published release until release.yml publishes it.
#[test]
fn the_docs_name_the_current_release() {
    let found = stale(&from_disk(&repo_root()), &PINS, &repo_changelog()).unwrap();
    assert!(
        found.is_empty(),
        "{} are stale; run task docs:changelog",
        found.join(", ")
    );
}

#[test]
fn latest_and_the_site_page() {
    assert_eq!(latest(SAMPLE).unwrap(), "0.2.0");
    let ranged = SAMPLE.replacen(
        "## [0.2.0] - 2026-10-27\n\n### Added\n\n- A thing.\n\n",
        "",
        1,
    );
    assert_eq!(
        latest(&ranged).unwrap(),
        "0.1.3",
        "a range's latest is its upper end"
    );
    let page = site_page(SAMPLE, "0.2.0");
    assert!(
        page.starts_with("---\ntitle: Changelog\n") && !page.contains("# Changelog"),
        "front matter in place of the H1:\n{page}"
    );
    assert!(
        !page.contains("[Unreleased]")
            && page.contains("## [0.2.0] - 2026-10-27")
            && page.contains("[0.2.0]: "),
        "Unreleased goes, the releases stay:\n{page}"
    );
    // A line under Unreleased, which every user-facing pull request adds,
    // leaves the page as it was; so does a heading with trailing spaces.
    for variant in [
        SAMPLE.replacen(
            "## [Unreleased]\n",
            "## [Unreleased]\n\n### Added\n\n- Coming.\n",
            1,
        ),
        SAMPLE.replacen("## [Unreleased]\n", "## [Unreleased]  \n", 1),
        // A line that opens with a link is not a link reference.
        SAMPLE.replacen(
            "## [Unreleased]\n",
            "## [Unreleased]\n\n[apic mock](cli.md#apic-mock) answers HEAD.\n\n- More.\n",
            1,
        ),
    ] {
        let p = site_page(&variant, "0.2.0");
        assert!(p == page, "Unreleased changed the page:\n{p}");
    }
    // Latest is any release, the docs pin the newest stable one, and a
    // heading that is not a version is an error, not a panic.
    let rc = SAMPLE.replacen(
        "## [0.2.0] - 2026-10-27",
        "## [0.3.0-rc.1] - 2026-10-28\n\n- Candidate.\n\n## [0.2.0] - 2026-10-27",
        1,
    ) + "[0.3.0-rc.1]: u\n";
    assert_eq!(latest(&rc).unwrap(), "0.3.0-rc.1");
    assert_eq!(stable(&rc, "").unwrap(), "0.2.0");
    assert!(
        err_text(latest("## [1] - 2026-10-27\n")).contains("not a version"),
        "a bad version"
    );
}

/// A pin rewrites the version wherever its phrase appears, and a phrase
/// that is gone fails rather than leaving the old version in place.
#[test]
fn pins_rewrite_and_refuse_a_missing_phrase() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let pins = vec![
        pin("a.md", "version: v{v}"),
        pin("a.md", "minor (`v{minor}`)"),
    ];
    // Everything but the placeholders is literal: a dot in a host is a dot.
    let host = pin("a.md", "ghcr.io/x:{v} ");
    assert!(
        host.re.is_match("ghcr.io/x:0.2.0 ") && !host.re.is_match("ghcrXio/x:0.2.0 "),
        "pin pattern {}",
        host.re
    );
    write_file(
        root,
        "a.md",
        "version: v0.1.4\nversion: v0.1.4 # x\nminor (`v0.1`)\n",
    );
    let disk = from_disk(&path_str(root));
    let (_, mut after) = pinned(&disk, &pins, "a.md", "0.2.0").unwrap();
    assert_eq!(
        after,
        "version: v0.2.0\nversion: v0.2.0 # x\nminor (`v0.2`)\n"
    );
    // A prerelease's suffix is part of the version the pin matches, so a
    // second run changes nothing rather than adding the suffix again.
    write_file(root, "a.md", &after);
    for run in 1..=2 {
        (_, after) = pinned(&disk, &pins, "a.md", "0.3.0-rc.1").unwrap();
        assert_eq!(
            after, "version: v0.3.0-rc.1\nversion: v0.3.0-rc.1 # x\nminor (`v0.3`)\n",
            "run {run}"
        );
        write_file(root, "a.md", &after);
    }
    write_file(root, "a.md", "reworded\n");
    assert!(
        err_text(pinned(&disk, &pins, "a.md", "0.2.0")).contains("no match for the pin"),
        "a missing phrase"
    );
}

/// release.yml's check: the tag must be the newest dated section, and the
/// docs must already name it.
#[test]
fn release_checks_the_tag_and_the_docs() {
    let data = repo_changelog();
    let disk = from_disk(&repo_root());
    let newest = latest(&data).unwrap();
    let notes = release(&disk, &PINS, &data, &newest).expect("the current release");
    assert!(!notes.is_empty());
    assert!(
        err_text(release(&disk, &PINS, &data, "0.1.0"))
            .contains(&format!("newest dated section is {newest}")),
        "an older tag"
    );
}

/// A link reference inside a section is part of its body, and a fenced
/// example may hold lines that look like headings: neither ends a section,
/// leaks Unreleased onto the page, or fails `check`. Only the block of
/// references that ends the file is the file's link references.
#[test]
fn references_and_fences_inside_sections() {
    let body = "### Added\n\n- Errors follow [RFC 9457][rfc].\n\n[rfc]: https://example.com/rfc9457\n\n```markdown\n## Notes\n[x]: y\n```\n\n- After the example.";
    let with_body = SAMPLE.replacen("### Added\n\n- A thing.", body, 1);
    assert!(check(&with_body).is_empty(), "well formed");
    assert_eq!(
        section(&with_body, "0.2.0").unwrap(),
        body,
        "the whole section, reference and example included"
    );
    let unrel = SAMPLE.replacen(
        "## [Unreleased]\n",
        "## [Unreleased]\n\n- Errors follow [RFC 9457][rfc].\n\n[rfc]: https://example.com\n\n- Still unreleased.\n",
        1,
    );
    let page = site_page(&unrel, "0.2.0");
    assert!(
        page == site_page(SAMPLE, "0.2.0"),
        "Unreleased leaked onto the page:\n{page}"
    );
}

/// A changelog that fails `check` has no newest release to trust: a
/// section dated below an older one would hide it and the tag job would
/// skip it.
#[test]
fn latest_refuses_a_misordered_changelog() {
    let misordered = SAMPLE.replacen(
        "## [0.1.1] to [0.1.3] - 2026-10-10\n\nPipeline only.\n\n",
        "",
        1,
    );
    let misordered = misordered.replacen(
        "## [0.1.0] - 2026-10-10\n\nThe first release.\n",
        "## [0.1.0] - 2026-10-10\n\nThe first release.\n\n## [0.3.0] - 2026-10-28\n\nNew.\n",
        1,
    ) + "[0.3.0]: u\n";
    assert!(
        err_text(latest(&misordered)).contains("newest first"),
        "a misordered changelog"
    );
}

/// Runs git in a scratch repository with an identity of its own.
fn test_git(root: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.com",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "tag.gpgsign=false",
        ])
        .args(args)
        .output()
        .expect("git");
    assert!(
        out.status.success(),
        "git {args:?}: {}\n{}{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// The commit to tag is the first on main, after the section was dated,
/// at which the release check passes: not the dating commit while the docs
/// still lag, and not a later push that only added a line under Unreleased.
#[test]
fn release_commit_is_where_the_release_was_cut() {
    if !have_git() {
        return; // git is not installed
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let root_str = path_str(root);
    let pins = vec![pin("docs.md", "version: v{v}")];
    let commit = |msg: &str| -> String {
        test_git(root, &["add", "-A"]);
        test_git(root, &["commit", "-q", "-m", msg]);
        test_git(root, &["rev-parse", "HEAD"])
    };
    let v1 = "# Changelog\n\n## [Unreleased]\n\n## [0.1.0] - 2026-10-01\n\nFirst.\n\n[Unreleased]: u\n[0.1.0]: u\n";
    let v2 = "# Changelog\n\n## [Unreleased]\n\n## [0.2.0] - 2026-10-02\n\nSecond.\n\n## [0.1.0] - 2026-10-01\n\nFirst.\n\n[Unreleased]: u\n[0.2.0]: u\n[0.1.0]: u\n";
    test_git(root, &["init", "-q"]);
    write_file(root, "CHANGELOG.md", v1);
    write_file(root, "docs.md", "version: v0.1.0\n");
    write_file(root, RELEASED, "0.1.0\n");
    write(&root_str, &pins, v1).unwrap();
    commit("0.1.0");
    // Dating the section needs nothing else, but this pull request also
    // broke a pin, which the next commit mends.
    write_file(root, "CHANGELOG.md", v2);
    write_file(root, "docs.md", "version: v0.0.9\n");
    let dated = commit("date 0.2.0, and break a pin");
    write(&root_str, &pins, v2).unwrap();
    let caught_up = commit("task docs:changelog");
    assert_eq!(
        read_file(root, "docs.md"),
        "version: v0.1.0\n",
        "before 0.2.0 is published the docs stay on 0.1.0"
    );
    // A later merge breaks the file and the next mends it: the walk skips
    // the broken commit rather than starting the release after it.
    write_file(
        root,
        "CHANGELOG.md",
        &v2.replacen("## [Unreleased]\n", "## [Unreleased]\n\n## Notes\n", 1),
    );
    commit("a malformed heading");
    write_file(root, "CHANGELOG.md", v2);
    commit("mend it");
    write_file(
        root,
        "CHANGELOG.md",
        &v2.replacen("## [Unreleased]\n", "## [Unreleased]\n\n- Later.\n", 1),
    );
    let later = commit("a line under Unreleased");

    let got = release_commit(&root_str, &pins, "CHANGELOG.md", &later, "0.2.0");
    assert_eq!(
        got.as_deref(),
        Ok(caught_up.as_str()),
        "dated {dated}, later {later}"
    );
    assert!(
        err_text(release_commit(
            &root_str,
            &pins,
            "CHANGELOG.md",
            &dated,
            "0.2.0"
        ))
        .contains("passes the release check"),
        "docs never caught up"
    );
    assert!(
        err_text(release_commit(
            &root_str,
            &pins,
            "CHANGELOG.md",
            &later,
            "0.3.0"
        ))
        .contains("not the newest dated section"),
        "an undated version"
    );
}

/// Reading through git tells a file a commit does not have from git
/// failing, and `newer_tags` finds the version tags above a release.
#[test]
fn git_reader_and_newer_tags() {
    if !have_git() {
        return; // git is not installed
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let root_str = path_str(root);
    test_git(root, &["init", "-q"]);
    write_file(root, "a.md", "a\n");
    write_file(root, "sub/a.md", "sub\n");
    test_git(root, &["add", "-A"]);
    test_git(root, &["commit", "-q", "-m", "a"]);
    let read = from_git(&root_str, "HEAD");
    assert_eq!(read("a.md").unwrap(), "a\n");
    // From a subdirectory, paths are relative to it, as on disk.
    let sub: PathBuf = root.join("sub");
    assert_eq!(from_git(&path_str(&sub), "HEAD")("a.md").unwrap(), "sub\n");
    assert!(
        matches!(read("missing.md"), Err(Error::NotExist(_))),
        "a missing file does not exist"
    );
    assert!(
        matches!(
            from_git(&root_str, "no-such-commit")("a.md"),
            Err(Error::Git(_))
        ),
        "a bad commit is a git error"
    );
    assert!(
        matches!(
            release_commit(&root_str, &PINS, "CHANGELOG.md", "no-such-commit", "0.1.0"),
            Err(Error::Git(_))
        ),
        "release_commit passes a git failure on"
    );
    for tag in ["v0", "v0.1.0", "v0.2.0-rc.1", "v0.2.0", "vscode-v0.9.0"] {
        test_git(root, &["tag", tag]);
    }
    for (tag, want) in [
        ("0.1.0", "v0.2.0-rc.1 v0.2.0"),
        ("0.2.0-rc.1", "v0.2.0"),
        ("0.2.0", ""),
    ] {
        let newer = newer_tags(&root_str, tag).unwrap();
        assert_eq!(newer.join(" "), want, "above {tag}");
    }
}

/// A fence closes only on a bare fence line of the same kind, at least as
/// long: an inner "```http" in a ```` example is text, and so is the "## "
/// line after it.
#[test]
fn nested_fences() {
    let body = "- Example:\n\n````markdown\n```http\nGET /\n```\n## Usage\n````\n\n- After.";
    let with_body = SAMPLE.replacen("### Added\n\n- A thing.", body, 1);
    assert!(check(&with_body).is_empty(), "well formed");
    assert_eq!(section(&with_body, "0.2.0").unwrap(), body);
}

/// The site follows the published release: a dated section above it is not
/// on the page and the pins stay put, until `publish` records the release
/// and regenerates; a prerelease moves the page but not the pins, and an
/// older release than the one recorded is refused.
#[test]
fn publish_moves_the_docs() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let root_str = path_str(root);
    let pins = vec![pin("docs.md", "version: v{v}")];
    let changelog = "# Changelog\n\n## [Unreleased]\n\n## [0.3.0-rc.1] - 2026-10-03\n\nCandidate.\n\n## [0.2.0] - 2026-10-02\n\nSecond.\n\n## [0.1.0] - 2026-10-01\n\nFirst.\n\n[Unreleased]: u\n[0.3.0-rc.1]: u\n[0.2.0]: u\n[0.1.0]: u\n";
    write_file(root, "docs.md", "version: v0.1.0\n");
    write_file(root, RELEASED, "0.1.0\n");
    write(&root_str, &pins, changelog).unwrap();
    let page = read_file(root, PAGE);
    assert!(
        !page.contains("0.2.0") && !page.contains("rc.1") && page.contains("## [0.1.0]"),
        "only what is published is on the page:\n{page}"
    );
    publish(&root_str, &pins, changelog, "0.2.0").unwrap();
    assert!(
        read_file(root, RELEASED) == "0.2.0\n"
            && read_file(root, "docs.md") == "version: v0.2.0\n"
            && read_file(root, PAGE).contains("## [0.2.0] - 2026-10-02"),
        "after publishing 0.2.0: {:?} {:?}\n{}",
        read_file(root, RELEASED),
        read_file(root, "docs.md"),
        read_file(root, PAGE)
    );
    publish(&root_str, &pins, changelog, "0.3.0-rc.1").unwrap();
    assert!(
        read_file(root, "docs.md") == "version: v0.2.0\n"
            && read_file(root, PAGE).contains("## [0.3.0-rc.1]"),
        "a prerelease is on the page but not in the pins: {:?}",
        read_file(root, "docs.md")
    );
    assert_eq!(
        stale(&from_disk(&root_str), &pins, changelog),
        Ok(Vec::new()),
        "after publishing"
    );
    assert!(
        err_text(publish(&root_str, &pins, changelog, "0.2.0")).contains("is older"),
        "an older release"
    );
    assert!(
        publish(&root_str, &pins, changelog, "0.4.0").is_err(),
        "a version with no section should be refused"
    );
    write_file(root, RELEASED, "0.9.0\n");
    assert!(
        err_text(stale(&from_disk(&root_str), &pins, changelog)).contains(RELEASED),
        "a recorded release with no section"
    );
}

/// A section's notes carry the link definitions they use, whether written
/// at the end of the section or in the file's closing block, so they read
/// the same on GitHub as in the file.
#[test]
fn section_carries_the_references_it_uses() {
    let changelog = SAMPLE.replacen(
        "The first release.\n",
        "The first release: see [docs].\n\n[docs]: https://x\n",
        1,
    );
    let changelog = changelog.replacen("- A thing.", "- A thing, as [the spec][spec] says.", 1)
        + "[spec]: https://spec\n";
    assert!(check(&changelog).is_empty(), "well formed");
    assert_eq!(
        section(&changelog, "0.1.0").unwrap(),
        "The first release: see [docs].\n\n[docs]: https://x"
    );
    let got = section(&changelog, "0.2.0").unwrap();
    assert!(
        got.ends_with("says.\n\n[spec]: https://spec"),
        "0.2.0: {got:?}"
    );
    let got = section(&changelog, "0.1.1").unwrap();
    assert!(
        !got.contains("[docs]: "),
        "a section that does not use a reference does not carry it: {got:?}"
    );
}

/// `publish` writes nothing unless the whole regeneration succeeds.
#[test]
fn publish_is_all_or_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let root_str = path_str(root);
    let pins = vec![pin("docs.md", "version: v{v}")];
    let changelog = "# Changelog\n\n## [Unreleased]\n\n## [0.2.0] - 2026-10-02\n\nSecond.\n\n## [0.1.0] - 2026-10-01\n\nFirst.\n\n[Unreleased]: u\n[0.2.0]: u\n[0.1.0]: u\n";
    write_file(root, "docs.md", "version: v0.1.0\n");
    write_file(root, RELEASED, "0.1.0\n");
    write(&root_str, &pins, changelog).unwrap();
    // The pin's phrase is gone, so regenerating fails: nothing moves.
    write_file(root, "docs.md", "reworded\n");
    assert!(
        err_text(publish(&root_str, &pins, changelog, "0.2.0")).contains("no match for the pin"),
        "publish with a missing pin"
    );
    assert_eq!(
        read_file(root, RELEASED),
        "0.1.0\n",
        "the record moved anyway"
    );
}

/// A hard-wrapped paragraph or bullet is refused: GitHub shows a release
/// body's single newlines as breaks, so each is one line in the file.
/// Items, headings, tables, references, quotes and fenced code may follow
/// a line directly.
#[test]
fn check_refuses_wrapped_lines() {
    for (body, want) in [
        ("- A bullet that\n  wraps.", true),
        ("A paragraph that\nwraps.", true),
        ("- one\n- two\n  - nested\n1. three", false),
        ("### Added\nText right under a heading.", false),
        ("| a | b |\n|---|---|\n| 1 | 2 |", false),
        ("Text, then a quote:\n> quoted", false),
        ("- see [docs]\n[docs]: https://x", false),
        ("Example:\n```sh\napic run login\napic run x\n```", false),
    ] {
        let changelog = SAMPLE.replacen("- A thing.", body, 1);
        let got = check(&changelog)
            .join("\n")
            .contains("continues the line above");
        assert_eq!(got, want, "{body:?}");
    }
}
