//! Command-line flags in the single-dash form the generator programs under
//! `scripts/` are called with: `-name value`, `-name=value`, `--name`, a
//! bare `-name` for a switch, and everything from the first argument that
//! is not a flag (or after `--`) left as positional arguments.

use std::time::Duration;

use crate::{format_duration, parse_duration, quote};

/// What a flag holds, which decides how its value is checked and how the
/// usage text names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Bool,
    Int,
    Str,
    Duration,
}

#[derive(Clone, Debug)]
struct Flag {
    name: String,
    kind: Kind,
    usage: String,
    default: String,
    value: String,
}

/// Why the arguments could not be parsed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FlagError {
    /// `-h` or `-help` was given and no flag has that name.
    Help,
    /// The message to show above the usage text.
    Invalid(String),
}

impl std::fmt::Display for FlagError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FlagError::Help => f.write_str("flag: help requested"),
            FlagError::Invalid(msg) => f.write_str(msg),
        }
    }
}

impl std::error::Error for FlagError {}

/// A set of flags: define them, parse the arguments, read the values.
#[derive(Clone, Debug, Default)]
pub struct FlagSet {
    name: String,
    flags: Vec<Flag>,
    args: Vec<String>,
}

impl FlagSet {
    /// A set for the program called `name`, which the usage text shows.
    pub fn new(name: &str) -> FlagSet {
        FlagSet {
            name: name.to_string(),
            flags: Vec::new(),
            args: Vec::new(),
        }
    }

    fn define(&mut self, name: &str, kind: Kind, default: String, usage: &str) {
        self.flags.push(Flag {
            name: name.to_string(),
            kind,
            usage: usage.to_string(),
            value: default.clone(),
            default,
        });
    }

    /// Defines a switch.
    pub fn bool(&mut self, name: &str, default: bool, usage: &str) {
        self.define(name, Kind::Bool, default.to_string(), usage);
    }

    /// Defines a flag that takes text.
    pub fn string(&mut self, name: &str, default: &str, usage: &str) {
        self.define(name, Kind::Str, default.to_string(), usage);
    }

    /// Defines a flag that takes a whole number.
    pub fn int(&mut self, name: &str, default: i64, usage: &str) {
        self.define(name, Kind::Int, default.to_string(), usage);
    }

    /// Defines a flag that takes a duration such as `15m` or `1.5s`.
    pub fn duration(&mut self, name: &str, default: Duration, usage: &str) {
        self.define(name, Kind::Duration, format_duration(default), usage);
    }

    fn flag(&self, name: &str) -> &Flag {
        self.flags
            .iter()
            .find(|f| f.name == name)
            .unwrap_or_else(|| panic!("flag -{name} is not defined"))
    }

    /// The value of a text flag (or any flag, as text).
    pub fn get(&self, name: &str) -> &str {
        &self.flag(name).value
    }

    /// The value of a switch.
    pub fn get_bool(&self, name: &str) -> bool {
        parse_bool(&self.flag(name).value).unwrap_or(false)
    }

    /// The value of a number flag.
    pub fn get_int(&self, name: &str) -> i64 {
        parse_int(&self.flag(name).value).unwrap_or(0)
    }

    /// The value of a duration flag.
    pub fn get_duration(&self, name: &str) -> Duration {
        parse_duration(&self.flag(name).value).unwrap_or_default()
    }

    /// The arguments left after the flags.
    pub fn args(&self) -> &[String] {
        &self.args
    }

    /// Parses the arguments after the program's name.
    pub fn parse(&mut self, args: &[String]) -> Result<(), FlagError> {
        let mut i = 0;
        while i < args.len() {
            let arg = &args[i];
            if arg.len() < 2 || !arg.starts_with('-') {
                break;
            }
            let mut body = &arg[1..];
            if let Some(rest) = body.strip_prefix('-') {
                if rest.is_empty() {
                    i += 1; // "--" ends the flags
                    break;
                }
                body = rest;
            }
            if body.starts_with('-') || body.starts_with('=') {
                return Err(FlagError::Invalid(format!("bad flag syntax: {arg}")));
            }
            i += 1;
            let (name, mut value) = match body.split_once('=') {
                Some((n, v)) => (n, Some(v.to_string())),
                None => (body, None),
            };
            let Some(pos) = self.flags.iter().position(|f| f.name == name) else {
                if name == "help" || name == "h" {
                    return Err(FlagError::Help);
                }
                return Err(FlagError::Invalid(format!(
                    "flag provided but not defined: -{name}"
                )));
            };
            let kind = self.flags[pos].kind;
            if kind == Kind::Bool {
                let text = value.unwrap_or_else(|| "true".to_string());
                let Some(b) = parse_bool(&text) else {
                    return Err(FlagError::Invalid(format!(
                        "invalid boolean value {} for -{name}: parse error",
                        quote(&text)
                    )));
                };
                self.flags[pos].value = b.to_string();
                continue;
            }
            if value.is_none() && i < args.len() {
                value = Some(args[i].clone());
                i += 1;
            }
            let Some(text) = value else {
                return Err(FlagError::Invalid(format!(
                    "flag needs an argument: -{name}"
                )));
            };
            let ok = match kind {
                Kind::Int => parse_int(&text).is_some(),
                Kind::Duration => parse_duration(&text).is_ok(),
                Kind::Str | Kind::Bool => true,
            };
            if !ok {
                return Err(FlagError::Invalid(format!(
                    "invalid value {} for flag -{name}: parse error",
                    quote(&text)
                )));
            }
            self.flags[pos].value = text;
        }
        self.args = args[i..].to_vec();
        Ok(())
    }

    /// Parses the process's own arguments. A bad flag prints the message
    /// and the usage text on stderr and exits 2; `-h` prints the usage text
    /// and exits 0.
    pub fn parse_env(&mut self) {
        let args: Vec<String> = std::env::args().skip(1).collect();
        match self.parse(&args) {
            Ok(()) => {}
            Err(FlagError::Help) => {
                eprint!("{}", self.usage());
                std::process::exit(0);
            }
            Err(FlagError::Invalid(msg)) => {
                eprintln!("{msg}");
                eprint!("{}", self.usage());
                std::process::exit(2);
            }
        }
    }

    /// The usage text: every flag in name order with what it takes, what
    /// it does and its default when that is not empty, zero or false.
    pub fn usage(&self) -> String {
        let mut out = format!("Usage of {}:\n", self.name);
        let mut flags: Vec<&Flag> = self.flags.iter().collect();
        flags.sort_by(|a, b| a.name.cmp(&b.name));
        for f in flags {
            out.push_str("  -");
            out.push_str(&f.name);
            let unit = match f.kind {
                Kind::Bool => "",
                Kind::Int => "int",
                Kind::Str => "string",
                Kind::Duration => "duration",
            };
            if !unit.is_empty() {
                out.push(' ');
                out.push_str(unit);
            }
            out.push_str("\n    \t");
            out.push_str(&f.usage.replace('\n', "\n    \t"));
            let zero = match f.kind {
                Kind::Bool => f.default == "false",
                Kind::Int => f.default == "0",
                Kind::Str => f.default.is_empty(),
                Kind::Duration => f.default == "0s",
            };
            if !zero {
                if f.kind == Kind::Str {
                    out.push_str(&format!(" (default {})", quote(&f.default)));
                } else {
                    out.push_str(&format!(" (default {})", f.default));
                }
            }
            out.push('\n');
        }
        out
    }
}

fn parse_bool(s: &str) -> Option<bool> {
    match s {
        "1" | "t" | "T" | "TRUE" | "true" | "True" => Some(true),
        "0" | "f" | "F" | "FALSE" | "false" | "False" => Some(false),
        _ => None,
    }
}

/// A whole number in decimal, or with a `0x`, `0o`, `0b` or `0` prefix.
fn parse_int(s: &str) -> Option<i64> {
    let (neg, digits) = match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    let digits = digits.replace('_', "");
    let lower = digits.to_ascii_lowercase();
    let (radix, body) = if let Some(rest) = lower.strip_prefix("0x") {
        (16, rest)
    } else if let Some(rest) = lower.strip_prefix("0b") {
        (2, rest)
    } else if let Some(rest) = lower.strip_prefix("0o") {
        (8, rest)
    } else if lower.len() > 1 && lower.starts_with('0') {
        (8, &lower[1..])
    } else {
        (10, lower.as_str())
    };
    if body.is_empty() || body.starts_with(['+', '-']) {
        return None;
    }
    let n = i64::from_str_radix(body, radix).ok()?;
    Some(if neg { -n } else { n })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn set() -> FlagSet {
        let mut f = FlagSet::new("prog");
        f.bool("check", false, "check only");
        f.bool("baseline", true, "also run the baseline");
        f.string("out", "dist", "where to write");
        f.int("runs", 1, "how many");
        f.duration("timeout", Duration::from_secs(900), "limit");
        f
    }

    #[test]
    fn forms() {
        let mut f = set();
        f.parse(&args(&[
            "-check",
            "--out=x",
            "-runs",
            "3",
            "-baseline=false",
            "-timeout",
            "90s",
            "rest",
            "-not-a-flag",
        ]))
        .unwrap();
        assert!(f.get_bool("check"));
        assert!(!f.get_bool("baseline"));
        assert_eq!(f.get("out"), "x");
        assert_eq!(f.get_int("runs"), 3);
        assert_eq!(f.get_duration("timeout"), Duration::from_secs(90));
        assert_eq!(f.args(), args(&["rest", "-not-a-flag"]));
    }

    #[test]
    fn defaults_and_terminator() {
        let mut f = set();
        f.parse(&args(&["--", "-check"])).unwrap();
        assert!(!f.get_bool("check"));
        assert!(f.get_bool("baseline"));
        assert_eq!(f.get("out"), "dist");
        assert_eq!(f.get_duration("timeout"), Duration::from_secs(900));
        assert_eq!(f.args(), args(&["-check"]));
    }

    #[test]
    fn errors() {
        let bad = |list: &[&str]| set().parse(&args(list)).unwrap_err().to_string();
        assert_eq!(bad(&["-nope"]), "flag provided but not defined: -nope");
        assert_eq!(bad(&["-out"]), "flag needs an argument: -out");
        assert_eq!(
            bad(&["-runs", "many"]),
            "invalid value \"many\" for flag -runs: parse error"
        );
        assert_eq!(
            bad(&["-check=perhaps"]),
            "invalid boolean value \"perhaps\" for -check: parse error"
        );
        assert_eq!(bad(&["---x"]), "bad flag syntax: ---x");
        assert_eq!(set().parse(&args(&["-h"])), Err(FlagError::Help));
    }

    #[test]
    fn usage_text() {
        let text = set().usage();
        assert!(text.starts_with("Usage of prog:\n  -baseline\n    \talso run the baseline (default true)\n  -check\n    \tcheck only\n"));
        assert!(text.contains("  -out string\n    \twhere to write (default \"dist\")\n"));
        assert!(text.contains("  -timeout duration\n    \tlimit (default 15m0s)\n"));
    }
}
