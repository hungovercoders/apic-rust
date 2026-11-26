use std::fmt;

use apic_gocompat::quote;
use apic_httpfile::quote_param;

use crate::urlpath::url_path;

/// What a curl command line asked for.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Request {
    pub method: String,
    pub url: String,
    pub headers: Vec<Header>,
    /// The inline body.
    pub body: String,
    /// `-d @file`: sent as `< ./file`.
    pub body_file: String,
    /// The `-F` fields, a multipart body.
    pub parts: Vec<Part>,
    /// `-u user:password`, as `# @auth basic`.
    pub user: String,
    /// `-k`.
    pub insecure: bool,
    /// `-L`.
    pub follow: bool,
    /// `HTTP/1.1` for `--http1.1` and `HTTP/2` for `--http2`, written on
    /// the request line; empty otherwise.
    pub http_version: String,
    pub warnings: Vec<String>,
}

/// One `-H` header.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Header {
    pub name: String,
    pub value: String,
}

/// One `-F` field.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Part {
    pub name: String,
    pub value: String,
    /// `name=@path`.
    pub file: String,
    /// `;filename=`.
    pub filename: String,
    /// `;type=`.
    pub content_type: String,
}

/// Why a command line could not be read: its quoting does not close, a
/// flag lacks its value, or there is no URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    message: String,
}

impl Error {
    fn new(message: impl Into<String>) -> Self {
        Error {
            message: message.into(),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

/// Reads a curl command line, with its shell quoting and line
/// continuations, and returns the request it describes. Unknown flags are
/// warnings, not errors; an unreadable line or a missing URL is an error.
pub fn parse(command: &str) -> Result<Request, Error> {
    let mut tokens = tokenize(command)?;
    if tokens
        .first()
        .is_some_and(|t| t == "curl" || t.ends_with("/curl"))
    {
        tokens.remove(0);
    }
    let mut p = Parser::default();
    p.read_tokens(tokens)?;
    p.resolve_url()?;
    p.resolve_body();
    p.resolve_method_and_content_type();
    Ok(p.r)
}

/// The state [`parse`] builds up while it reads the flags, before it
/// settles the URL, the body, the method and the content type.
#[derive(Default)]
struct Parser {
    r: Request,
    data: Vec<String>,
    data_file: String,
    get: bool,
    head: bool,
    content_type_set: bool,
    positional: Vec<String>,
}

impl Parser {
    fn warn(&mut self, message: String) {
        self.r.warnings.push(message);
    }

    /// Walks the arguments: positional ones are kept for the URL, flags
    /// are looked up and applied, a bundle of boolean short flags (`-sSL`)
    /// is split into its letters.
    fn read_tokens(&mut self, mut tokens: Vec<String>) -> Result<(), Error> {
        let mut i = 0;
        while i < tokens.len() {
            let tok = tokens[i].clone();
            if tok == "--" {
                self.positional.extend(tokens.drain(i + 1..));
                return Ok(());
            }
            if !tok.starts_with('-') || tok == "-" {
                self.positional.push(tok);
                i += 1;
                continue;
            }
            let (name, mut value, attached) = split_flag(&tok);
            let Some(spec) = flag(name) else {
                if let Some(bundle) = split_bundle(&tok) {
                    tokens.splice(i..=i, bundle);
                    continue;
                }
                self.warn(format!("unknown flag {tok} (ignored)"));
                i += 1;
                continue;
            };
            if spec.takes_value && !attached {
                if i + 1 >= tokens.len() {
                    return Err(Error::new(format!("{tok} needs a value")));
                }
                i += 1;
                value = tokens[i].clone();
            }
            self.apply(spec.meaning, &tok, value);
            i += 1;
        }
        Ok(())
    }

    /// Applies one flag meaning to the request being built. A meaning with
    /// nothing to do, such as `Quiet`, is read and dropped.
    fn apply(&mut self, meaning: Meaning, tok: &str, value: String) {
        match meaning {
            Meaning::Request => self.r.method = to_upper(&value),
            Meaning::Header => self.header(&value),
            Meaning::Data => self.data_or_file(value),
            Meaning::DataRaw => self.data.push(value),
            Meaning::DataUrlencode => {
                let encoded = urlencode_data(&value, &mut self.r.warnings);
                self.data.push(encoded);
            }
            Meaning::Form => self.form(&value, false),
            Meaning::FormString => self.form(&value, true),
            Meaning::User => self.r.user = value,
            Meaning::Url => self.positional.push(value),
            Meaning::Json => self.json(value),
            Meaning::Oauth2Bearer => self.add_header("Authorization", format!("Bearer {value}")),
            Meaning::Get => self.get = true,
            Meaning::Head => self.head = true,
            Meaning::Cookie => self.cookie(value),
            Meaning::UserAgent => self.add_header("User-Agent", value),
            Meaning::Referer => self.add_header("Referer", value),
            Meaning::Insecure => self.r.insecure = true,
            Meaning::Location => self.r.follow = true,
            // apic decompresses gzip responses on its own, so --compressed
            // needs nothing.
            Meaning::Compressed | Meaning::Quiet => {}
            Meaning::Http11 => self.r.http_version = "HTTP/1.1".to_string(),
            Meaning::Http2 => self.r.http_version = "HTTP/2".to_string(),
            Meaning::Tls => self.warn(format!(
                "{tok}: configure tls: in apic.yaml, or pass {tok} to apic itself"
            )),
            Meaning::Ignored => self.warn(format!(
                "{tok} is about curl's own output or transport and has no place in a request file (ignored)"
            )),
        }
    }

    fn add_header(&mut self, name: impl Into<String>, value: impl Into<String>) {
        self.r.headers.push(Header {
            name: name.into(),
            value: value.into(),
        });
    }

    fn header(&mut self, value: &str) {
        let Some((k, v)) = value.split_once(':') else {
            let k = value.trim();
            // curl's spelling of an empty header
            if let Some(name) = k.strip_suffix(';') {
                self.add_header(name, "");
            } else {
                self.warn(format!(
                    "header {} is not `Name: value` (ignored)",
                    quote(value)
                ));
            }
            return;
        };
        let k = k.trim();
        if is_content_type(k) {
            self.content_type_set = true;
        }
        self.add_header(k, v.trim());
    }

    fn data_or_file(&mut self, value: String) {
        let Some(file) = value.strip_prefix('@') else {
            self.data.push(value);
            return;
        };
        if !self.data_file.is_empty() || !self.data.is_empty() {
            self.warn(format!("more than one body: {value} dropped"));
            return;
        }
        self.data_file = file.to_string();
    }

    fn form(&mut self, value: &str, literal: bool) {
        match parse_form(value, literal) {
            Some(part) => self.r.parts.push(part),
            None => self.warn(format!(
                "form field {} is not name=value (ignored)",
                quote(value)
            )),
        }
    }

    fn json(&mut self, value: String) {
        self.data.push(value);
        if !self.content_type_set {
            self.add_header("Content-Type", "application/json");
            self.content_type_set = true;
        }
        self.add_header("Accept", "application/json");
    }

    fn cookie(&mut self, value: String) {
        if !value.contains('=') {
            self.warn(format!(
                "-b {value} names a cookie file, which apic does not read; switch the jar on with --cookies instead"
            ));
            return;
        }
        self.add_header("Cookie", value);
    }

    /// Picks the URL: the argument that looks like one; when none does,
    /// the first, with curl's own `http://` default. Anything else is
    /// reported.
    fn resolve_url(&mut self) -> Result<(), Error> {
        if let Some(arg) = self.positional.iter().find(|arg| arg.contains("://")) {
            self.r.url = arg.clone();
        }
        for arg in std::mem::take(&mut self.positional) {
            if !self.r.url.is_empty() && arg == self.r.url {
                continue;
            }
            if self.r.url.is_empty() {
                self.r.url = format!("http://{arg}");
            } else {
                self.warn(format!("extra argument {} (ignored)", quote(&arg)));
            }
        }
        if self.r.url.is_empty() {
            return Err(Error::new("no URL in the command"));
        }
        Ok(())
    }

    /// Joins the data arguments into the body, or, under `-G`, onto the
    /// query string.
    fn resolve_body(&mut self) {
        let body = self.data.join("&");
        if self.get && (!body.is_empty() || !self.data_file.is_empty()) {
            if !self.data_file.is_empty() {
                self.warn("-G with a body file: the file is not appended to the URL".to_string());
            }
            if !body.is_empty() {
                let sep = if self.r.url.contains('?') { '&' } else { '?' };
                self.r.url.push(sep);
                self.r.url.push_str(&body);
            }
        } else if !self.data_file.is_empty() {
            self.r.body_file = self.data_file.clone();
        } else {
            self.r.body = body;
        }
    }

    /// Fills in what curl would infer: the method from `-I`, `-G` and the
    /// body, and the content type of a body or a form.
    fn resolve_method_and_content_type(&mut self) {
        let r = &mut self.r;
        let has_body = !r.body.is_empty() || !r.body_file.is_empty() || !r.parts.is_empty();
        if r.method.is_empty() {
            r.method = if self.head {
                "HEAD"
            } else if has_body && !self.get {
                "POST"
            } else {
                "GET"
            }
            .to_string();
        }
        if !r.parts.is_empty() {
            r.headers.retain(|h| !is_content_type(&h.name));
            self.add_header(
                "Content-Type",
                "multipart/form-data; boundary=WebAppBoundary",
            );
        } else if (!r.body.is_empty() || !r.body_file.is_empty()) && !self.content_type_set {
            // curl's default for -d.
            self.add_header("Content-Type", "application/x-www-form-urlencoded");
        }
    }
}

fn is_content_type(name: &str) -> bool {
    name.eq_ignore_ascii_case("Content-Type")
}

/// Upper-cases a method one character at a time, leaving alone the few
/// characters whose upper case is more than one character.
fn to_upper(s: &str) -> String {
    s.chars()
        .map(|c| {
            let mut upper = c.to_uppercase();
            match (upper.next(), upper.next()) {
                (Some(u), None) => u,
                _ => c,
            }
        })
        .collect()
}

/// What a curl option means to apic.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Meaning {
    Request,
    Header,
    Data,
    DataRaw,
    DataUrlencode,
    Form,
    FormString,
    User,
    Url,
    Json,
    Oauth2Bearer,
    Get,
    Head,
    Cookie,
    UserAgent,
    Referer,
    Insecure,
    Location,
    Compressed,
    Http11,
    Http2,
    Tls,
    Quiet,
    Ignored,
}

#[derive(Clone, Copy)]
struct FlagSpec {
    meaning: Meaning,
    takes_value: bool,
}

/// Maps every curl option apic reads (or knowingly ignores) to what it
/// means. Long options are keyed with their dashes, short ones too.
fn flag(name: &str) -> Option<FlagSpec> {
    use Meaning::*;
    let (meaning, takes_value) = match name {
        "-X" | "--request" => (Request, true),
        "-H" | "--header" => (Header, true),
        "-d" | "--data" | "--data-ascii" | "--data-binary" => (Data, true),
        "--data-raw" => (DataRaw, true),
        "--data-urlencode" => (DataUrlencode, true),
        "-F" | "--form" => (Form, true),
        "--form-string" => (FormString, true),
        "-u" | "--user" => (User, true),
        "--url" => (Url, true),
        "-G" | "--get" => (Get, false),
        "-I" | "--head" => (Head, false),
        "-b" | "--cookie" => (Cookie, true),
        "-A" | "--user-agent" => (UserAgent, true),
        "-e" | "--referer" => (Referer, true),
        "-k" | "--insecure" => (Insecure, false),
        "-L" | "--location" => (Location, false),
        "--compressed" => (Compressed, false),
        "-s" | "--silent" | "-S" | "--show-error" | "-v" | "--verbose" | "-i" | "--include"
        | "-#" | "--progress-bar" | "-f" | "--fail" => (Quiet, false),
        "--cacert" | "--cert" | "--key" | "-E" => (Tls, true),
        "--json" => (Json, true),
        "--oauth2-bearer" => (Oauth2Bearer, true),
        // Value-taking options apic knows nothing to do with: named so
        // their value is never mistaken for the URL.
        "--max-redirs"
        | "--resolve"
        | "-T"
        | "--upload-file"
        | "--limit-rate"
        | "-U"
        | "--proxy-user"
        | "--interface"
        | "--dns-servers"
        | "--unix-socket"
        | "--abstract-unix-socket"
        | "-K"
        | "--config"
        | "--keepalive-time"
        | "--speed-limit"
        | "--speed-time"
        | "--ciphers"
        | "--tls-max"
        | "--pinnedpubkey"
        | "-r"
        | "--range"
        | "--stderr"
        | "--trace"
        | "--trace-ascii"
        | "-z"
        | "--time-cond"
        | "--aws-sigv4"
        | "--proto"
        | "--proto-default"
        | "--url-query"
        | "--variable"
        | "--connect-to"
        | "--happy-eyeballs-timeout-ms"
        | "--retry-delay"
        | "--retry-max-time"
        | "--max-filesize"
        | "-y"
        | "-Y" => (Ignored, true),
        "--ntlm" | "--digest" | "--negotiate" | "--basic" | "--anyauth" => (Quiet, false),
        "--http1.1" | "--http1.0" | "-0" => (Http11, false),
        "--http2" => (Http2, false),
        "--http3" | "--tlsv1.2" | "--tlsv1.3" => (Quiet, false),
        "-4" | "-6" | "-N" | "--no-buffer" | "--retry-all-errors" => (Quiet, false),
        "-o" | "--output" | "-w" | "--write-out" | "-x" | "--proxy" | "--max-time" | "-m"
        | "--connect-timeout" | "--retry" | "-c" | "--cookie-jar" | "-D" | "--dump-header" => {
            (Ignored, true)
        }
        "-O" | "--remote-name" => (Ignored, false),
        _ => return None,
    };
    Some(FlagSpec {
        meaning,
        takes_value,
    })
}

/// Separates the `--name=value` and `-Xvalue` forms: the flag's name, the
/// attached value and whether there was one.
fn split_flag(tok: &str) -> (&str, String, bool) {
    if tok.starts_with("--") {
        return match tok.split_once('=') {
            Some((k, v)) => (k, v.to_string(), true),
            None => (tok, String::new(), false),
        };
    }
    if tok.len() > 2 {
        if let Some(short) = tok.get(..2) {
            if flag(short).is_some_and(|spec| spec.takes_value) {
                return (short, tok[2..].to_string(), true);
            }
        }
    }
    (tok, String::new(), false)
}

/// Turns `-sSL` into `-s -S -L` when every letter is a boolean flag curl
/// knows, and returns `None` otherwise.
fn split_bundle(tok: &str) -> Option<Vec<String>> {
    if tok.starts_with("--") || tok.len() <= 2 || !all_boolean(&tok[1..]) {
        return None;
    }
    Some(tok[1..].chars().map(|c| format!("-{c}")).collect())
}

fn all_boolean(chars: &str) -> bool {
    chars
        .chars()
        .all(|c| flag(&format!("-{c}")).is_some_and(|spec| !spec.takes_value))
}

/// Handles the four spellings of `--data-urlencode`: `content`,
/// `=content`, `name=content` and `name@file`.
fn urlencode_data(v: &str, warnings: &mut Vec<String>) -> String {
    if let Some((name, file)) = v.split_once('@') {
        if !name.contains('=') {
            warnings.push(format!(
                "--data-urlencode {v} reads a file, which apic does not do here; paste its content"
            ));
            return format!("{name}={}", query_escape(&format!("@{file}")));
        }
    }
    if let Some(content) = v.strip_prefix('=') {
        return escape(content);
    }
    if let Some((name, content)) = v.split_once('=') {
        return format!("{name}={}", escape(content));
    }
    escape(v)
}

/// Percent-encodes a value, keeping `{{placeholders}}` for the runner.
fn escape(s: &str) -> String {
    query_escape(s)
        .replace("%7B%7B", "{{")
        .replace("%7D%7D", "}}")
}

/// Encodes a value for a query string: a space becomes `+`, and every byte
/// but the unreserved ones (letters, digits, `-`, `_`, `.`, `~`) becomes
/// `%XX`.
fn query_escape(s: &str) -> String {
    const UPPER_HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(s.len());
    for c in s.bytes() {
        if c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.' | b'~') {
            out.push(c as char);
        } else if c == b' ' {
            out.push('+');
        } else {
            out.push('%');
            out.push(UPPER_HEX[usize::from(c >> 4)] as char);
            out.push(UPPER_HEX[usize::from(c & 15)] as char);
        }
    }
    out
}

/// Reads a `-F` field: `name=value`, `name=@file[;type=..][;filename=..]`
/// or `name=<file` (the content of a file as text).
fn parse_form(v: &str, literal: bool) -> Option<Part> {
    let (name, rest) = v.split_once('=')?;
    if name.is_empty() {
        return None;
    }
    let mut p = Part {
        name: name.to_string(),
        ..Part::default()
    };
    if literal || (!rest.starts_with('@') && !rest.starts_with('<')) {
        p.value = rest.to_string();
        return Some(p);
    }
    let mut spec = rest.split(';');
    p.file = spec
        .next()
        .unwrap_or("")
        .trim_start_matches(['@', '<'])
        .to_string();
    for opt in spec {
        let (k, val) = opt.split_once('=').unwrap_or((opt, ""));
        match k.trim() {
            "type" => p.content_type = val.trim().to_string(),
            "filename" => p.filename = val.trim().to_string(),
            _ => {}
        }
    }
    if rest.starts_with('<') {
        // `<file` sends the file's content as a text field: same thing
        // from a request file's point of view, without a filename.
        p.filename = String::new();
    } else if p.filename.is_empty() {
        p.filename = base_name(&p.file.replace('\\', "/"));
    }
    Some(p)
}

/// The last element of a slash-separated path: trailing slashes are
/// dropped first, an empty path is `.` and one of only slashes is `/`.
fn base_name(path: &str) -> String {
    if path.is_empty() {
        return ".".to_string();
    }
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return "/".to_string();
    }
    match trimmed.rfind('/') {
        Some(i) => trimmed[i + 1..].to_string(),
        None => trimmed.to_string(),
    }
}

/// Splits a shell command line: whitespace separates words, single quotes
/// are literal, double quotes keep spaces and take backslash escapes,
/// `$'...'` takes C escapes, and a backslash before a newline joins lines.
fn tokenize(s: &str) -> Result<Vec<String>, Error> {
    let s = s.as_bytes();
    let mut out = Vec::new();
    let mut cur: Vec<u8> = Vec::new();
    let mut in_word = false;
    let mut i = 0;
    while i < s.len() {
        let c = s[i];
        if matches!(c, b' ' | b'\t' | b'\n' | b'\r') {
            if in_word {
                out.push(word(&mut cur));
                in_word = false;
            }
            i += 1;
            continue;
        }
        let Some(read) = quote_reader(s, i) else {
            cur.push(c);
            in_word = true;
            i += 1;
            continue;
        };
        let (last, contributed) = read(s, i, &mut cur)?;
        i = last + 1;
        in_word = in_word || contributed;
    }
    if in_word {
        out.push(word(&mut cur));
    }
    Ok(out)
}

/// Takes the word collected so far. Only ASCII bytes are ever dropped or
/// replaced, so the bytes of a word are text again.
fn word(cur: &mut Vec<u8>) -> String {
    match String::from_utf8(std::mem::take(cur)) {
        Ok(word) => word,
        Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
    }
}

/// Consumes one shell construct that starts at `s[i]`, writes what it
/// stands for to `cur`, and returns the index of the last byte it consumed
/// and whether it contributed to a word (a line continuation does not).
type QuoteRead = fn(s: &[u8], i: usize, cur: &mut Vec<u8>) -> Result<(usize, bool), Error>;

/// Picks the reader for the construct at `s[i]`, or `None` for a plain
/// byte.
fn quote_reader(s: &[u8], i: usize) -> Option<QuoteRead> {
    match s[i] {
        b'\\' => Some(read_escape),
        b'\'' => Some(read_single_quoted),
        b'"' => Some(read_double_quoted),
        b'$' if s.get(i + 1) == Some(&b'\'') => Some(read_ansi_quoted),
        _ => None,
    }
}

/// Handles a backslash outside quotes: a line continuation (a backslash
/// before a newline or CRLF) disappears, anything else is taken literally.
fn read_escape(s: &[u8], i: usize, cur: &mut Vec<u8>) -> Result<(usize, bool), Error> {
    if i + 1 >= s.len() {
        return Err(Error::new("trailing backslash"));
    }
    let i = i + 1;
    if s[i] == b'\n' {
        return Ok((i, false)); // line continuation
    }
    if s[i] == b'\r' && s.get(i + 1) == Some(&b'\n') {
        return Ok((i + 1, false));
    }
    cur.push(s[i]);
    Ok((i, true))
}

/// Takes everything up to the next single quote literally.
fn read_single_quoted(s: &[u8], i: usize, cur: &mut Vec<u8>) -> Result<(usize, bool), Error> {
    let Some(end) = s[i + 1..].iter().position(|&b| b == b'\'') else {
        return Err(Error::new("unterminated single quote"));
    };
    cur.extend_from_slice(&s[i + 1..i + 1 + end]);
    Ok((i + end + 1, true))
}

/// Honours the backslash escapes the shell keeps inside double quotes: a
/// backslash before `"`, another backslash, `$`, a backtick or a newline.
fn read_double_quoted(s: &[u8], i: usize, cur: &mut Vec<u8>) -> Result<(usize, bool), Error> {
    let mut i = i + 1;
    while i < s.len() {
        if s[i] == b'"' {
            return Ok((i, true));
        }
        if s[i] == b'\\' && i + 1 < s.len() && b"\"\\$`\n".contains(&s[i + 1]) {
            i += 1;
            if s[i] == b'\n' {
                i += 1;
                continue;
            }
        }
        cur.push(s[i]);
        i += 1;
    }
    Err(Error::new("unterminated double quote"))
}

/// Reads bash's `$'...'` form, with its `n`, `t` and `r` escapes.
fn read_ansi_quoted(s: &[u8], i: usize, cur: &mut Vec<u8>) -> Result<(usize, bool), Error> {
    let mut i = i + 2;
    while i < s.len() {
        if s[i] == b'\'' {
            return Ok((i, true));
        }
        if s[i] == b'\\' && i + 1 < s.len() {
            i += 1;
            cur.push(ansi_escape(s[i]));
        } else {
            cur.push(s[i]);
        }
        i += 1;
    }
    Err(Error::new("unterminated $'...' quote"))
}

fn ansi_escape(c: u8) -> u8 {
    match c {
        b'n' => b'\n',
        b't' => b'\t',
        b'r' => b'\r',
        _ => c,
    }
}

/// Derives a request name from the method and path when `--name` is not
/// given: `POST /todos/42` becomes `post-todos-42`.
pub fn name(r: &Request) -> String {
    let mut text = r.method.clone().into_bytes();
    text.push(b' ');
    text.extend(url_path(&r.url).unwrap_or_default());
    // Every run of anything but letters and digits becomes one dash.
    let mut name = String::new();
    for c in text {
        if c.is_ascii_alphanumeric() {
            name.push(c.to_ascii_lowercase() as char);
        } else if !name.ends_with('-') {
            name.push('-');
        }
    }
    let name = name.trim_matches('-');
    if name.is_empty() {
        return "request".to_string();
    }
    name.to_string()
}

/// Reports whether the inline body would be misread by the `.http`
/// parser: a line starting with `###` opens a new request block and a body
/// starting with `< ` refers to a file. Such a body belongs in a side file
/// ([`Request::body_file`]).
pub fn splits_block(body: &str) -> bool {
    let t = body.trim();
    if t.starts_with("< ") || t.starts_with("<@ ") {
        return true;
    }
    body.split('\n').any(|line| line.trim().starts_with("###"))
}

/// Writes the request as a `###` block. `base_url`, when it prefixes the
/// URL, is replaced by `{{baseUrl}}`.
pub fn render(r: &Request, name: &str, base_url: &str) -> String {
    let mut b = String::new();
    let mut raw_url = r.url.clone();
    if !base_url.is_empty() {
        let base = base_url.trim_end_matches('/');
        if let Some(rest) = raw_url.strip_prefix(base) {
            if rest.is_empty() || rest.starts_with('/') || rest.starts_with('?') {
                raw_url = format!("{{{{baseUrl}}}}{rest}");
            }
        }
    }
    b.push_str(&format!("### {} {}\n", r.method, title_path(&raw_url)));
    b.push_str(&format!("# @name {name}\n"));
    if !r.user.is_empty() {
        let (u, p) = r.user.split_once(':').unwrap_or((r.user.as_str(), ""));
        b.push_str(&format!("# @auth basic {} {}\n", auth_arg(u), auth_arg(p)));
    }
    if r.insecure {
        b.push_str("# curl was run with --insecure; run this with `apic --insecure` or set tls.verifyHost in apic.yaml\n");
    }
    b.push_str("# @assert status == 200\n");
    let mut line = format!("{} {}", r.method, one_line(&raw_url));
    if !r.http_version.is_empty() {
        line.push(' ');
        line.push_str(&r.http_version);
    }
    b.push_str(&line);
    b.push('\n');
    for h in &r.headers {
        b.push_str(&format!("{}: {}\n", one_line(&h.name), one_line(&h.value)));
    }
    if !r.parts.is_empty() {
        b.push('\n');
        for p in &r.parts {
            b.push_str("--WebAppBoundary\n");
            if !p.file.is_empty() && !p.filename.is_empty() {
                b.push_str(&format!(
                    "Content-Disposition: form-data; name={}; filename={}\n",
                    quote_param(&p.name),
                    quote_param(&p.filename)
                ));
            } else {
                b.push_str(&format!(
                    "Content-Disposition: form-data; name={}\n",
                    quote_param(&p.name)
                ));
            }
            if !p.content_type.is_empty() {
                b.push_str(&format!("Content-Type: {}\n", one_line(&p.content_type)));
            }
            if p.file.is_empty() {
                b.push_str(&format!("\n{}\n", p.value));
            } else {
                b.push_str(&format!("\n< {}\n", relative_file(&p.file)));
            }
        }
        b.push_str("--WebAppBoundary--\n");
    } else if !r.body_file.is_empty() {
        b.push_str(&format!("\n< {}\n", relative_file(&r.body_file)));
    } else if !r.body.is_empty() {
        b.push_str(&format!("\n{}\n", r.body));
    }
    b
}

/// Keeps a `< file` reference relative to the `.http` file, the only place
/// the runner reads it from.
fn relative_file(p: &str) -> String {
    let p = p.replace('\\', "/");
    if p.starts_with("./") || p.starts_with("../") || p.starts_with('/') {
        return p;
    }
    format!("./{p}")
}

/// The path of the URL for the block's title, or the URL itself when it
/// has none or cannot be read.
fn title_path(raw_url: &str) -> String {
    match url_path(&raw_url.replace("{{baseUrl}}", "http://baseurl")) {
        Some(path) if !path.is_empty() => String::from_utf8_lossy(&path).into_owned(),
        _ => one_line(raw_url),
    }
}

fn auth_arg(s: &str) -> String {
    let s = one_line(s);
    if s.is_empty() {
        return "\"\"".to_string();
    }
    if s.contains([' ', '\t', '"']) {
        return format!("\"{}\"", s.replace('"', "\\\""));
    }
    s
}

fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
#[path = "curl_tests.rs"]
mod tests;
