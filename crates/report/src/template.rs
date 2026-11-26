//! The small template language `report.html.tmpl` is written in: named
//! blocks (`{{define "x"}}`), fields of the value in hand (`{{.Name}}`,
//! `{{.Timings.Reused}}`), `if`/`else if`/`else`, `range`, `template`, and
//! a handful of functions. Everything a template prints is escaped for
//! HTML, so no value can break out of the page.

use std::collections::HashMap;

use apic_gocompat::{format_time, Time};

/// A value a template can show or test.
#[derive(Clone, Debug)]
pub(crate) enum Val {
    Nil,
    Bool(bool),
    Int(i64),
    Str(String),
    List(Vec<Val>),
    /// A record: its fields by name.
    Map(HashMap<&'static str, Val>),
    Time(Time),
    /// A length of time in nanoseconds.
    Duration(i64),
}

impl Val {
    pub(crate) fn map<const N: usize>(fields: [(&'static str, Val); N]) -> Val {
        Val::Map(HashMap::from(fields))
    }

    pub(crate) fn list<T>(items: impl IntoIterator<Item = T>, f: impl Fn(T) -> Val) -> Val {
        Val::List(items.into_iter().map(f).collect())
    }

    /// Whether an `if` takes the value for true: not false, zero, empty or
    /// absent. A record always is.
    fn truthy(&self) -> bool {
        match self {
            Val::Nil => false,
            Val::Bool(b) => *b,
            Val::Int(n) => *n != 0,
            Val::Str(s) => !s.is_empty(),
            Val::List(l) => !l.is_empty(),
            Val::Duration(d) => *d != 0,
            Val::Map(_) | Val::Time(_) => true,
        }
    }

    fn field(&self, name: &str) -> Val {
        match self {
            Val::Map(m) => m.get(name).cloned().unwrap_or(Val::Nil),
            _ => Val::Nil,
        }
    }

    /// The text the value prints as, before escaping.
    fn text(&self) -> String {
        match self {
            Val::Nil => String::new(),
            Val::Bool(b) => b.to_string(),
            Val::Int(n) => n.to_string(),
            Val::Str(s) => s.clone(),
            Val::Duration(d) => {
                apic_gocompat::format_duration(std::time::Duration::from_nanos((*d).max(0) as u64))
            }
            Val::Time(t) => t.format_rfc3339_nano(),
            Val::List(_) | Val::Map(_) => String::new(),
        }
    }
}

impl From<&str> for Val {
    fn from(s: &str) -> Val {
        Val::Str(s.to_string())
    }
}

impl From<String> for Val {
    fn from(s: String) -> Val {
        Val::Str(s)
    }
}

impl From<bool> for Val {
    fn from(b: bool) -> Val {
        Val::Bool(b)
    }
}

impl From<i64> for Val {
    fn from(n: i64) -> Val {
        Val::Int(n)
    }
}

impl From<usize> for Val {
    fn from(n: usize) -> Val {
        Val::Int(n as i64)
    }
}

/// One operand of an action.
#[derive(Clone, Debug)]
enum Term {
    /// `.` and `.A.B`: the value in hand, or a field of it.
    Field(Vec<String>),
    Str(String),
    Int(i64),
    /// A function's name.
    Ident(String),
}

/// An action's command: a value on its own, or a function and its
/// arguments.
type Command = Vec<Term>;

#[derive(Clone, Debug)]
enum Node {
    Text(String),
    Print(Command),
    /// The branches in order, each with its condition; then the `else`.
    If(Vec<(Command, Vec<Node>)>, Vec<Node>),
    Range(Command, Vec<Node>),
    Template(String, Command),
}

/// A parsed set of named blocks.
pub(crate) struct Template {
    blocks: HashMap<String, Vec<Node>>,
}

/// Splits an action's text into terms.
fn terms(action: &str) -> Result<Command, String> {
    let mut out = Vec::new();
    let mut rest = action.trim();
    while !rest.is_empty() {
        if let Some(quoted) = rest.strip_prefix('"') {
            let end = quoted
                .find('"')
                .ok_or_else(|| format!("unterminated string in {{{{{action}}}}}"))?;
            out.push(Term::Str(quoted[..end].to_string()));
            rest = quoted[end + 1..].trim_start();
            continue;
        }
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let word = &rest[..end];
        rest = rest[end..].trim_start();
        if let Some(path) = word.strip_prefix('.') {
            let fields = path.split('.').filter(|f| !f.is_empty());
            out.push(Term::Field(fields.map(str::to_string).collect()));
        } else if let Ok(n) = word.parse::<i64>() {
            out.push(Term::Int(n));
        } else {
            out.push(Term::Ident(word.to_string()));
        }
    }
    Ok(out)
}

/// What ended a run of nodes.
enum Stop {
    End,
    Else(Option<Command>),
    Eof,
}

struct Parser<'a> {
    rest: &'a str,
}

impl Parser<'_> {
    /// Reads nodes up to the `end` or `else` that closes them.
    fn nodes(&mut self) -> Result<(Vec<Node>, Stop), String> {
        let mut out = Vec::new();
        loop {
            let Some(open) = self.rest.find("{{") else {
                if !self.rest.is_empty() {
                    out.push(Node::Text(self.rest.to_string()));
                }
                self.rest = "";
                return Ok((out, Stop::Eof));
            };
            if open > 0 {
                out.push(Node::Text(self.rest[..open].to_string()));
            }
            let after = &self.rest[open + 2..];
            let close = after
                .find("}}")
                .ok_or_else(|| "unclosed action".to_string())?;
            let action = after[..close].trim();
            self.rest = &after[close + 2..];
            let (word, tail) = match action.split_once(char::is_whitespace) {
                Some((w, t)) => (w, t.trim()),
                None => (action, ""),
            };
            match word {
                "end" => return Ok((out, Stop::End)),
                "else" => {
                    let cond = match tail.strip_prefix("if") {
                        Some(cond) => Some(terms(cond)?),
                        None => None,
                    };
                    return Ok((out, Stop::Else(cond)));
                }
                "if" => out.push(self.if_node(terms(tail)?)?),
                "range" => {
                    let (body, stop) = self.nodes()?;
                    if !matches!(stop, Stop::End) {
                        return Err("range without end".to_string());
                    }
                    out.push(Node::Range(terms(tail)?, body));
                }
                "template" => {
                    let mut args = terms(tail)?;
                    if args.is_empty() {
                        return Err("template without a name".to_string());
                    }
                    let Term::Str(name) = args.remove(0) else {
                        return Err("template without a name".to_string());
                    };
                    out.push(Node::Template(name, args));
                }
                "define" => return Err("define inside a block".to_string()),
                _ => out.push(Node::Print(terms(action)?)),
            }
        }
    }

    fn if_node(&mut self, cond: Command) -> Result<Node, String> {
        let mut branches = Vec::new();
        let mut cond = cond;
        loop {
            let (body, stop) = self.nodes()?;
            branches.push((cond, body));
            match stop {
                Stop::End => return Ok(Node::If(branches, Vec::new())),
                Stop::Else(Some(next)) => cond = next,
                Stop::Else(None) => {
                    let (otherwise, stop) = self.nodes()?;
                    if !matches!(stop, Stop::End) {
                        return Err("else without end".to_string());
                    }
                    return Ok(Node::If(branches, otherwise));
                }
                Stop::Eof => return Err("if without end".to_string()),
            }
        }
    }
}

impl Template {
    /// Parses the blocks of a template. Text outside a block is not part
    /// of any.
    pub(crate) fn parse(source: &str) -> Result<Template, String> {
        let mut blocks = HashMap::new();
        let mut rest = source;
        while let Some(at) = rest.find("{{define ") {
            let after = &rest[at + "{{define ".len()..];
            let close = after
                .find("}}")
                .ok_or_else(|| "unclosed define".to_string())?;
            let name = after[..close].trim().trim_matches('"').to_string();
            let mut p = Parser {
                rest: &after[close + 2..],
            };
            let (nodes, stop) = p.nodes()?;
            if !matches!(stop, Stop::End) {
                return Err(format!("block {name:?} has no end"));
            }
            blocks.insert(name, nodes);
            rest = p.rest;
        }
        Ok(Template { blocks })
    }

    /// Renders the block `name` for `data`.
    pub(crate) fn execute(&self, name: &str, data: &Val) -> Result<String, String> {
        let mut out = String::new();
        self.block(name, data, &mut out)?;
        Ok(out)
    }

    fn block(&self, name: &str, dot: &Val, out: &mut String) -> Result<(), String> {
        let nodes = self
            .blocks
            .get(name)
            .ok_or_else(|| format!("no template {name:?}"))?;
        self.run(nodes, dot, out)
    }

    fn run(&self, nodes: &[Node], dot: &Val, out: &mut String) -> Result<(), String> {
        for node in nodes {
            match node {
                Node::Text(t) => out.push_str(t),
                Node::Print(cmd) => escape_into(&eval(cmd, dot)?.text(), out),
                Node::If(branches, otherwise) => {
                    let mut taken = false;
                    for (cond, body) in branches {
                        if eval(cond, dot)?.truthy() {
                            self.run(body, dot, out)?;
                            taken = true;
                            break;
                        }
                    }
                    if !taken {
                        self.run(otherwise, dot, out)?;
                    }
                }
                Node::Range(cmd, body) => {
                    if let Val::List(items) = eval(cmd, dot)? {
                        for item in &items {
                            self.run(body, item, out)?;
                        }
                    }
                }
                Node::Template(name, cmd) => {
                    let arg = if cmd.is_empty() {
                        Val::Nil
                    } else {
                        eval(cmd, dot)?
                    };
                    self.block(name, &arg, out)?;
                }
            }
        }
        Ok(())
    }
}

fn term(t: &Term, dot: &Val) -> Val {
    match t {
        Term::Field(path) => path.iter().fold(dot.clone(), |v, f| v.field(f)),
        Term::Str(s) => Val::Str(s.clone()),
        Term::Int(n) => Val::Int(*n),
        Term::Ident(_) => Val::Nil,
    }
}

fn int(v: &Val) -> Result<i64, String> {
    match v {
        Val::Int(n) => Ok(*n),
        other => Err(format!("{other:?} is not a number")),
    }
}

fn equal(a: &Val, b: &Val) -> Result<bool, String> {
    match (a, b) {
        (Val::Int(a), Val::Int(b)) => Ok(a == b),
        (Val::Str(a), Val::Str(b)) => Ok(a == b),
        (Val::Bool(a), Val::Bool(b)) => Ok(a == b),
        _ => Err(format!("cannot compare {a:?} with {b:?}")),
    }
}

/// Evaluates a command against the value in hand.
fn eval(cmd: &Command, dot: &Val) -> Result<Val, String> {
    let Some(first) = cmd.first() else {
        return Err("empty action".to_string());
    };
    let args: Vec<Val> = cmd[1..].iter().map(|t| term(t, dot)).collect();
    let arity = |n: usize, name: &str| -> Result<(), String> {
        if args.len() == n {
            Ok(())
        } else {
            Err(format!("{name} takes {n} arguments, got {}", args.len()))
        }
    };
    match first {
        Term::Ident(name) => match name.as_str() {
            "not" => {
                arity(1, name)?;
                Ok(Val::Bool(!args[0].truthy()))
            }
            "or" => {
                // The first argument that holds, else the last.
                let hit = args.iter().find(|a| a.truthy()).or(args.last());
                hit.cloned().ok_or_else(|| "or takes arguments".to_string())
            }
            "lt" => {
                arity(2, name)?;
                Ok(Val::Bool(int(&args[0])? < int(&args[1])?))
            }
            "gt" => {
                arity(2, name)?;
                Ok(Val::Bool(int(&args[0])? > int(&args[1])?))
            }
            "eq" => {
                arity(2, name)?;
                Ok(Val::Bool(equal(&args[0], &args[1])?))
            }
            "ne" => {
                arity(2, name)?;
                Ok(Val::Bool(!equal(&args[0], &args[1])?))
            }
            "len" => {
                arity(1, name)?;
                match &args[0] {
                    Val::List(l) => Ok(Val::from(l.len())),
                    Val::Str(s) => Ok(Val::from(s.len())),
                    other => Err(format!("len of {other:?}")),
                }
            }
            "ms" => {
                arity(1, name)?;
                match &args[0] {
                    Val::Duration(d) => Ok(Val::Str(format!("{} ms", d / 1_000_000))),
                    other => Err(format!("ms of {other:?}")),
                }
            }
            "json" => {
                arity(1, name)?;
                Ok(Val::Str(crate::html::pretty_json(&args[0].text())))
            }
            other => Err(format!("function {other:?} not defined")),
        },
        Term::Field(path) => {
            // A time shows itself the way a layout says.
            if let (Some((last, head)), [Val::Str(layout)]) = (path.split_last(), args.as_slice()) {
                if last == "Format" {
                    let at = head.iter().fold(dot.clone(), |v, f| v.field(f));
                    if let Val::Time(t) = at {
                        return Ok(Val::Str(format_time(t, "UTC", layout)));
                    }
                }
            }
            if !args.is_empty() {
                return Err(format!("{path:?} takes no arguments"));
            }
            Ok(term(first, dot))
        }
        literal => Ok(term(literal, dot)),
    }
}

/// Writes text into the page with the characters that mean something in
/// HTML replaced: in an element, in a quoted attribute and in the title
/// alike.
fn escape_into(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '\0' => out.push('\u{FFFD}'),
            '"' => out.push_str("&#34;"),
            '&' => out.push_str("&amp;"),
            '\'' => out.push_str("&#39;"),
            '+' => out.push_str("&#43;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            c => out.push(c),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(src: &str, data: Val) -> String {
        Template::parse(&format!("{{{{define \"t\"}}}}{src}{{{{end}}}}"))
            .unwrap()
            .execute("t", &data)
            .unwrap()
    }

    #[test]
    fn prints_fields_escaped() {
        let data = Val::map([
            ("Name", "a <b> & \"c\" + 'd'".into()),
            ("N", 3usize.into()),
            ("Inner", Val::map([("Ok", true.into())])),
        ]);
        assert_eq!(
            render("{{.Name}}|{{.N}}|{{.Inner.Ok}}|{{.Missing}}", data),
            "a &lt;b&gt; &amp; &#34;c&#34; &#43; &#39;d&#39;|3|true|"
        );
    }

    #[test]
    fn branches_and_loops() {
        let data = Val::map([
            ("Status", 302i64.into()),
            ("Word", "passed".into()),
            ("Empty", "".into()),
            (
                "Rows",
                Val::list([vec!["a", "b"], vec!["c"]], |r| Val::list(r, Val::from)),
            ),
            ("D", Val::Duration(1_999_999)),
        ]);
        let src = "{{if lt .Status 300}}ok{{else if lt .Status 400}}warn{{else}}fail{{end}} \
                   {{if eq .Word \"passed\"}}p{{end}}{{if ne .Word \"passed\"}}n{{end}} \
                   {{if not .Empty}}e{{end}}{{if or .Empty .Rows}}o{{end}}{{if gt .Status 1}}g{{end}} \
                   {{range .Rows}}[{{range .}}{{.}},{{end}}]{{end}} {{len .Rows}} {{ms .D}}";
        assert_eq!(render(src, data), "warn p eog [a,b,][c,] 2 1 ms");
    }

    #[test]
    fn blocks_call_each_other() {
        let t = Template::parse(
            "outside{{define \"a\"}}<{{template \"b\" .X}}>{{end}}\n{{define \"b\"}}{{.}}{{end}}",
        )
        .unwrap();
        let data = Val::map([("X", "x".into())]);
        assert_eq!(t.execute("a", &data).unwrap(), "<x>");
        assert!(t.execute("c", &data).is_err());
    }

    #[test]
    fn a_time_formats_itself() {
        let data = Val::map([("Time", Val::Time(Time::from_unix(0, 0)))]);
        assert_eq!(
            render("{{.Time.Format \"2006-01-02 15:04:05 UTC\"}}", data),
            "1970-01-01 00:00:00 UTC"
        );
    }

    #[test]
    fn broken_templates_are_refused() {
        for src in [
            "{{define \"a\"}}{{if .X}}{{end}}",
            "{{define \"a\"}}{{range .X}}",
            "{{define \"a\"}}{{.X",
        ] {
            assert!(Template::parse(src).is_err(), "{src}");
        }
    }
}
