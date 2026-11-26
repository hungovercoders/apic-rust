//! Writes `skills/apic/references/cheatsheet.md` from the site's
//! `cheatsheet.md`, the one page an agent needs when it writes a request.
//! The skill is copied into other projects, where the docs' relative links
//! mean nothing, so each becomes a link to the published site. A test fails
//! when the copy is stale; `task skill` regenerates it.

#![forbid(unsafe_code)]

use std::path::Path;
use std::sync::LazyLock;

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
    let src = std::fs::read(&source).map_err(|err| apic_gocompat_path_error("open", &source, &err))?;
    let out = root.join(TARGET);
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir).map_err(|err| apic_gocompat_path_error("mkdir", dir, &err))?;
    }
    std::fs::write(&out, render(&String::from_utf8_lossy(&src)))
        .map_err(|err| apic_gocompat_path_error("open", &out, &err))
}

fn main() {
    if let Err(err) = run(Path::new(".")) {
        eprintln!("skilldocs: {err}");
        std::process::exit(1);
    }
}
