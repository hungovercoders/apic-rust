//! The help pages, laid out from the command tree, and `apic help`.

use crate::io::w;
use crate::root::{App, CliResult, Invocation};
use crate::spec::{command_path, inherited_flags, use_line, CommandSpec, FlagKind, FlagSpec};

pub(crate) fn spec() -> CommandSpec {
    CommandSpec::new("help [command]", "Help about any command")
        .long(
            "Help provides help for any command in the application.
Simply type apic help [path to command] for full details.",
        )
        .runs(run)
}

fn run(a: &mut App, inv: &Invocation) -> CliResult {
    let root = App::spec();
    let (chain, rest) = crate::parse::find(root, &inv.args);
    if chain.len() == 1 && !rest.is_empty() {
        let topics: Vec<String> = inv.args.iter().map(|t| format!("`{t}`")).collect();
        w!(
            a.stdout,
            "Unknown help topic [{}]\n{}",
            topics.join(" "),
            usage_with(&chain, false)
        );
        return Ok(());
    }
    a.stdout.print(&help(&chain));
    Ok(())
}

/// The help page of the command a chain ends at: its description, then
/// its usage.
pub(crate) fn help(chain: &[&CommandSpec]) -> String {
    let leaf = chain[chain.len() - 1];
    let about = if leaf.long.is_empty() {
        leaf.short
    } else {
        &leaf.long
    };
    let mut out = String::new();
    if !about.is_empty() {
        out.push_str(about.trim_end());
        out.push_str("\n\n");
    }
    out.push_str(&usage(chain));
    out
}

/// The usage of a command: its usage line, examples, subcommands and
/// flags.
pub(crate) fn usage(chain: &[&CommandSpec]) -> String {
    usage_with(chain, true)
}

/// The usage, with or without the line for the help flag itself.
fn usage_with(chain: &[&CommandSpec], with_help: bool) -> String {
    let leaf = chain[chain.len() - 1];
    let path = command_path(chain);
    let subs: Vec<&CommandSpec> = leaf.subcommands.iter().filter(|c| !c.hidden).collect();
    let mut out = String::from("Usage:");
    if leaf.runnable() {
        out.push_str(&format!("\n  {}", use_line(chain)));
    }
    if !subs.is_empty() {
        out.push_str(&format!("\n  {path} [command]"));
    }
    if !leaf.example.is_empty() {
        out.push_str(&format!("\n\nExamples:\n{}", leaf.example));
    }
    if !subs.is_empty() {
        out.push_str("\n\nAvailable Commands:");
        let pad = subs.iter().map(|c| c.name.len()).max().unwrap_or(0).max(11);
        let mut sorted = subs.clone();
        sorted.sort_by_key(|c| c.name);
        for c in sorted {
            out.push_str(&format!("\n  {:pad$} {}", c.name, c.short));
        }
    }
    let help_flag = FlagSpec::bool("help", &format!("help for {}", leaf.name)).short('h');
    let mut local: Vec<&FlagSpec> = leaf.flags.iter().filter(|f| !f.hidden).collect();
    if with_help {
        local.push(&help_flag);
    }
    local.sort_by_key(|f| f.name);
    out.push_str(&format!("\n\nFlags:\n{}", flag_usages(&local).trim_end()));
    let inherited: Vec<&FlagSpec> = inherited_flags(chain)
        .into_iter()
        .filter(|f| !f.hidden)
        .collect();
    if !inherited.is_empty() {
        out.push_str(&format!(
            "\n\nGlobal Flags:\n{}",
            flag_usages(&inherited).trim_end()
        ));
    }
    if !subs.is_empty() {
        out.push_str(&format!(
            "\n\nUse \"{path} [command] --help\" for more information about a command."
        ));
    }
    out.push('\n');
    out
}

/// One line per flag, the descriptions in a column.
fn flag_usages(flags: &[&FlagSpec]) -> String {
    let lines: Vec<(String, String)> = flags
        .iter()
        .map(|f| {
            let mut left = match f.short {
                Some(c) => format!("  -{c}, --{}", f.name),
                None => format!("      --{}", f.name),
            };
            match f.kind {
                FlagKind::Bool => left.push_str("[=true|false]"),
                kind => {
                    left.push(' ');
                    left.push_str(kind.type_name());
                }
            }
            let mut right = f.usage.clone();
            match f.kind {
                FlagKind::String if !f.default.is_empty() => {
                    right.push_str(&format!(" (default {})", apic_gocompat::quote(&f.default)));
                }
                FlagKind::Int if f.default != "0" => {
                    right.push_str(&format!(" (default {})", f.default))
                }
                _ => {}
            }
            (left, right)
        })
        .collect();
    let width = lines.iter().map(|(l, _)| l.len()).max().unwrap_or(0);
    lines
        .iter()
        .map(|(l, r)| format!("{l:width$}   {r}\n"))
        .collect()
}
