//! The command tree as data: every command with its usage line, its
//! texts, its argument rule and its flags. The parser, the help pages, the
//! completion scripts and the documentation generators are all built from
//! this one description, so none of them can drift from the others.

use std::fmt;

use crate::root::{App, CliResult, Invocation};

/// The type of a flag's value, named as the reference pages name it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlagKind {
    /// A switch: `--json`, `--json=false`.
    Bool,
    /// One value; the last one given wins.
    String,
    /// A value that may be given several times, each kept.
    StringArray,
    /// A whole number.
    Int,
    /// A duration such as `500ms` or `1m30s`.
    Duration,
}

impl FlagKind {
    /// The word the reference shows after the flag: `string`,
    /// `stringArray`, `int`, `duration`, and nothing for a switch.
    pub fn type_name(self) -> &'static str {
        match self {
            FlagKind::Bool => "",
            FlagKind::String => "string",
            FlagKind::StringArray => "stringArray",
            FlagKind::Int => "int",
            FlagKind::Duration => "duration",
        }
    }
}

/// One flag of a command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FlagSpec {
    /// The long name, without the dashes.
    pub name: &'static str,
    /// The one-letter form, if any.
    pub short: Option<char>,
    pub kind: FlagKind,
    /// The default as text: `false`, an empty string, `[]`, `0s`, `8089`.
    pub default: String,
    /// What the flag is for, as the help and the reference print it.
    pub usage: String,
    /// A persistent flag is accepted by the command and everything under
    /// it; the root's are the global flags.
    pub persistent: bool,
    /// Left out of the help and the reference.
    pub hidden: bool,
}

impl FlagSpec {
    fn new(name: &'static str, short: Option<char>, kind: FlagKind, default: &str, usage: &str) -> FlagSpec {
        FlagSpec {
            name,
            short,
            kind,
            default: default.to_string(),
            usage: usage.to_string(),
            persistent: false,
            hidden: false,
        }
    }

    /// A switch, off unless given.
    pub fn bool(name: &'static str, usage: &str) -> FlagSpec {
        FlagSpec::new(name, None, FlagKind::Bool, "false", usage)
    }

    /// A flag taking one value.
    pub fn string(name: &'static str, default: &str, usage: &str) -> FlagSpec {
        FlagSpec::new(name, None, FlagKind::String, default, usage)
    }

    /// A repeatable flag.
    pub fn string_array(name: &'static str, usage: &str) -> FlagSpec {
        FlagSpec::new(name, None, FlagKind::StringArray, "[]", usage)
    }

    /// A flag taking a whole number.
    pub fn int(name: &'static str, default: i64, usage: &str) -> FlagSpec {
        FlagSpec::new(name, None, FlagKind::Int, &default.to_string(), usage)
    }

    /// A flag taking a duration; the default is none.
    pub fn duration(name: &'static str, usage: &str) -> FlagSpec {
        FlagSpec::new(name, None, FlagKind::Duration, "0s", usage)
    }

    /// Gives the flag its one-letter form.
    pub fn short(mut self, c: char) -> FlagSpec {
        self.short = Some(c);
        self
    }

    /// Makes the flag one every command below accepts too.
    pub fn persistent(mut self) -> FlagSpec {
        self.persistent = true;
        self
    }

    /// Reports whether the flag takes a value (everything but a switch).
    pub fn takes_value(&self) -> bool {
        self.kind != FlagKind::Bool
    }
}

/// How many arguments a command takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArgRule {
    /// Whatever is given; a command that reads none ignores them.
    Any,
    /// None at all.
    None,
    Exact(usize),
    Min(usize),
    Max(usize),
    Range(usize, usize),
}

impl ArgRule {
    /// Checks `args` against the rule; `path` is the command's full name,
    /// for the message.
    pub(crate) fn check(self, path: &str, args: &[String]) -> Result<(), String> {
        let n = args.len();
        match self {
            ArgRule::Any => Ok(()),
            ArgRule::None if n > 0 => Err(format!(
                "unknown command {} for {}",
                apic_gocompat::quote(&args[0]),
                apic_gocompat::quote(path)
            )),
            ArgRule::Exact(want) if n != want => Err(format!("accepts {want} arg(s), received {n}")),
            ArgRule::Min(want) if n < want => {
                Err(format!("requires at least {want} arg(s), only received {n}"))
            }
            ArgRule::Max(want) if n > want => Err(format!("accepts at most {want} arg(s), received {n}")),
            ArgRule::Range(min, max) if n < min || n > max => {
                Err(format!("accepts between {min} and {max} arg(s), received {n}"))
            }
            _ => Ok(()),
        }
    }
}

/// What runs a command once its flags and arguments have been read.
pub(crate) type Handler = fn(&mut App, &Invocation) -> CliResult;

/// One command of the tree.
#[derive(Clone)]
pub struct CommandSpec {
    /// The word that names it.
    pub name: &'static str,
    /// The name followed by its arguments, as written in the usage line:
    /// `run <request|file.http|file.http#name>...`.
    pub usage: &'static str,
    /// One line on what it does.
    pub short: &'static str,
    /// The long description; empty when the short one is all there is.
    pub long: String,
    /// The examples, one indented line each; empty for none.
    pub example: &'static str,
    pub args: ArgRule,
    /// The command's own flags, persistent ones included.
    pub flags: Vec<FlagSpec>,
    pub subcommands: Vec<CommandSpec>,
    /// Left out of the help and the reference.
    pub hidden: bool,
    /// Leaves `[flags]` off the usage line.
    pub disable_flags_in_use_line: bool,
    /// The words offered as arguments by shell completion.
    pub valid_args: Vec<String>,
    pub(crate) run: Option<Handler>,
}

impl fmt::Debug for CommandSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CommandSpec")
            .field("name", &self.name)
            .field("usage", &self.usage)
            .field("flags", &self.flags)
            .field("subcommands", &self.subcommands)
            .finish_non_exhaustive()
    }
}

impl CommandSpec {
    /// A command named by the first word of `usage`.
    pub(crate) fn new(usage: &'static str, short: &'static str) -> CommandSpec {
        CommandSpec {
            name: usage.split(' ').next().unwrap_or(usage),
            usage,
            short,
            long: String::new(),
            example: "",
            args: ArgRule::Any,
            flags: Vec::new(),
            subcommands: Vec::new(),
            hidden: false,
            disable_flags_in_use_line: false,
            valid_args: Vec::new(),
            run: None,
        }
    }

    pub(crate) fn long(mut self, long: impl Into<String>) -> CommandSpec {
        self.long = long.into();
        self
    }

    pub(crate) fn example(mut self, example: &'static str) -> CommandSpec {
        self.example = example;
        self
    }

    pub(crate) fn args(mut self, rule: ArgRule) -> CommandSpec {
        self.args = rule;
        self
    }

    pub(crate) fn flag(mut self, flag: FlagSpec) -> CommandSpec {
        self.flags.push(flag);
        self
    }

    pub(crate) fn subcommand(mut self, sub: CommandSpec) -> CommandSpec {
        self.subcommands.push(sub);
        self
    }

    pub(crate) fn runs(mut self, run: Handler) -> CommandSpec {
        self.run = Some(run);
        self
    }

    /// Reports whether the command does something itself, as against only
    /// holding subcommands.
    pub fn runnable(&self) -> bool {
        self.run.is_some()
    }

    /// The subcommand of that name.
    pub fn find(&self, name: &str) -> Option<&CommandSpec> {
        self.subcommands.iter().find(|c| c.name == name)
    }

    /// The command a path of names leads to, starting below this one.
    pub fn find_path<S: AsRef<str>>(&self, path: &[S]) -> Option<&CommandSpec> {
        path.iter().try_fold(self, |c, name| c.find(name.as_ref()))
    }

    /// The flags the command has that are not persistent: its own alone.
    pub fn local_flags(&self) -> Vec<&FlagSpec> {
        let mut out: Vec<&FlagSpec> = self.flags.iter().filter(|f| !f.persistent).collect();
        out.sort_by_key(|f| f.name);
        out
    }

    /// The command's persistent flags, by name.
    pub fn persistent_flags(&self) -> Vec<&FlagSpec> {
        let mut out: Vec<&FlagSpec> = self.flags.iter().filter(|f| f.persistent).collect();
        out.sort_by_key(|f| f.name);
        out
    }
}

/// The flags a command accepts: its own, then the persistent flags of the
/// commands above it, nearest first. A flag of the command hides one of
/// the same name further up, along with that one's short form.
pub fn effective_flags<'a>(chain: &[&'a CommandSpec]) -> Vec<&'a FlagSpec> {
    let mut out: Vec<&FlagSpec> = Vec::new();
    let Some((leaf, parents)) = chain.split_last() else {
        return out;
    };
    out.extend(leaf.flags.iter());
    for parent in parents.iter().rev() {
        for f in parent.flags.iter().filter(|f| f.persistent) {
            if !out.iter().any(|have| have.name == f.name) {
                out.push(f);
            }
        }
    }
    out
}

/// The persistent flags a command takes from the commands above it, by
/// name, without the ones its own flags hide.
pub fn inherited_flags<'a>(chain: &[&'a CommandSpec]) -> Vec<&'a FlagSpec> {
    let Some((leaf, _)) = chain.split_last() else {
        return Vec::new();
    };
    let mut out: Vec<&FlagSpec> = effective_flags(chain)
        .into_iter()
        .filter(|f| !leaf.flags.iter().any(|own| std::ptr::eq(own, *f)))
        .collect();
    out.sort_by_key(|f| f.name);
    out
}

/// The usage line of the command a chain of commands ends at: the names
/// of the commands above it, its own usage, and `[flags]` when it takes
/// any: `apic run <request|file.http|file.http#name>... [flags]`.
pub fn use_line(chain: &[&CommandSpec]) -> String {
    let Some((leaf, parents)) = chain.split_last() else {
        return String::new();
    };
    let mut line: Vec<&str> = parents.iter().map(|c| c.name).collect();
    line.push(leaf.usage);
    let mut line = line.join(" ");
    let has_flags = effective_flags(chain).iter().any(|f| !f.hidden);
    if has_flags && !leaf.disable_flags_in_use_line && !line.contains("[flags]") {
        line.push_str(" [flags]");
    }
    line
}

/// The full name of the command a chain ends at: `apic session clear`.
pub fn command_path(chain: &[&CommandSpec]) -> String {
    chain.iter().map(|c| c.name).collect::<Vec<_>>().join(" ")
}
