//! Writes `skills/apic/references/cheatsheet.md` from the site's
//! `cheatsheet.md`, the one page an agent needs when it writes a request.
//! The skill is copied into other projects, where the docs' relative links
//! mean nothing, so each becomes a link to the published site. A test fails
//! when the copy is stale; `task skill` regenerates it.

#![forbid(unsafe_code)]

use std::path::Path;
use std::sync::LazyLock;

use apic_gocompat::path_error;
use regex::Regex;

/// Where the docs are published. A page links to another as
/// `format.md#anchor`; the site serves it at `format/#anchor`
/// (`website/plugins/remark-doc-links.mjs` does the same rewrite at build).
const SITE: &str = "https://apic.sh/";

/// The page read and the file written, relative to the repository root.
const SOURCE: &str = "website/src/content/docs/cheatsheet.md";
const TARGET: &str = "skills/apic/references/cheatsheet.md";

static RE_DOC_LINK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\]\(([a-z0-9/-]+)\.md(#[^)]*)?\)").unwrap());
static RE_FRONT_MATTER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)\A---\n.*?\n---\n").unwrap());

/// Turns the cheat sheet into the skill's reference: a notice that it is
/// generated, the page's front matter replaced by a heading, and every
/// relative link to another page made absolute.
fn render(cheatsheet: &str) -> String {
    let body = RE_FRONT_MATTER.replace_all(cheatsheet, "");
    let body = RE_DOC_LINK.replace_all(&body, format!("]({SITE}${{1}}/${{2}})"));
    format!(
        "<!-- Generated from {SOURCE} by scripts/skilldocs (`task skill`); edit that page, not this file. -->\n# apic cheat sheet\n{}",
        body.trim_start_matches('\n')
    )
}

fn run(root: &Path) -> Result<(), String> {
    let source = root.join(SOURCE);
    let src = std::fs::read(&source).map_err(|err| path_error("open", &source, &err))?;
    let out = root.join(TARGET);
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir).map_err(|err| path_error("mkdir", dir, &err))?;
    }
    std::fs::write(&out, render(&String::from_utf8_lossy(&src)))
        .map_err(|err| path_error("open", &out, &err))
}

fn main() {
    if let Err(err) = run(Path::new(".")) {
        eprintln!("skilldocs: {err}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");

    fn read(rel: &str) -> String {
        let text = std::fs::read_to_string(Path::new(ROOT).join(rel))
            .unwrap_or_else(|err| panic!("{rel}: {err}"));
        // A checkout with autocrlf (the Windows runner) is not staleness.
        text.replace("\r\n", "\n")
    }

    /// Fails when the site's `cheatsheet.md` changed without `task skill`,
    /// which is how CI catches a stale copy in the skill.
    #[test]
    fn reference_is_current() {
        assert!(
            read(TARGET) == render(&read(SOURCE)),
            "{TARGET} is stale; run `task skill`"
        );
    }

    /// The page's front matter is for the site; the skill gets a heading.
    #[test]
    fn render_replaces_front_matter() {
        let got = render("---\ntitle: \"Cheat sheet\"\n---\n\nBody with a [link](format.md#x).\n");
        let want = format!(
            "<!-- Generated from {SOURCE} by scripts/skilldocs (`task skill`); edit that page, not this file. -->\n# apic cheat sheet\nBody with a [link]({SITE}format/#x).\n"
        );
        assert_eq!(got, want);
    }

    /// Every link in the reference must work from a project that only has
    /// the skill: absolute, or an anchor on the page itself.
    #[test]
    fn links_are_absolute() {
        let reference = read(TARGET);
        for m in Regex::new(r"\]\(([^)]+)\)")
            .unwrap()
            .captures_iter(&reference)
        {
            let to = &m[1];
            assert!(
                to.starts_with("https://") || to.starts_with('#'),
                "relative link {to:?} would not resolve inside the skill"
            );
        }
        assert!(
            reference.contains(&format!("]({SITE}format/#body-paths)")),
            "expected format.md#body-paths to become a site link"
        );
    }

    /// Every skill's front matter is what an agent sees before deciding to
    /// read the rest, and the body is read in full every time it does, so
    /// both are checked for each directory under `skills/`: a name that is
    /// the directory's and a description within the Agent Skills limits, a
    /// body short enough to be worth loading that points at its references,
    /// and no `apic <word>` that is not a command.
    #[test]
    fn skill() {
        let known: Vec<&str> = apic_cli::App::spec()
            .subcommands
            .iter()
            .map(|c| c.name)
            .collect();
        let mut names: Vec<String> = std::fs::read_dir(Path::new(ROOT).join("skills"))
            .expect("the skills directory")
            .flatten()
            .filter(|e| e.path().is_dir())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        let description = Regex::new(r"(?m)^description: (.+)$").unwrap();
        let reference = Regex::new(r"\]\((references/[^)#]+)").unwrap();
        let command = Regex::new(r"\bapic ([a-z]+)\b").unwrap();
        let mut failures = Vec::new();
        for name in &names {
            let mut fail = |msg: String| failures.push(format!("{name}: {msg}"));
            let skill = read(&format!("skills/{name}/SKILL.md"));
            let parts: Vec<&str> = skill.splitn(3, "---\n").collect();
            if parts.len() != 3 || !parts[0].is_empty() {
                fail("SKILL.md must start with a --- frontmatter block".to_string());
                continue;
            }
            let (front, body) = (parts[1], parts[2]);
            if !front.contains(&format!("name: {name}\n")) {
                fail(format!(
                    "frontmatter must name the skill {name}, its directory"
                ));
            }
            if !description
                .captures(front)
                .is_some_and(|m| m[1].len() <= 1024)
            {
                fail(
                    "frontmatter needs a one-line description of at most 1024 characters"
                        .to_string(),
                );
            }
            let lines = body.matches('\n').count();
            if lines > 200 {
                fail(format!(
                    "SKILL.md body is {lines} lines; keep it under 200 and put detail in references/"
                ));
            }
            if !body.contains("](references/") {
                fail("SKILL.md should point at its references/ for the detail".to_string());
            }
            // Every file the body links to must be in the skill, since it
            // is copied into other projects on its own.
            for m in reference.captures_iter(body) {
                let path = Path::new(ROOT).join("skills").join(name).join(&m[1]);
                if !path.exists() {
                    fail(format!(
                        "SKILL.md links {}, which is not in the skill",
                        &m[1]
                    ));
                }
            }
            // Every `apic <command>` the skill mentions must be a real
            // command, so a rename or removal fails here rather than in an
            // agent's shell.
            for m in command.captures_iter(body) {
                if !known.contains(&&m[1]) {
                    fail(format!(
                        "SKILL.md mentions `apic {}`, which is not a command",
                        &m[1]
                    ));
                }
            }
            // The binary carries the same directory.
            if !apic_skills::has(name) {
                fail("the directory is not in the pack the binary carries".to_string());
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
        assert!(
            names.iter().any(|n| n == "apic"),
            "skills/apic is the primary skill and must exist"
        );
        assert!(
            read("skills/apic/SKILL.md").contains("](references/cheatsheet.md)"),
            "skills/apic/SKILL.md should point at references/cheatsheet.md for the full format"
        );
    }
}
