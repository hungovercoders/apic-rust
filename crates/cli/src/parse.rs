//! Reading a command line: which command the arguments name, then that
//! command's flags and what is left over.

use std::collections::HashMap;
use std::time::Duration;

use clap::error::{ContextKind, ContextValue, ErrorKind};
use clap::{Arg, ArgAction, Command};

use crate::spec::{effective_flags, CommandSpec, FlagKind, FlagSpec};

/// Reports whether `token`, a long flag without its dashes, is a switch:
/// the value of any other flag is the argument after it.
fn long_is_switch(name: &str, flags: &[&FlagSpec]) -> bool {
    name == "help" || flags.iter().any(|f| f.name == name && !f.takes_value())
}

fn short_is_switch(c: &str, flags: &[&FlagSpec]) -> bool {
    c == "h"
        || flags
            .iter()
            .any(|f| f.short.is_some_and(|s| s.to_string() == c) && !f.takes_value())
}

/// Reports whether `s` is a flag whose value is the next argument. A flag
/// the command does not know counts: it is taken to have a value.
fn takes_next(s: &str, flags: &[&FlagSpec]) -> bool {
    if s.contains('=') {
        return false;
    }
    if let Some(name) = s.strip_prefix("--") {
        return !long_is_switch(name, flags);
    }
    s.starts_with('-') && s.len() == 2 && !short_is_switch(&s[1..], flags)
}

/// The arguments that are neither flags nor the values of flags, up to a
/// `--`.
pub(crate) fn strip_flags(args: &[String], flags: &[&FlagSpec]) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let s = &args[i];
        i += 1;
        if s == "--" {
            break;
        }
        if takes_next(s, flags) {
            // The value goes with the flag; a flag at the very end, or
            // with only its value after it, ends the search.
            if args.len() - i <= 1 {
                break;
            }
            i += 1;
            continue;
        }
        if !s.is_empty() && !s.starts_with('-') {
            out.push(s.clone());
        }
    }
    out
}

/// `args` without the first argument equal to `x` that is not a flag's
/// value.
fn args_minus_first(args: &[String], x: &str, flags: &[&FlagSpec]) -> Vec<String> {
    let mut pos = 0;
    while pos < args.len() {
        let s = &args[pos];
        if s == "--" {
            break;
        }
        if takes_next(s, flags) {
            pos += 2;
            continue;
        }
        if !s.starts_with('-') && s == x {
            let mut out = args[..pos].to_vec();
            out.extend_from_slice(&args[pos + 1..]);
            return out;
        }
        pos += 1;
    }
    args.to_vec()
}

/// Finds the command the arguments name: at each level the first
/// argument that is not a flag picks a subcommand, if it is one. Returns
/// the commands from the root down and the arguments without their names.
pub(crate) fn find<'a>(root: &'a CommandSpec, args: &[String]) -> (Vec<&'a CommandSpec>, Vec<String>) {
    let mut chain = vec![root];
    let mut rest = args.to_vec();
    loop {
        let flags = effective_flags(&chain);
        let stripped = strip_flags(&rest, &flags);
        let Some(next) = stripped.first() else {
            break;
        };
        let current = chain[chain.len() - 1];
        let Some(sub) = current.find(next) else {
            break;
        };
        rest = args_minus_first(&rest, next, &flags);
        chain.push(sub);
    }
    (chain, rest)
}

/// The commands close to what was typed, by name: within two edits, or
/// starting with it.
pub(crate) fn suggestions_for(cmd: &CommandSpec, typed: &str) -> Vec<String> {
    let typed = typed.to_lowercase();
    let mut names: Vec<&str> = cmd.subcommands.iter().filter(|c| !c.hidden).map(|c| c.name).collect();
    names.sort_unstable();
    names
        .into_iter()
        .filter(|name| {
            let name = name.to_lowercase();
            levenshtein(&typed, &name) <= 2 || name.starts_with(&typed)
        })
        .map(str::to_string)
        .collect()
}

fn levenshtein(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut cur = vec![i; b.len() + 1];
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        prev = cur;
    }
    prev[b.len()]
}

/// The flags of one invocation, read from the command line.
#[derive(Clone, Debug, Default)]
pub(crate) struct Flags {
    values: HashMap<&'static str, Vec<String>>,
    kinds: HashMap<&'static str, FlagKind>,
    defaults: HashMap<&'static str, String>,
}

impl Flags {
    fn last(&self, name: &str) -> &str {
        match self.values.get(name).and_then(|v| v.last()) {
            Some(v) => v,
            None => self.defaults.get(name).map_or("", String::as_str),
        }
    }

    /// Reports whether the flag was given on the command line.
    pub fn changed(&self, name: &str) -> bool {
        self.values.contains_key(name)
    }

    pub fn bool(&self, name: &str) -> bool {
        parse_bool(self.last(name)).unwrap_or(false)
    }

    pub fn string(&self, name: &str) -> String {
        self.last(name).to_string()
    }

    /// Every value of a repeatable flag, in the order given.
    pub fn strings(&self, name: &str) -> Vec<String> {
        self.values.get(name).cloned().unwrap_or_default()
    }

    pub fn int(&self, name: &str) -> i64 {
        parse_int(self.last(name)).unwrap_or(0)
    }

    /// A duration; one written negative counts as none.
    pub fn duration(&self, name: &str) -> Duration {
        parse_signed_duration(self.last(name)).unwrap_or(Duration::ZERO)
    }
}

fn parse_bool(s: &str) -> Option<bool> {
    match s {
        "1" | "t" | "T" | "TRUE" | "true" | "True" => Some(true),
        "0" | "f" | "F" | "FALSE" | "false" | "False" => Some(false),
        _ => None,
    }
}

/// A whole number in base ten, or with a `0x`, `0o`, `0b` or `0` prefix.
fn parse_int(s: &str) -> Option<i64> {
    let (neg, digits) = match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    let lower = digits.to_ascii_lowercase();
    let (radix, body) = if let Some(b) = lower.strip_prefix("0x") {
        (16, b.to_string())
    } else if let Some(b) = lower.strip_prefix("0o") {
        (8, b.to_string())
    } else if let Some(b) = lower.strip_prefix("0b") {
        (2, b.to_string())
    } else if lower.len() > 1 && lower.starts_with('0') {
        (8, lower[1..].to_string())
    } else {
        (10, lower)
    };
    if body.is_empty() || body.starts_with('_') || body.ends_with('_') {
        return None;
    }
    let body = if radix == 10 && !body.contains('_') {
        body
    } else {
        body.replace('_', "")
    };
    let n = i128::from_str_radix(&body, radix).ok()?;
    i64::try_from(if neg { -n } else { n }).ok()
}

fn parse_signed_duration(s: &str) -> Option<Duration> {
    match s.strip_prefix('-') {
        Some(rest) => apic_gocompat::parse_duration(rest).ok().map(|_| Duration::ZERO),
        None => apic_gocompat::parse_duration(s).ok(),
    }
}

/// What reading the flags of a command came to.
pub(crate) enum Parsed {
    /// The flags and the arguments left over.
    Ok(Flags, Vec<String>),
    /// `--help` or `-h` was among them.
    Help,
    /// A flag the command does not have, a missing or a bad value: the
    /// message, and whether `--json` had been read before it.
    Err(String, bool),
}

const ARGS: &str = "args";

/// The command a chain of commands ends at as a flat parser: its flags
/// and the persistent flags above it, and whatever else as arguments.
fn flat_command(flags: &[&FlagSpec]) -> Command {
    let mut cmd = Command::new("apic")
        .no_binary_name(true)
        .args_override_self(true)
        .disable_version_flag(true)
        .arg(Arg::new(ARGS).num_args(0..).action(ArgAction::Append));
    for f in flags {
        cmd = cmd.arg(clap_arg(f));
    }
    cmd
}

/// A flag as a clap argument. Every value is read as text and checked
/// afterwards, so the messages are apic's own.
pub(crate) fn clap_arg(f: &FlagSpec) -> Arg {
    let mut arg = Arg::new(f.name).long(f.name);
    if let Some(c) = f.short {
        arg = arg.short(c);
    }
    match f.kind {
        FlagKind::Bool => arg
            .action(ArgAction::Set)
            .num_args(0..=1)
            .require_equals(true)
            .default_missing_value("true")
            .value_name("true|false"),
        FlagKind::StringArray => arg
            .action(ArgAction::Append)
            .num_args(1)
            .allow_hyphen_values(true)
            .value_name(f.kind.type_name()),
        _ => arg
            .action(ArgAction::Set)
            .num_args(1)
            .allow_hyphen_values(true)
            .value_name(f.kind.type_name()),
    }
}

/// Reads the flags of the command `chain` ends at from `args`.
pub(crate) fn parse_flags(chain: &[&CommandSpec], args: &[String]) -> Parsed {
    let flags = effective_flags(chain);
    let matches = match flat_command(&flags).try_get_matches_from(args) {
        Ok(m) => m,
        Err(err) => return parse_error(&err, args, &flags),
    };
    let mut out = Flags::default();
    for f in &flags {
        out.kinds.insert(f.name, f.kind);
        out.defaults.insert(f.name, f.default.clone());
        if matches.value_source(f.name) != Some(clap::parser::ValueSource::CommandLine) {
            continue;
        }
        let values: Vec<String> = matches
            .get_many::<String>(f.name)
            .map(|v| v.cloned().collect())
            .unwrap_or_default();
        let bad = values.iter().find_map(|v| check_value(f, v).err().map(|why| (v, why)));
        if let Some((value, why)) = bad {
            let shown = match f.short {
                Some(c) => format!("-{c}, --{}", f.name),
                None => format!("--{}", f.name),
            };
            let msg = format!(
                "invalid argument {} for {} flag: {why}",
                apic_gocompat::quote(value),
                apic_gocompat::quote(&shown)
            );
            return Parsed::Err(msg, json_before(args, flag_position(args, f)));
        }
        out.values.insert(f.name, values);
    }
    let rest = matches
        .get_many::<String>(ARGS)
        .map(|v| v.cloned().collect())
        .unwrap_or_default();
    Parsed::Ok(out, rest)
}

fn check_value(f: &FlagSpec, v: &str) -> Result<(), &'static str> {
    let ok = match f.kind {
        FlagKind::Bool => parse_bool(v).is_some(),
        FlagKind::Int => parse_int(v).is_some(),
        FlagKind::Duration => parse_signed_duration(v).is_some(),
        FlagKind::String | FlagKind::StringArray => true,
    };
    if ok {
        return Ok(());
    }
    Err(match f.kind {
        FlagKind::Bool => "must be true or false",
        FlagKind::Int => "must be an integer",
        _ => "must be a duration like \"30s\" or \"5m\"",
    })
}

/// Where a flag first appears among the arguments.
fn flag_position(args: &[String], f: &FlagSpec) -> usize {
    let long = format!("--{}", f.name);
    let short = f.short.map(|c| format!("-{c}"));
    args.iter()
        .take_while(|a| *a != "--")
        .position(|a| {
            *a == long
                || a.starts_with(&format!("{long}="))
                || short.as_ref().is_some_and(|s| !a.starts_with("--") && a.starts_with(s.as_str()))
        })
        .unwrap_or(args.len())
}

/// Reports whether `--json` is switched on among the first `end`
/// arguments: the flags are read in order, so one after the mistake has
/// not been seen when the error is printed.
fn json_before(args: &[String], end: usize) -> bool {
    let mut json = false;
    for a in args.iter().take(end) {
        if a == "--" {
            break;
        }
        if a == "--json" {
            json = true;
        } else if let Some(v) = a.strip_prefix("--json=") {
            json = parse_bool(v).unwrap_or(json);
        }
    }
    json
}

/// Words a parse failure the way the reference does.
fn parse_error(err: &clap::Error, args: &[String], flags: &[&FlagSpec]) -> Parsed {
    match err.kind() {
        ErrorKind::DisplayHelp | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => Parsed::Help,
        ErrorKind::UnknownArgument => {
            let named = match err.get(ContextKind::InvalidArg) {
                Some(ContextValue::String(s)) => s.clone(),
                _ => String::new(),
            };
            if let Some(long) = named.strip_prefix("--") {
                let long = long.split('=').next().unwrap_or(long);
                let pos = args
                    .iter()
                    .position(|a| a.strip_prefix("--").is_some_and(|a| a.split('=').next() == Some(long)))
                    .unwrap_or(args.len());
                return Parsed::Err(format!("unknown flag: --{long}"), json_before(args, pos));
            }
            let c = named.trim_start_matches('-').chars().next().unwrap_or('?');
            // The message quotes the group from the unknown letter on.
            let found = args.iter().enumerate().find_map(|(i, a)| {
                if a.starts_with("--") || !a.starts_with('-') {
                    return None;
                }
                let group = &a[1..];
                let at = first_unknown_short(group, flags)?;
                (group[at..].starts_with(c)).then(|| (i, group[at..].to_string()))
            });
            match found {
                Some((pos, group)) => Parsed::Err(
                    format!("unknown shorthand flag: {} in -{group}", quote_char(c)),
                    json_before(args, pos),
                ),
                None => Parsed::Err(
                    format!("unknown shorthand flag: {} in -{c}", quote_char(c)),
                    json_before(args, args.len()),
                ),
            }
        }
        ErrorKind::InvalidValue | ErrorKind::TooFewValues | ErrorKind::WrongNumberOfValues => {
            // A flag at the end with no value after it.
            let last = args.last().map_or("", String::as_str);
            let json = json_before(args, args.len().saturating_sub(1));
            if last.starts_with("--") {
                return Parsed::Err(format!("flag needs an argument: {last}"), json);
            }
            let c = last.chars().last().unwrap_or('?');
            Parsed::Err(
                format!("flag needs an argument: {} in -{c}", quote_char(c)),
                json,
            )
        }
        _ => {
            let text = err.to_string();
            let first = text.lines().next().unwrap_or("").trim_start_matches("error: ");
            Parsed::Err(first.to_string(), json_before(args, args.len()))
        }
    }
}

/// The byte offset of the first letter of a group of short flags that is
/// not a flag of the command; `None` when a value-taking flag ends the
/// group first.
fn first_unknown_short(group: &str, flags: &[&FlagSpec]) -> Option<usize> {
    for (at, c) in group.char_indices() {
        if c == 'h' {
            continue;
        }
        match flags.iter().find(|f| f.short == Some(c)) {
            None => return Some(at),
            Some(f) if f.takes_value() => return None,
            Some(_) => {}
        }
    }
    None
}

fn quote_char(c: char) -> String {
    format!("'{c}'")
}
