use std::hash::{Hash, Hasher};
use std::ops::Deref;
use std::sync::Arc;

/// What a request knows about the file it was read from: the path it was
/// parsed under and the file-level variables. Shared by the [`File`] and
/// every [`Request`] in it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileInfo {
    /// The path given to the parser; used for diagnostics and ids only.
    pub path: String,
    /// The `@name = value` variables, in file order.
    pub vars: Vec<Var>,
}

/// A parsed `.http` file. It dereferences to its [`FileInfo`], so
/// `file.path` and `file.vars` read as fields.
#[derive(Debug, Clone, Default)]
pub struct File {
    /// The path and variables, shared with every request of the file.
    pub info: Arc<FileInfo>,
    /// The requests, in file order.
    pub requests: Vec<Arc<Request>>,
}

impl Deref for File {
    type Target = FileInfo;

    fn deref(&self) -> &FileInfo {
        &self.info
    }
}

/// A file-level variable declared as `@name = value`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Var {
    pub name: String,
    pub value: String,
    pub line: usize,
}

/// A `# @key value` comment placed before a request line. `column` is the
/// 1-based byte column where `value` starts on the line, or where `@key`
/// starts when there is no value.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Directive {
    pub key: String,
    pub value: String,
    pub line: usize,
    pub column: usize,
}

/// Declares that a value selected from the response should be stored under
/// `name` for later requests: `# @capture token = body.$.access_token`.
/// `column` is where `selector` starts on the line.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Capture {
    pub name: String,
    pub selector: String,
    pub line: usize,
    pub column: usize,
}

/// A raw assertion expression: `# @assert status == 200`. `column` is
/// where `expr` starts on the line.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Assert {
    pub expr: String,
    pub line: usize,
    pub column: usize,
}

/// A raw, un-templated request header.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Header {
    pub name: String,
    pub value: String,
}

/// One request block within a file.
#[derive(Debug, Clone, Default)]
pub struct Request {
    /// The file the request was read from: its path and variables.
    pub file: Arc<FileInfo>,
    /// 1-based position within the file.
    pub index: usize,
    /// From `# @name`; may be empty.
    pub name: String,
    /// Free text after `###`.
    pub title: String,
    /// From `# @description`, else the title.
    pub description: String,
    pub directives: Vec<Directive>,
    pub captures: Vec<Capture>,
    pub asserts: Vec<Assert>,
    pub method: String,
    /// Raw template.
    pub url: String,
    pub http_version: String,
    pub headers: Vec<Header>,
    /// Raw template, empty when there is none.
    pub body: String,
    /// Line of the first body line, 0 when there is no body.
    pub body_line: usize,
    /// Set when the body is `< ./file`.
    pub body_file: String,
    /// `<@ ./file`: substitute `{{vars}}` inside the file too.
    pub body_file_templated: bool,
    /// Line of the `< ./file` reference, for diagnostics.
    pub body_file_line: usize,
    /// Column where the path starts on that line.
    pub body_file_column: usize,
    /// `>> ./file` after the body: where the response body goes.
    pub save_to: Option<SaveTo>,
    /// Line number of the request line.
    pub line: usize,
}

/// A `>> ./file` (create) or `>>! ./file` (overwrite) line after the body:
/// the response body is written there, relative to the `.http` file.
/// `column` is where the path starts on the line.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SaveTo {
    pub path: String,
    pub overwrite: bool,
    pub line: usize,
    pub column: usize,
}

/// A `# @ref` or `# @forceRef` directive: another request to run before
/// this one. A ref runs only when this request is missing a variable; a
/// forceRef runs every time.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Ref {
    pub id: String,
    pub force: bool,
    pub line: usize,
    /// Column where the target starts.
    pub column: usize,
}

impl Request {
    /// Returns the line and the `[col, end)` columns of the value of the
    /// first directive with the given key, or zeros when there is none.
    pub fn directive_span(&self, key: &str) -> (usize, usize, usize) {
        self.directives
            .iter()
            .find(|d| d.key == key)
            .map_or((0, 0, 0), |d| (d.line, d.column, d.column + d.value.len()))
    }

    /// Returns the stable identifier used on the command line: the
    /// request's name when it has one, else `<file>#<index>`.
    pub fn id(&self) -> String {
        if !self.name.is_empty() {
            return self.name.clone();
        }
        format!("{}#{}", self.file.path, self.index)
    }

    /// Returns every `# @step` phrase declared on the request.
    pub fn steps(&self) -> Vec<String> {
        self.directives
            .iter()
            .filter(|d| d.key == "step")
            .map(|d| d.value.clone())
            .collect()
    }

    /// Returns every `# @ref` and `# @forceRef` declared on the request,
    /// in order.
    pub fn refs(&self) -> Vec<Ref> {
        self.directives
            .iter()
            .filter(|d| d.key == "ref" || d.key == "forceRef")
            .map(|d| Ref {
                id: d.value.clone(),
                force: d.key == "forceRef",
                line: d.line,
                column: d.column,
            })
            .collect()
    }

    /// Returns the value of the first directive with the given key.
    pub fn directive(&self, key: &str) -> Option<&str> {
        self.directives
            .iter()
            .find(|d| d.key == key)
            .map(|d| d.value.as_str())
    }
}

/// A request as a map key or set member: two keys are equal when they are
/// the same parsed request (the same allocation), whatever their fields
/// hold. The key keeps the request alive, so an address is never reused
/// while a map still holds it.
#[derive(Debug, Clone)]
pub struct RequestKey(pub Arc<Request>);

impl RequestKey {
    /// The key for a request.
    pub fn new(request: &Arc<Request>) -> Self {
        RequestKey(Arc::clone(request))
    }

    /// The request the key stands for.
    pub fn request(&self) -> &Arc<Request> {
        &self.0
    }
}

impl From<&Arc<Request>> for RequestKey {
    fn from(request: &Arc<Request>) -> Self {
        RequestKey::new(request)
    }
}

impl From<Arc<Request>> for RequestKey {
    fn from(request: Arc<Request>) -> Self {
        RequestKey(request)
    }
}

impl PartialEq for RequestKey {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for RequestKey {}

impl Hash for RequestKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        Arc::as_ptr(&self.0).hash(state);
    }
}
