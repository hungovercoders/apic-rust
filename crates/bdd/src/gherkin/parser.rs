//! The Gherkin parser: a line scanner, a matcher that classifies each line
//! for the current dialect, the state machine in [`super::table`], and the
//! builder that turns the matched lines into a [`Document`].
//!
//! Parsing does not stop at the first problem: every line that fits
//! nowhere is reported, with its position, in one [`ParseErrors`].

use std::collections::VecDeque;
use std::fmt;
use std::sync::LazyLock;

use regex::Regex;

use super::dialects::{dialect, Dialect};
use super::table::STATES;
use super::{
    Background, Comment, DataTable, DocString, Document, Examples, Feature, FeatureChild, Rule,
    RuleChild, Scenario, Step, TableRow, Tag,
};

const DEFAULT_DIALECT: &str = "en";
const DOCSTRING_SEPARATOR: &str = "\"\"\"";
const DOCSTRING_ALTERNATIVE_SEPARATOR: &str = "```";

/// The kinds of line the matcher tells apart.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum TokenType {
    Eof,
    Empty,
    Comment,
    TagLine,
    FeatureLine,
    RuleLine,
    BackgroundLine,
    ScenarioLine,
    ExamplesLine,
    StepLine,
    DocStringSeparator,
    TableRow,
    Language,
    Other,
}

/// The grammar rules the builder makes nodes for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum RuleType {
    None,
    GherkinDocument,
    Feature,
    FeatureHeader,
    Rule,
    RuleHeader,
    Background,
    ScenarioDefinition,
    Scenario,
    ExamplesDefinition,
    Examples,
    ExamplesTable,
    Step,
    DataTable,
    DocString,
    Tags,
    Description,
}

/// What a transition does to the tree before moving on.
pub(super) enum Action {
    Start(RuleType),
    End,
    Build,
}

pub(super) struct Transition {
    pub(super) token: TokenType,
    /// A tag line belongs to whatever follows the tags: `0` asks whether
    /// that is a scenario, `1` whether it is an examples table.
    pub(super) lookahead: Option<u8>,
    pub(super) actions: &'static [Action],
    pub(super) next: usize,
}

pub(super) struct State {
    /// The tokens the state accepts, as an error lists them.
    pub(super) expected: &'static str,
    pub(super) transitions: &'static [Transition],
}

/// One problem in a feature file, at a line and column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParseError {
    msg: String,
    line: usize,
    column: usize,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "({}:{}): {}", self.line, self.column, self.msg)
    }
}

/// Every problem found in one feature file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParseErrors(pub(crate) Vec<ParseError>);

impl fmt::Display for ParseErrors {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Parser errors:")?;
        for e in &self.0 {
            write!(f, "\n{e}")?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
struct LineSpan {
    text: String,
}

#[derive(Debug, Clone)]
struct Token {
    ty: TokenType,
    keyword: String,
    text: String,
    items: Vec<LineSpan>,
    line: usize,
    column: usize,
}

#[derive(Debug, Clone)]
struct Line {
    text: String,
    number: usize,
    /// Where the text starts after leading blanks and tabs.
    indent: usize,
    at_eof: bool,
}

impl Line {
    fn trimmed(&self) -> &str {
        &self.text[self.indent..]
    }

    fn is_empty(&self) -> bool {
        self.trimmed().is_empty()
    }

    fn starts_with(&self, prefix: &str) -> bool {
        self.trimmed().starts_with(prefix)
    }
}

/// Hands out the lines of a document, then one empty line marked as the
/// end, numbered one past the last.
struct Scanner {
    lines: VecDeque<String>,
    line: usize,
}

impl Scanner {
    fn new(data: &str) -> Scanner {
        let mut lines: VecDeque<String> = data
            .split('\n')
            .map(|l| l.strip_suffix('\r').unwrap_or(l).to_string())
            .collect();
        // A final newline ends the last line rather than starting another.
        if data.is_empty() || data.ends_with('\n') {
            lines.pop_back();
        }
        Scanner { lines, line: 0 }
    }

    fn scan(&mut self) -> Line {
        self.line += 1;
        match self.lines.pop_front() {
            Some(text) => {
                let indent = text.len() - text.trim_start_matches([' ', '\t']).len();
                Line {
                    text,
                    number: self.line,
                    indent,
                    at_eof: false,
                }
            }
            None => Line {
                text: String::new(),
                number: self.line,
                indent: 0,
                at_eof: true,
            },
        }
    }
}

static LANGUAGE_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^[\t\n\f\r ]*#[\t\n\f\r ]*language[\t\n\f\r ]*:[\t\n\f\r ]*([a-zA-Z\-_]+)[\t\n\f\r ]*$",
    )
    .expect("a valid pattern")
});

type Matched = Option<(Token, Option<ParseError>)>;

struct Matcher {
    lang: String,
    dialect: &'static Dialect,
    active_doc_string_separator: &'static str,
    indent_to_remove: usize,
}

fn is_ascii_space(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\x0c' | '\r' | ' ')
}

impl Matcher {
    fn new(language: &str) -> Option<Matcher> {
        Some(Matcher {
            lang: language.to_string(),
            dialect: dialect(language)?,
            active_doc_string_separator: "",
            indent_to_remove: 0,
        })
    }

    fn token(&self, ty: TokenType, line: usize, index: usize) -> Token {
        Token {
            ty,
            keyword: String::new(),
            text: String::new(),
            items: Vec::new(),
            line,
            column: index + 1,
        }
    }

    /// Tries one kind of token against a line. Only the end-of-file
    /// matcher sees the line after the last.
    fn matches(&mut self, ty: TokenType, line: &Line) -> Matched {
        if ty == TokenType::Eof {
            return line
                .at_eof
                .then(|| (self.token(ty, line.number, line.indent), None));
        }
        if line.at_eof {
            return None;
        }
        match ty {
            TokenType::Eof => None,
            TokenType::Empty => line
                .is_empty()
                .then(|| (self.token(ty, line.number, 0), None)),
            TokenType::Comment => line.starts_with("#").then(|| {
                let mut t = self.token(ty, line.number, 0);
                t.text = line.text.clone();
                (t, None)
            }),
            TokenType::TagLine => self.match_tag_line(line),
            TokenType::FeatureLine => self.match_title(line, ty, self.dialect.feature),
            TokenType::RuleLine => self.match_title(line, ty, self.dialect.rule),
            TokenType::BackgroundLine => self.match_title(line, ty, self.dialect.background),
            TokenType::ScenarioLine => self
                .match_title(line, ty, self.dialect.scenario)
                .or_else(|| self.match_title(line, ty, self.dialect.scenario_outline)),
            TokenType::ExamplesLine => self.match_title(line, ty, self.dialect.examples),
            TokenType::StepLine => self.match_step_line(line),
            TokenType::DocStringSeparator => self.match_doc_string_separator(line),
            TokenType::TableRow => self.match_table_row(line),
            TokenType::Language => self.match_language(line),
            TokenType::Other => Some((self.match_other(line), None)),
        }
    }

    fn match_tag_line(&self, line: &Line) -> Matched {
        if !line.starts_with("@") {
            return None;
        }
        let trimmed = line.trimmed();
        // A ` #` ends the tags: the rest of the line is a comment.
        let mut uncommented = trimmed;
        let mut run_start: Option<usize> = None;
        for (i, c) in trimmed.char_indices() {
            if is_ascii_space(c) {
                run_start.get_or_insert(i);
            } else {
                if c == '#' {
                    if let Some(start) = run_start {
                        uncommented = &trimmed[..start];
                        break;
                    }
                }
                run_start = None;
            }
        }
        let mut tags = Vec::new();
        let mut column = line.indent + 1;
        let mut err = None;
        for part in uncommented.split('@') {
            let txt = part.trim_end();
            if txt.is_empty() {
                continue;
            }
            if txt.contains(is_ascii_space) {
                err = Some(ParseError {
                    msg: "A tag may not contain whitespace".to_string(),
                    line: line.number,
                    column,
                });
                break;
            }
            tags.push(LineSpan {
                text: format!("@{txt}"),
            });
            column += part.chars().count() + 1;
        }
        let mut t = self.token(TokenType::TagLine, line.number, line.indent);
        t.items = tags;
        Some((t, err))
    }

    fn match_title(&self, line: &Line, ty: TokenType, keywords: &[&str]) -> Matched {
        for keyword in keywords {
            let rest = line
                .trimmed()
                .strip_prefix(keyword)
                .and_then(|r| r.strip_prefix(':'));
            if let Some(rest) = rest {
                let mut t = self.token(ty, line.number, line.indent);
                t.keyword = (*keyword).to_string();
                t.text = rest.trim_matches([' ', '\t']).to_string();
                return Some((t, None));
            }
        }
        None
    }

    fn match_step_line(&self, line: &Line) -> Matched {
        for keyword in self.dialect.steps {
            if let Some(rest) = line.trimmed().strip_prefix(keyword) {
                let mut t = self.token(TokenType::StepLine, line.number, line.indent);
                t.keyword = (*keyword).to_string();
                t.text = rest.trim_matches([' ', '\t']).to_string();
                return Some((t, None));
            }
        }
        None
    }

    fn match_doc_string_separator(&mut self, line: &Line) -> Matched {
        if !self.active_doc_string_separator.is_empty() {
            if !line.starts_with(self.active_doc_string_separator) {
                return None;
            }
            // The closing delimiter.
            let mut t = self.token(TokenType::DocStringSeparator, line.number, line.indent);
            t.keyword = self.active_doc_string_separator.to_string();
            self.indent_to_remove = 0;
            self.active_doc_string_separator = "";
            return Some((t, None));
        }
        if line.starts_with(DOCSTRING_SEPARATOR) {
            self.active_doc_string_separator = DOCSTRING_SEPARATOR;
        } else if line.starts_with(DOCSTRING_ALTERNATIVE_SEPARATOR) {
            self.active_doc_string_separator = DOCSTRING_ALTERNATIVE_SEPARATOR;
        } else {
            return None;
        }
        // The opening delimiter, with the media type after it.
        self.indent_to_remove = line.indent;
        let mut t = self.token(TokenType::DocStringSeparator, line.number, line.indent);
        t.keyword = self.active_doc_string_separator.to_string();
        t.text = line.trimmed()[self.active_doc_string_separator.len()..].to_string();
        Some((t, None))
    }

    fn match_table_row(&self, line: &Line) -> Matched {
        let rest = line.trimmed().strip_prefix('|')?;
        let not_newline_space = |c: char| c.is_whitespace() && c != '\n';
        let mut cells = Vec::new();
        let mut cell = String::new();
        let mut chars = rest.chars();
        while let Some(c) = chars.next() {
            if c == '|' {
                let text = cell
                    .trim_start_matches(not_newline_space)
                    .trim_end_matches(not_newline_space)
                    .to_string();
                cells.push(LineSpan { text });
                cell.clear();
            } else if c == '\\' {
                // The escaping character is not part of the cell.
                match chars.next() {
                    Some('n') => cell.push('\n'),
                    Some(next) => {
                        if next != '|' && next != '\\' {
                            cell.push('\\');
                        }
                        cell.push(next);
                    }
                    None => break,
                }
            } else {
                cell.push(c);
            }
        }
        let mut t = self.token(TokenType::TableRow, line.number, line.indent);
        t.items = cells;
        Some((t, None))
    }

    fn match_language(&mut self, line: &Line) -> Matched {
        let caps = LANGUAGE_PATTERN.captures(line.trimmed())?;
        let lang = &caps[1];
        let mut t = self.token(TokenType::Language, line.number, line.indent);
        t.text = lang.to_string();
        match dialect(lang) {
            None => {
                let err = ParseError {
                    msg: format!("Language not supported: {lang}"),
                    line: t.line,
                    column: t.column,
                };
                Some((t, Some(err)))
            }
            Some(d) => {
                self.lang = lang.to_string();
                self.dialect = d;
                Some((t, None))
            }
        }
    }

    fn match_other(&self, line: &Line) -> Token {
        let mut t = self.token(TokenType::Other, line.number, 0);
        let element = line.text.as_str();
        let txt = element.trim_start_matches(' ');
        // A doc string keeps what is indented beyond its delimiter.
        let kept = if element.len() - txt.len() > self.indent_to_remove {
            &element[self.indent_to_remove..]
        } else {
            txt
        };
        t.text = match self.active_doc_string_separator {
            DOCSTRING_SEPARATOR => kept.replace("\\\"\\\"\\\"", DOCSTRING_SEPARATOR),
            DOCSTRING_ALTERNATIVE_SEPARATOR => {
                kept.replace("\\`\\`\\`", DOCSTRING_ALTERNATIVE_SEPARATOR)
            }
            _ => kept.to_string(),
        };
        t
    }
}

/// What a finished rule leaves in its parent node.
enum Built {
    Node(Node),
    Step(Step),
    DocString(DocString),
    DataTable(DataTable),
    Background(Background),
    Scenario(Scenario),
    Examples(Examples),
    Rows(Vec<TableRow>),
    Description(String),
    Feature(Option<Feature>),
    Rule(Option<Rule>),
    Document(Document),
}

struct Node {
    rule: RuleType,
    tokens: Vec<Token>,
    children: Vec<(RuleType, Built)>,
}

impl Node {
    fn new(rule: RuleType) -> Node {
        Node {
            rule,
            tokens: Vec::new(),
            children: Vec::new(),
        }
    }

    fn token(&self, ty: TokenType) -> Option<&Token> {
        self.tokens.iter().find(|t| t.ty == ty)
    }

    fn tokens(&self, ty: TokenType) -> impl Iterator<Item = &Token> {
        self.tokens.iter().filter(move |t| t.ty == ty)
    }

    fn take_all(&mut self, rule: RuleType) -> Vec<Built> {
        let mut out = Vec::new();
        let mut rest = Vec::new();
        for (r, b) in self.children.drain(..) {
            if r == rule {
                out.push(b);
            } else {
                rest.push((r, b));
            }
        }
        self.children = rest;
        out
    }

    fn take(&mut self, rule: RuleType) -> Option<Built> {
        let i = self.children.iter().position(|(r, _)| *r == rule)?;
        Some(self.children.remove(i).1)
    }

    fn take_node(&mut self, rule: RuleType) -> Option<Node> {
        match self.take(rule) {
            Some(Built::Node(n)) => Some(n),
            _ => None,
        }
    }

    fn description(&mut self) -> String {
        match self.take(RuleType::Description) {
            Some(Built::Description(d)) => d,
            _ => String::new(),
        }
    }

    fn steps(&mut self) -> Vec<Step> {
        self.take_all(RuleType::Step)
            .into_iter()
            .filter_map(|b| match b {
                Built::Step(s) => Some(s),
                _ => None,
            })
            .collect()
    }

    fn background(&mut self) -> Option<Background> {
        match self.take(RuleType::Background) {
            Some(Built::Background(b)) => Some(b),
            _ => None,
        }
    }

    fn scenarios(&mut self) -> Vec<Scenario> {
        self.take_all(RuleType::ScenarioDefinition)
            .into_iter()
            .filter_map(|b| match b {
                Built::Scenario(s) => Some(s),
                _ => None,
            })
            .collect()
    }
}

struct Builder<'a> {
    stack: Vec<Node>,
    comments: Vec<Comment>,
    new_id: &'a mut dyn FnMut() -> String,
}

impl Builder<'_> {
    fn current(&mut self) -> &mut Node {
        self.stack.last_mut().expect("the root node")
    }

    fn build(&mut self, token: Token) {
        if token.ty == TokenType::Comment {
            self.comments.push(Comment {
                line: token.line,
                text: token.text,
            });
        } else {
            self.current().tokens.push(token);
        }
    }

    fn start_rule(&mut self, rule: RuleType) {
        self.stack.push(Node::new(rule));
    }

    fn end_rule(&mut self) -> Option<ParseError> {
        if self.stack.len() < 2 {
            return None;
        }
        let node = self.stack.pop().expect("a node");
        let rule = node.rule;
        let (built, err) = self.transform(node);
        self.current().children.push((rule, built));
        err
    }

    fn table_rows(&mut self, node: &Node) -> (Vec<TableRow>, Option<ParseError>) {
        let mut rows = Vec::new();
        for token in node.tokens(TokenType::TableRow) {
            rows.push(TableRow {
                id: (self.new_id)(),
                line: token.line,
                column: token.column,
                cells: token.items.iter().map(|i| i.text.clone()).collect(),
            });
        }
        // Every row has as many cells as the first.
        let err = rows
            .iter()
            .find(|r| r.cells.len() != rows[0].cells.len())
            .map(|r| ParseError {
                msg: "inconsistent cell count within the table".to_string(),
                line: r.line,
                column: r.column,
            });
        (rows, err)
    }

    fn tags(&mut self, node: &mut Node) -> Vec<Tag> {
        let mut tags = Vec::new();
        let Some(tags_node) = node.take_node(RuleType::Tags) else {
            return tags;
        };
        for token in tags_node.tokens(TokenType::TagLine) {
            for item in &token.items {
                // Every tag takes an id, in order.
                (self.new_id)();
                tags.push(Tag {
                    line: token.line,
                    name: item.text.clone(),
                });
            }
        }
        tags
    }

    fn transform(&mut self, node: Node) -> (Built, Option<ParseError>) {
        match node.rule {
            RuleType::Step => self.step(node),
            RuleType::DocString => self.doc_string(node),
            RuleType::DataTable => {
                let (rows, err) = self.table_rows(&node);
                (Built::DataTable(DataTable { rows }), err)
            }
            RuleType::Background => self.background(node),
            RuleType::ScenarioDefinition => self.scenario(node),
            RuleType::ExamplesDefinition => self.examples(node),
            RuleType::ExamplesTable => {
                let (rows, err) = self.table_rows(&node);
                (Built::Rows(rows), err)
            }
            RuleType::Description => self.description(node),
            RuleType::Feature => self.feature(node),
            RuleType::Rule => self.rule(node),
            RuleType::GherkinDocument => self.document(node),
            _ => (Built::Node(node), None),
        }
    }

    /// A step, with the table or doc string under it.
    fn step(&mut self, mut node: Node) -> (Built, Option<ParseError>) {
        let Some(line) = node.token(TokenType::StepLine).cloned() else {
            return (Built::Node(node), None);
        };
        let id = (self.new_id)();
        let data_table = match node.take(RuleType::DataTable) {
            Some(Built::DataTable(t)) => Some(t),
            _ => None,
        };
        let doc_string = match node.take(RuleType::DocString) {
            Some(Built::DocString(d)) => Some(d),
            _ => None,
        };
        let step = Step {
            id,
            line: line.line,
            keyword: line.keyword,
            text: line.text,
            data_table,
            doc_string,
        };
        (Built::Step(step), None)
    }

    /// A doc string.
    fn doc_string(&self, node: Node) -> (Built, Option<ParseError>) {
        let Some(sep) = node.token(TokenType::DocStringSeparator) else {
            return (Built::Node(node), None);
        };
        let content: Vec<&str> = node
            .tokens(TokenType::Other)
            .map(|t| t.text.as_str())
            .collect();
        let doc = DocString {
            line: sep.line,
            media_type: sep.text.clone(),
            content: content.join("\n"),
            delimiter: sep.keyword.clone(),
        };
        (Built::DocString(doc), None)
    }

    /// A background.
    fn background(&mut self, mut node: Node) -> (Built, Option<ParseError>) {
        let Some(line) = node.token(TokenType::BackgroundLine).cloned() else {
            return (Built::Node(node), None);
        };
        (self.new_id)();
        let bg = Background {
            keyword: line.keyword,
            name: line.text,
            steps: node.steps(),
        };
        (Built::Background(bg), None)
    }

    /// A scenario and its examples.
    fn scenario(&mut self, mut node: Node) -> (Built, Option<ParseError>) {
        let Some(mut scenario_node) = node.take_node(RuleType::Scenario) else {
            return (Built::Node(node), None);
        };
        let Some(line) = scenario_node.token(TokenType::ScenarioLine).cloned() else {
            return (Built::Node(node), None);
        };
        let tags = self.tags(&mut node);
        let examples = scenario_node
            .take_all(RuleType::ExamplesDefinition)
            .into_iter()
            .filter_map(|b| match b {
                Built::Examples(e) => Some(e),
                _ => None,
            })
            .collect();
        let sc = Scenario {
            id: (self.new_id)(),
            line: line.line,
            tags,
            keyword: line.keyword,
            name: line.text,
            description: scenario_node.description(),
            steps: scenario_node.steps(),
            examples,
        };
        (Built::Scenario(sc), None)
    }

    /// One examples table of a scenario outline.
    fn examples(&mut self, mut node: Node) -> (Built, Option<ParseError>) {
        let tags = self.tags(&mut node);
        let Some(mut examples_node) = node.take_node(RuleType::Examples) else {
            return (Built::Node(node), None);
        };
        let Some(line) = examples_node.token(TokenType::ExamplesLine).cloned() else {
            return (Built::Node(node), None);
        };
        let mut rows = match examples_node.take(RuleType::ExamplesTable) {
            Some(Built::Rows(rows)) => rows,
            _ => Vec::new(),
        };
        let table_header = (!rows.is_empty()).then(|| rows.remove(0));
        (self.new_id)();
        let ex = Examples {
            line: line.line,
            tags,
            keyword: line.keyword,
            name: line.text,
            table_header,
            table_body: rows,
        };
        (Built::Examples(ex), None)
    }

    /// The free text under a heading, without the blank lines that end it.
    fn description(&self, node: Node) -> (Built, Option<ParseError>) {
        let mut lines: Vec<&str> = node
            .tokens(TokenType::Other)
            .map(|t| t.text.as_str())
            .collect();
        while lines.last().is_some_and(|l| l.trim().is_empty()) {
            lines.pop();
        }
        (Built::Description(lines.join("\n")), None)
    }

    /// The feature: its header, then its background, scenarios and rules.
    fn feature(&mut self, mut node: Node) -> (Built, Option<ParseError>) {
        let Some(mut header) = node.take_node(RuleType::FeatureHeader) else {
            return (Built::Feature(None), None);
        };
        let tags = self.tags(&mut header);
        let Some(line) = header.token(TokenType::FeatureLine).cloned() else {
            return (Built::Feature(None), None);
        };
        let mut children = Vec::new();
        if let Some(bg) = node.background() {
            children.push(FeatureChild::Background(bg));
        }
        children.extend(node.scenarios().into_iter().map(FeatureChild::Scenario));
        for built in node.take_all(RuleType::Rule) {
            if let Built::Rule(Some(rule)) = built {
                children.push(FeatureChild::Rule(rule));
            }
        }
        let feature = Feature {
            line: line.line,
            tags,
            keyword: line.keyword,
            name: line.text,
            description: header.description(),
            children,
        };
        (Built::Feature(Some(feature)), None)
    }

    /// A rule: its background and scenarios.
    fn rule(&mut self, mut node: Node) -> (Built, Option<ParseError>) {
        let Some(mut header) = node.take_node(RuleType::RuleHeader) else {
            return (Built::Rule(None), None);
        };
        if header.token(TokenType::RuleLine).is_none() {
            return (Built::Rule(None), None);
        }
        let tags = self.tags(&mut header);
        let mut children = Vec::new();
        if let Some(bg) = node.background() {
            children.push(RuleChild::Background(bg));
        }
        children.extend(node.scenarios().into_iter().map(RuleChild::Scenario));
        (self.new_id)();
        let rule = Rule { tags, children };
        (Built::Rule(Some(rule)), None)
    }

    /// The whole document.
    fn document(&mut self, mut node: Node) -> (Built, Option<ParseError>) {
        let feature = match node.take(RuleType::Feature) {
            Some(Built::Feature(f)) => f,
            _ => None,
        };
        let doc = Document {
            feature,
            comments: std::mem::take(&mut self.comments),
        };
        (Built::Document(doc), None)
    }
}

struct Context<'a> {
    scanner: Scanner,
    matcher: Matcher,
    builder: Builder<'a>,
    queue: VecDeque<Line>,
    errors: Vec<ParseError>,
}

impl Context<'_> {
    fn scan(&mut self) -> Line {
        match self.queue.pop_front() {
            Some(line) => line,
            None => self.scanner.scan(),
        }
    }

    /// Reads ahead past blank lines, comments and further tags to see
    /// whether the tags belong to a line of the wanted kind. The lines
    /// read are queued to be parsed again.
    fn lookahead(&mut self, wanted: TokenType) -> bool {
        let mut seen = Vec::new();
        let mut found = false;
        loop {
            let line = self.scan();
            seen.push(line.clone());
            if self.matcher.matches(wanted, &line).is_some() {
                found = true;
                break;
            }
            let skippable = [TokenType::Empty, TokenType::Comment, TokenType::TagLine]
                .into_iter()
                .any(|ty| self.matcher.matches(ty, &line).is_some());
            if !skippable || line.at_eof {
                break;
            }
        }
        self.queue.extend(seen);
        found
    }

    fn step(&mut self, state: usize, line: &Line) -> usize {
        let st = &STATES[state];
        for tr in st.transitions {
            let Some((token, err)) = self.matcher.matches(tr.token, line) else {
                continue;
            };
            if let Some(which) = tr.lookahead {
                let wanted = if which == 0 {
                    TokenType::ScenarioLine
                } else {
                    TokenType::ExamplesLine
                };
                if !self.lookahead(wanted) {
                    continue;
                }
            }
            let mut token = Some(token);
            for action in tr.actions {
                match action {
                    Action::Start(rule) => self.builder.start_rule(*rule),
                    Action::End => {
                        if let Some(e) = self.builder.end_rule() {
                            self.errors.push(e);
                        }
                    }
                    Action::Build => {
                        if let Some(t) = token.take() {
                            self.builder.build(t);
                        }
                    }
                }
            }
            self.errors.extend(err);
            return tr.next;
        }
        self.errors.push(if line.at_eof {
            ParseError {
                msg: format!("unexpected end of file, expected: {}", st.expected),
                line: line.number,
                column: 0,
            }
        } else {
            ParseError {
                msg: format!("expected: {}, got '{}'", st.expected, line.text),
                line: line.number,
                column: line.indent + 1,
            }
        });
        state
    }
}

/// Parses a feature file. Ids for the nodes are drawn from `new_id` as
/// each node is completed: a step after its table's rows, a scenario after
/// its steps, examples and tags, the feature's tags last of all.
pub(crate) fn parse(
    data: &[u8],
    new_id: &mut dyn FnMut() -> String,
) -> Result<Document, ParseErrors> {
    let text = String::from_utf8_lossy(data);
    let mut builder = Builder {
        stack: vec![Node::new(RuleType::None)],
        comments: Vec::new(),
        new_id,
    };
    builder.start_rule(RuleType::GherkinDocument);
    let mut ctx = Context {
        scanner: Scanner::new(&text),
        matcher: Matcher::new(DEFAULT_DIALECT).expect("the default dialect"),
        builder,
        queue: VecDeque::new(),
        errors: Vec::new(),
    };
    let mut state = 0;
    loop {
        let line = ctx.scan();
        state = ctx.step(state, &line);
        if line.at_eof {
            break;
        }
    }
    if let Some(e) = ctx.builder.end_rule() {
        ctx.errors.push(e);
    }
    if !ctx.errors.is_empty() {
        return Err(ParseErrors(ctx.errors));
    }
    let doc = match ctx.builder.current().take(RuleType::GherkinDocument) {
        Some(Built::Document(doc)) => doc,
        _ => Document::default(),
    };
    Ok(doc)
}
