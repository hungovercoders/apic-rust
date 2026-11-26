//! Generates the "Commands and flags" section at the end of the site's
//! `cli.md` and the man pages, from apic's own command tree, so the
//! reference cannot drift from what the binary accepts. The hand-written
//! parts of `cli.md` (targets, exit codes, JSON shapes, examples) stay as
//! they are; only the text between the markers is replaced.
//!
//! ```text
//! cargo run -q -p clidocs                  rewrite cli.md (task docs:cli)
//! cargo run -q -p clidocs -- -check        exit 1 when cli.md is stale
//! cargo run -q -p clidocs -- -man man      write man/apic.1, man/apic-run.1, …
//! ```
//!
//! It lives under `scripts/` rather than in the binary: nothing at run
//! time needs it, and the `reference_is_current` test fails when the
//! committed page is stale, so CI catches a flag added without
//! regenerating.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;
use std::process::exit;

use apic_cli::{App, CommandSpec, FlagSpec};
use apic_gocompat::{path_error, quote, FlagSet};

/// The CLI reference on the site, relative to the repository root.
const PAGE: &str = "website/src/content/docs/cli.md";
/// The markers around the page's generated section.
const BEGIN: &str = "<!-- BEGIN GENERATED: cargo run -q -p clidocs (task docs:cli) rewrites this section; edit the flags in crates/cli instead -->";
const END: &str = "<!-- END GENERATED -->";

fn main() {
    let mut flags = FlagSet::new("clidocs");
    flags.bool(
        "check",
        false,
        "exit 1 if the page is stale instead of rewriting it",
    );
    flags.string("md", PAGE, "the page holding the generated section");
    flags.string(
        "man",
        "",
        "write man pages into this directory instead of updating the page",
    );
    flags.parse_env();
    let root = tree();
    let man_dir = flags.get("man");
    if !man_dir.is_empty() {
        // Only the man pages: the release build, which runs this, must not
        // touch the docs.
        let dir = Path::new(man_dir);
        if let Err(err) = std::fs::create_dir_all(dir) {
            fatal(&path_error("mkdir", dir, &err));
        }
        for (name, text) in man(root) {
            let path = dir.join(name);
            if let Err(err) = std::fs::write(&path, text) {
                fatal(&path_error("open", &path, &err));
            }
        }
        return;
    }
    let page = flags.get("md");
    let current = match std::fs::read(page) {
        Ok(data) => String::from_utf8_lossy(&data).into_owned(),
        Err(err) => fatal(&path_error("open", Path::new(page), &err)),
    };
    let updated = match update(&current, &markdown(root)) {
        Ok(updated) => updated,
        Err(err) => fatal(&format!("{page}: {err}")),
    };
    if flags.get_bool("check") {
        if updated != current.replace("\r\n", "\n") {
            fatal(&format!("{page} is stale; run `task docs:cli`"));
        }
        return;
    }
    if let Err(err) = std::fs::write(page, updated) {
        fatal(&path_error("open", Path::new(page), &err));
    }
}

fn fatal(msg: &str) -> ! {
    eprintln!("{msg}");
    exit(1);
}

/// apic's command tree as the binary builds it, the completion command
/// included.
fn tree() -> &'static CommandSpec {
    App::spec()
}

/// A command with the commands above it, from the root down: what its
/// usage line, its full name and its inherited flags are worked out from.
type Chain<'a> = Vec<&'a CommandSpec>;

fn leaf<'a>(chain: &[&'a CommandSpec]) -> &'a CommandSpec {
    chain[chain.len() - 1]
}

/// The subcommands a reference lists: not the hidden ones, nor `help`.
fn visible(c: &CommandSpec) -> Vec<&CommandSpec> {
    let mut subs: Vec<&CommandSpec> = c
        .subcommands
        .iter()
        .filter(|sub| !sub.hidden && sub.name != "help")
        .collect();
    subs.sort_by_key(|sub| sub.name);
    subs
}

/// Every visible command under root, depth first, by name.
fn commands(root: &CommandSpec) -> Vec<Chain<'_>> {
    fn walk<'a>(chain: &Chain<'a>, out: &mut Vec<Chain<'a>>) {
        for sub in visible(leaf(chain)) {
            let mut next = chain.clone();
            next.push(sub);
            out.push(next.clone());
            walk(&next, out);
        }
    }
    let mut out = Vec::new();
    walk(&vec![root], &mut out);
    out
}

/// Replaces the generated section of page, between the markers, or appends
/// it when the page has none yet.
fn update(page: &str, generated: &str) -> Result<String, String> {
    let page = page.replace("\r\n", "\n");
    match (page.find(BEGIN), page.find(END)) {
        (None, None) => Ok(format!("{}\n\n{generated}", page.trim_end_matches('\n'))),
        (Some(b), Some(e)) if e >= b => Ok(format!(
            "{}{generated}{}",
            &page[..b],
            page[e + END.len()..].trim_start_matches('\n')
        )),
        _ => Err("the generated-section markers are out of order or one is missing".to_string()),
    }
}

/// Renders the section: the global flags once, then each command with its
/// synopsis, usage and own flags.
fn markdown(root: &CommandSpec) -> String {
    let mut b = String::new();
    b.push_str(BEGIN);
    b.push_str("\n\n");
    b.push_str("## Commands and flags\n\n");
    b.push_str("Generated from apic's own command tree, so it lists exactly what the\n");
    b.push_str("binary accepts. The sections above explain each command; this is every\n");
    b.push_str("flag in one place. `man apic` and `man apic-run` show the same, where\n");
    b.push_str("the release archive's `man/` pages are installed.\n\n");
    b.push_str("### Global flags\n\nThese work with every command.\n\n");
    flag_table(&mut b, &root.persistent_flags());
    for chain in commands(root) {
        let c = leaf(&chain);
        let _ = write!(
            b,
            "\n### {}\n\n{}\n\n",
            apic_cli::command_path(&chain),
            sentence(c.short)
        );
        let _ = write!(b, "```\n{}\n```\n\n", apic_cli::use_line(&chain));
        if c.flags.iter().any(|f| !f.hidden) {
            flag_table(&mut b, &c.local_flags());
        } else {
            b.push_str("No flags of its own.\n");
        }
    }
    b.push('\n');
    b.push_str(END);
    b.push('\n');
    b
}

fn flag_table(b: &mut String, flags: &[&FlagSpec]) {
    b.push_str("| Flag | Meaning |\n|---|---|\n");
    for f in flags {
        if f.hidden || f.name == "help" {
            continue;
        }
        let _ = writeln!(b, "| `{}` | {} |", flag_name(f), cell(&flag_usage(f)));
    }
}

/// Splits a flag's usage into the name of its value and the text. The
/// first back-quoted word of the usage is the name, and loses its quotes
/// in the text; without one the name is the flag's type, and a switch has
/// none.
fn unquote_usage(f: &FlagSpec) -> (String, String) {
    if let Some(start) = f.usage.find('`') {
        if let Some(len) = f.usage[start + 1..].find('`') {
            let end = start + 1 + len;
            let name = &f.usage[start + 1..end];
            let usage = format!("{}{name}{}", &f.usage[..start], &f.usage[end + 1..]);
            return (name.to_string(), usage);
        }
    }
    (f.kind.type_name().to_string(), f.usage.clone())
}

/// `-C, --dir <string>`.
fn flag_name(f: &FlagSpec) -> String {
    let (name, _) = unquote_usage(f);
    let mut s = format!("--{}", f.name);
    if let Some(short) = f.short {
        s = format!("-{short}, {s}");
    }
    if !name.is_empty() {
        let _ = write!(s, " <{}>", type_word(&name));
    }
    s
}

fn type_word(t: &str) -> &str {
    match t {
        "stringArray" | "stringSlice" | "strings" => "string",
        _ => t,
    }
}

/// The usage text with the default, when it is not the zero value.
fn flag_usage(f: &FlagSpec) -> String {
    let (_, mut usage) = unquote_usage(f);
    match f.default.as_str() {
        "" | "false" | "0" | "[]" | "0s" => {}
        default => {
            let _ = write!(usage, " (default `{default}`)");
        }
    }
    usage
}

/// Makes usage text safe in a table cell: a `|` would end the cell, and
/// `<attempts>` would be read as an HTML tag and vanish from the page.
fn cell(s: &str) -> String {
    let mut b = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '|' => b.push_str("\\|"),
            '\n' => b.push(' '),
            '&' => b.push_str("&amp;"),
            '<' => b.push_str("&lt;"),
            '>' => b.push_str("&gt;"),
            c => b.push(c),
        }
    }
    b
}

fn sentence(s: &str) -> String {
    let s = s.trim();
    if s.is_empty() || s.ends_with('.') {
        return s.to_string();
    }
    format!("{s}.")
}

/// Renders one man page per command, named the way man(1) finds them:
/// `apic.1`, `apic-run.1`, `apic-session-clear.1`.
fn man(root: &CommandSpec) -> BTreeMap<String, String> {
    let mut pages = BTreeMap::new();
    pages.insert("apic.1".to_string(), man_page(&[root]));
    for chain in commands(root) {
        pages.insert(format!("{}.1", man_name(&chain)), man_page(&chain));
    }
    pages
}

fn man_name(chain: &[&CommandSpec]) -> String {
    apic_cli::command_path(chain).replace(' ', "-")
}

fn man_page(chain: &[&CommandSpec]) -> String {
    let c = leaf(chain);
    let mut b = String::new();
    let name = man_name(chain);
    let _ = writeln!(
        b,
        ".TH {} 1 \"\" \"apic\" \"apic manual\"",
        quote(&name.to_uppercase())
    );
    let _ = writeln!(b, ".SH NAME\n{} \\- {}", roff(&name), roff(c.short));
    let _ = writeln!(b, ".SH SYNOPSIS\n.B {}", roff(&apic_cli::use_line(chain)));
    let desc = if c.long.is_empty() {
        c.short
    } else {
        c.long.as_str()
    };
    b.push_str(".SH DESCRIPTION\n");
    for para in desc.trim().split("\n\n") {
        let _ = writeln!(b, ".PP\n{}", roff_lines(para));
    }
    man_flags(&mut b, "OPTIONS", &c.local_flags());
    if chain.len() == 1 {
        man_flags(&mut b, "GLOBAL OPTIONS", &c.persistent_flags());
    } else {
        man_flags(
            &mut b,
            "OPTIONS INHERITED FROM PARENT COMMANDS",
            &apic_cli::inherited_flags(chain),
        );
    }
    let example = c.example.trim();
    if !example.is_empty() {
        let _ = writeln!(b, ".SH EXAMPLES\n.PP\n.nf\n{}\n.fi", roff_lines(example));
    }
    let mut see = Vec::new();
    if chain.len() > 1 {
        see.push(format!("{}(1)", man_name(&chain[..chain.len() - 1])));
    }
    for sub in visible(c) {
        let mut below = chain.to_vec();
        below.push(sub);
        see.push(format!("{}(1)", man_name(&below)));
    }
    see.sort();
    if !see.is_empty() {
        let _ = writeln!(b, ".SH SEE ALSO\n{}", roff(&see.join(", ")));
    }
    b.push_str(".PP\nhttps://apic.sh/cli/\n");
    b
}

fn man_flags(b: &mut String, title: &str, flags: &[&FlagSpec]) {
    let rows: Vec<String> = flags
        .iter()
        .filter(|f| !f.hidden && f.name != "help")
        .map(|f| {
            format!(
                ".TP\n.B {}\n{}\n",
                roff(&flag_name(f)),
                roff_lines(&flag_usage(f).replace('`', ""))
            )
        })
        .collect();
    if rows.is_empty() {
        return;
    }
    let _ = write!(b, ".SH {title}\n{}", rows.concat());
}

/// Escapes text for a man page: backslashes and hyphens.
fn roff(s: &str) -> String {
    s.replace('\\', "\\e").replace('-', "\\-")
}

/// Escapes a block and keeps a line that starts with a dot or a quote from
/// being read as a request.
fn roff_lines(s: &str) -> String {
    roff(s)
        .split('\n')
        .map(|l| {
            if l.starts_with('.') || l.starts_with('\'') {
                format!("\\&{l}")
            } else {
                l.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use regex::Regex;

    use super::*;

    fn repo(rel: &str) -> String {
        format!("{}/../../{rel}", env!("CARGO_MANIFEST_DIR"))
    }

    /// Fails when a flag or command changed without `task docs:cli`, which
    /// is how CI catches a stale `cli.md`.
    #[test]
    fn reference_is_current() {
        let current = std::fs::read_to_string(repo(PAGE)).expect("the CLI reference");
        let want = update(&current, &markdown(tree())).expect("the markers");
        // A checkout with autocrlf (the Windows runner) is not staleness.
        assert!(
            current.replace("\r\n", "\n") == want,
            "docs/cli.md is stale; run `task docs:cli`"
        );
        assert!(
            want.contains("\n## apic run\n"),
            "the hand-written sections must survive"
        );
    }

    /// Every flag `run` registers is in its section, and so on for every
    /// command.
    #[test]
    fn every_flag_is_listed() {
        let root = tree();
        let md = markdown(root);
        for chain in commands(root) {
            let path = apic_cli::command_path(&chain);
            let Some(start) = md.find(&format!("\n### {path}\n")) else {
                panic!("no section for {path}");
            };
            let mut section = &md[start + 1..];
            if let Some(next) = section[4..].find("\n### ") {
                section = &section[..next + 4];
            }
            for f in leaf(&chain).local_flags() {
                assert!(
                    f.name == "help"
                        || section.contains(&format!("--{}`", f.name))
                        || section.contains(&format!("--{} <", f.name)),
                    "{path}: --{} missing",
                    f.name
                );
            }
        }
        for name in [
            "--output <string>",
            "--report <string>",
            "--retry <string>",
            "--keep-going",
            "-e, --env <string>",
        ] {
            assert!(md.contains(&format!("`{name}`")), "reference lacks {name}");
        }
    }

    /// The first back-quoted word of a usage string is read as the value's
    /// name, so `--output >>! file` once appeared in help. Every value
    /// name must be a word.
    #[test]
    fn value_names_are_words() {
        let word = Regex::new(r"^[a-zA-Z][a-zA-Z0-9_-]*$").unwrap();
        let root = tree();
        let mut all = commands(root);
        all.push(vec![root]);
        for chain in all {
            for f in apic_cli::effective_flags(&chain) {
                let (name, _) = unquote_usage(f);
                assert!(
                    name.is_empty() || word.is_match(&name),
                    "{} --{}: value name {name:?} comes from backticks in its usage",
                    apic_cli::command_path(&chain),
                    f.name
                );
            }
        }
    }

    #[test]
    fn man_pages() {
        let root = tree();
        let pages = man(root);
        for chain in commands(root) {
            assert!(
                pages.contains_key(&format!("{}.1", man_name(&chain))),
                "no page for {}",
                apic_cli::command_path(&chain)
            );
        }
        let run = &pages["apic-run.1"];
        for want in [
            ".TH \"APIC-RUN\" 1",
            ".SH NAME\napic\\-run \\- ",
            "\\-\\-output <string>",
            ".SH EXAMPLES",
            "apic(1)",
            ".SH OPTIONS INHERITED FROM PARENT COMMANDS",
        ] {
            assert!(run.contains(want), "apic-run.1 lacks {want:?}:\n{run}");
        }
        let top = &pages["apic.1"];
        assert!(
            top.contains(".SH GLOBAL OPTIONS") && top.contains("apic\\-run(1)"),
            "apic.1:\n{top}"
        );
        for (name, text) in &pages {
            for (i, l) in text.split('\n').enumerate() {
                assert!(
                    !l.starts_with('\''),
                    "{name}:{} starts with a quote, which roff reads as a request",
                    i + 1
                );
            }
        }
    }

    /// The Homebrew cask names each man page it installs, so a new command
    /// needs a line under `manpages:` in `.goreleaser.yaml` or
    /// `brew install` fails on a file the archive does not have.
    #[test]
    fn cask_lists_every_man_page() {
        let cfg = std::fs::read_to_string(repo(".goreleaser.yaml")).expect(".goreleaser.yaml");
        let listed: Vec<String> = Regex::new(r"(?m)^      - man/(\S+)$")
            .unwrap()
            .captures_iter(&cfg)
            .map(|m| m[1].to_string())
            .collect();
        let pages = man(tree());
        for name in pages.keys() {
            assert!(
                listed.contains(name),
                ".goreleaser.yaml homebrew_casks.manpages lacks man/{name}"
            );
        }
        for name in &listed {
            assert!(
                pages.contains_key(name),
                ".goreleaser.yaml homebrew_casks.manpages lists man/{name}, which no command produces"
            );
        }
    }

    #[test]
    fn update_replaces_or_appends() {
        let gen = format!("{BEGIN}\nnew\n{END}\n");
        assert_eq!(
            update("# Page\n\ntext\n", &gen),
            Ok(format!("# Page\n\ntext\n\n{gen}")),
            "append"
        );
        assert_eq!(
            update(&format!("# Page\n\n{BEGIN}\nold\n{END}\n"), &gen),
            Ok(format!("# Page\n\n{gen}")),
            "replace"
        );
        assert!(
            update(&format!("# Page\n{END}\n{BEGIN}\n"), &gen).is_err(),
            "markers out of order"
        );
    }

    /// Usage text reaches a Markdown table, where `<word>` is an HTML tag.
    #[test]
    fn cell_escapes_html() {
        assert_eq!(
            cell("send \"<attempts> [interval]\" | a & b\nnext"),
            r#"send "&lt;attempts&gt; [interval]" \| a &amp; b next"#
        );
        assert!(
            markdown(tree()).contains("&lt;attempts&gt;"),
            "run --retry's usage is not escaped in the reference"
        );
    }
}
