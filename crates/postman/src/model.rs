//! The model of a Postman collection, hand-written from the collection
//! format, on purpose: it reads only what the generator needs and
//! tolerates the two shapes (string or object) the format allows for
//! several fields.
//!
//! Decoding is forgiving the way it has always been: keys match whatever
//! their capitalisation, a key the model does not know is ignored, a
//! `null` leaves a field unset, and a value of the wrong type is an error
//! that names the field: `json: cannot unmarshal number into Go struct
//! field Info.info.name of type string`. The first such value in the file
//! is the one reported.

use crate::json::{number_text, Json};

/// A value of the wrong type, and where it was found.
#[derive(Clone, Debug)]
struct TypeError {
    /// What the file holds: `number`, `string`, `object`...
    value: String,
    /// What was expected: `string`, `[]postman.Item`...
    typ: String,
    /// The type that holds the field.
    holder: &'static str,
    /// The path of keys to the field.
    field: String,
}

impl TypeError {
    fn message(&self) -> String {
        if self.holder.is_empty() && self.field.is_empty() {
            return format!(
                "json: cannot unmarshal {} into Go value of type {}",
                self.value, self.typ
            );
        }
        format!(
            "json: cannot unmarshal {} into Go struct field {}.{} of type {}",
            self.value, self.holder, self.field, self.typ
        )
    }
}

/// The state of one pass over a value: where it is, and the first value
/// of the wrong type it met. A field that reads itself (a request, a URL,
/// a script's lines) starts a pass of its own and hands its error to the
/// pass around it, which says where the field is.
#[derive(Default)]
struct Decoder {
    err: Option<TypeError>,
    holder: &'static str,
    stack: Vec<&'static str>,
}

impl Decoder {
    fn save(&mut self, err: TypeError) {
        if self.err.is_none() {
            self.err = Some(err);
        }
    }

    /// Records a value of the wrong type at the current field.
    fn fail(&mut self, value: &Json, typ: &str) {
        self.save(TypeError {
            value: value.kind().to_string(),
            typ: typ.to_string(),
            holder: self.holder,
            field: self.stack.join("."),
        });
    }

    /// Takes the error of a pass that ran inside the current field.
    fn adopt(&mut self, mut inner: TypeError) {
        if !self.holder.is_empty() || !self.stack.is_empty() {
            inner.holder = self.holder;
            let mut path: Vec<&str> = self.stack.clone();
            if !inner.field.is_empty() {
                path.push(&inner.field);
            }
            inner.field = path.join(".");
        }
        self.save(inner);
    }

    /// Runs `f` with the position moved to a field of `holder`.
    fn field(&mut self, holder: &'static str, name: &'static str, f: impl FnOnce(&mut Decoder)) {
        let outer = self.holder;
        self.holder = holder;
        self.stack.push(name);
        f(self);
        self.stack.pop();
        self.holder = outer;
    }
}

/// A type that reads itself from JSON.
trait Decode: Default {
    /// The type as messages name it.
    fn type_name() -> String;
    fn decode(&mut self, d: &mut Decoder, v: &Json);
}

impl Decode for String {
    fn type_name() -> String {
        "string".to_string()
    }

    fn decode(&mut self, d: &mut Decoder, v: &Json) {
        match v {
            Json::Null => {}
            Json::Str(s) => self.clone_from(s),
            other => d.fail(other, &Self::type_name()),
        }
    }
}

impl Decode for bool {
    fn type_name() -> String {
        "bool".to_string()
    }

    fn decode(&mut self, d: &mut Decoder, v: &Json) {
        match v {
            Json::Null => {}
            Json::Bool(b) => *self = *b,
            other => d.fail(other, &Self::type_name()),
        }
    }
}

impl<T: Decode> Decode for Vec<T> {
    fn type_name() -> String {
        format!("[]{}", T::type_name())
    }

    fn decode(&mut self, d: &mut Decoder, v: &Json) {
        match v {
            Json::Null => self.clear(),
            Json::Array(items) => {
                self.clear();
                for item in items {
                    let mut element = T::default();
                    element.decode(d, item);
                    self.push(element);
                }
            }
            other => d.fail(other, &Self::type_name()),
        }
    }
}

impl<T: Decode> Decode for Option<T> {
    fn type_name() -> String {
        T::type_name()
    }

    fn decode(&mut self, d: &mut Decoder, v: &Json) {
        match v {
            Json::Null => *self = None,
            other => self.get_or_insert_with(T::default).decode(d, other),
        }
    }
}

/// Finds the field a key names: its exact name, else the same letters in
/// another case.
fn field_index(names: &[&str], key: &str) -> Option<usize> {
    names
        .iter()
        .position(|n| *n == key)
        .or_else(|| names.iter().position(|n| n.eq_ignore_ascii_case(key)))
}

/// Implements [`Decode`] for a struct: its name in messages, then each
/// key with the field it fills and the function that reads it.
macro_rules! decode_struct {
    ($ty:ident, $holder:literal, $type_name:literal, { $($key:literal => $field:ident: $read:path),* $(,)? }) => {
        impl Decode for $ty {
            fn type_name() -> String {
                $type_name.to_string()
            }

            fn decode(&mut self, d: &mut Decoder, v: &Json) {
                const KEYS: &[&str] = &[$($key),*];
                match v {
                    Json::Null => {}
                    Json::Object(members) => {
                        for (key, value) in members {
                            let Some(index) = field_index(KEYS, key) else {
                                continue;
                            };
                            let mut at = 0;
                            $(
                                if index == at {
                                    d.field($holder, $key, |d| $read(&mut self.$field, d, value));
                                }
                                at += 1;
                            )*
                            let _ = at;
                        }
                    }
                    other => d.fail(other, &Self::type_name()),
                }
            }
        }
    };
}

/// A Postman collection file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Collection {
    pub info: Info,
    /// `None` when the file has no item list at all, which a v1 export
    /// does not.
    pub item: Option<Vec<Item>>,
    pub variable: Vec<Variable>,
    pub auth: Option<Auth>,
    pub event: Vec<Event>,
}

decode_struct!(Collection, "Collection", "postman.Collection", {
    "info" => info: Decode::decode,
    "item" => item: Decode::decode,
    "variable" => variable: Decode::decode,
    "auth" => auth: Decode::decode,
    "event" => event: Decode::decode,
});

impl Collection {
    /// Decodes a collection. The error is the message to show:
    /// `json: cannot unmarshal ...`, or `invalid JSON` for a file that is
    /// not JSON at all.
    pub fn from_json(data: &[u8]) -> Result<Collection, String> {
        from_json(data)
    }
}

fn from_json<T: Decode>(data: &[u8]) -> Result<T, String> {
    let json = Json::parse(data).ok_or_else(|| "invalid JSON".to_string())?;
    let mut d = Decoder::default();
    let mut out = T::default();
    out.decode(&mut d, &json);
    match d.err {
        Some(err) => Err(err.message()),
        None => Ok(out),
    }
}

/// Holds the collection's name and schema URL.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Info {
    pub name: String,
    pub schema: String,
    /// Text, whichever shape the file gives it; see [`Variable::value`].
    pub description: String,
}

decode_struct!(Info, "Info", "postman.Info", {
    "name" => name: Decode::decode,
    "schema" => schema: Decode::decode,
    "description" => description: flex_text,
});

/// A request, or a folder when `request` is `None`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Item {
    pub name: String,
    pub description: String,
    pub item: Vec<Item>,
    pub request: Option<Request>,
    pub event: Vec<Event>,
    pub auth: Option<Auth>,
}

decode_struct!(Item, "Item", "postman.Item", {
    "name" => name: Decode::decode,
    "description" => description: flex_text,
    "item" => item: Decode::decode,
    "request" => request: Decode::decode,
    "event" => event: Decode::decode,
    "auth" => auth: Decode::decode,
});

impl Item {
    /// Reports whether the item groups other items.
    pub fn is_folder(&self) -> bool {
        self.request.is_none()
    }
}

/// The request of an item. The file may give it in the short form, a bare
/// URL string, which is a GET of that URL.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Request {
    pub method: String,
    pub header: Vec<Header>,
    pub url: Url,
    pub body: Option<Body>,
    pub auth: Option<Auth>,
    pub description: String,
}

/// A request written out as an object.
#[derive(Default)]
struct PlainRequest(Request);

impl Decode for PlainRequest {
    fn type_name() -> String {
        "postman.plain".to_string()
    }

    fn decode(&mut self, d: &mut Decoder, v: &Json) {
        const KEYS: &[&str] = &["method", "header", "url", "body", "auth", "description"];
        let r = &mut self.0;
        match v {
            Json::Null => {}
            Json::Object(members) => {
                for (key, value) in members {
                    match field_index(KEYS, key) {
                        Some(0) => d.field("plain", "method", |d| r.method.decode(d, value)),
                        Some(1) => d.field("plain", "header", |d| r.header.decode(d, value)),
                        Some(2) => d.field("plain", "url", |d| r.url.decode(d, value)),
                        Some(3) => d.field("plain", "body", |d| r.body.decode(d, value)),
                        Some(4) => d.field("plain", "auth", |d| r.auth.decode(d, value)),
                        Some(5) => d.field("plain", "description", |d| {
                            flex_text(&mut r.description, d, value);
                        }),
                        _ => {}
                    }
                }
            }
            other => d.fail(other, &Self::type_name()),
        }
    }
}

impl Decode for Request {
    fn type_name() -> String {
        "postman.Request".to_string()
    }

    fn decode(&mut self, d: &mut Decoder, v: &Json) {
        if let Json::Str(raw) = v {
            *self = Request {
                method: "GET".to_string(),
                url: Url {
                    raw: raw.clone(),
                    ..Url::default()
                },
                ..Request::default()
            };
            return;
        }
        let mut inner = Decoder::default();
        let mut plain = PlainRequest::default();
        plain.decode(&mut inner, v);
        match inner.err {
            Some(err) => d.adopt(err),
            None => *self = plain.0,
        }
    }
}

/// One request header; disabled ones are kept as comments.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Header {
    pub key: String,
    pub value: String,
    pub disabled: bool,
}

decode_struct!(Header, "Header", "postman.Header", {
    "key" => key: Decode::decode,
    "value" => value: Decode::decode,
    "disabled" => disabled: Decode::decode,
});

/// The request URL: the raw text, plus the parsed parts Postman keeps
/// alongside it. Only `raw`, `query` and `variable` are read. The file
/// may give a bare string instead, which is the raw text.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Url {
    pub raw: String,
    pub query: Vec<Query>,
    pub variable: Vec<Variable>,
}

/// A URL written out as an object.
#[derive(Default)]
struct PlainUrl(Url);

impl Decode for PlainUrl {
    fn type_name() -> String {
        "postman.plain".to_string()
    }

    fn decode(&mut self, d: &mut Decoder, v: &Json) {
        const KEYS: &[&str] = &["raw", "query", "variable"];
        let u = &mut self.0;
        match v {
            Json::Null => {}
            Json::Object(members) => {
                for (key, value) in members {
                    match field_index(KEYS, key) {
                        Some(0) => d.field("plain", "raw", |d| u.raw.decode(d, value)),
                        Some(1) => d.field("plain", "query", |d| u.query.decode(d, value)),
                        Some(2) => d.field("plain", "variable", |d| u.variable.decode(d, value)),
                        _ => {}
                    }
                }
            }
            other => d.fail(other, &Self::type_name()),
        }
    }
}

impl Decode for Url {
    fn type_name() -> String {
        "postman.URL".to_string()
    }

    fn decode(&mut self, d: &mut Decoder, v: &Json) {
        if let Json::Str(raw) = v {
            *self = Url {
                raw: raw.clone(),
                ..Url::default()
            };
            return;
        }
        let mut inner = Decoder::default();
        let mut plain = PlainUrl::default();
        plain.decode(&mut inner, v);
        match inner.err {
            Some(err) => d.adopt(err),
            None => *self = plain.0,
        }
    }
}

/// One query parameter of a URL object.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Query {
    pub key: String,
    pub value: String,
    pub disabled: bool,
}

decode_struct!(Query, "Query", "postman.Query", {
    "key" => key: Decode::decode,
    "value" => value: Decode::decode,
    "disabled" => disabled: Decode::decode,
});

/// A collection, URL (path) or environment variable.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Variable {
    pub key: String,
    /// Text, whichever shape the file gives it: a string as it is, a
    /// number or a boolean as written, an object's `content` or `src`, the
    /// first element of a list, and anything else as its JSON text.
    pub value: String,
    /// `secret` in environments marks a masked value.
    pub r#type: String,
    pub disabled: bool,
    /// Environment files use `enabled` instead of `disabled`.
    pub enabled: Option<bool>,
}

decode_struct!(Variable, "Variable", "postman.Variable", {
    "key" => key: Decode::decode,
    "value" => value: flex_text,
    "type" => r#type: Decode::decode,
    "disabled" => disabled: Decode::decode,
    "enabled" => enabled: Decode::decode,
});

impl Variable {
    /// Reports whether the variable is in effect.
    pub fn active(&self) -> bool {
        match self.enabled {
            Some(enabled) => enabled,
            None => !self.disabled,
        }
    }
}

/// The request body in one of Postman's modes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Body {
    /// `raw`, `urlencoded`, `formdata`, `file` or `graphql`.
    pub mode: String,
    pub raw: String,
    pub options: Option<BodyOptions>,
    pub urlencoded: Vec<FormField>,
    pub formdata: Vec<FormField>,
    pub file: Option<BodyFile>,
    pub graphql: Option<GraphQL>,
    pub disabled: bool,
}

decode_struct!(Body, "Body", "postman.Body", {
    "mode" => mode: Decode::decode,
    "raw" => raw: Decode::decode,
    "options" => options: Decode::decode,
    "urlencoded" => urlencoded: Decode::decode,
    "formdata" => formdata: Decode::decode,
    "file" => file: Decode::decode,
    "graphql" => graphql: Decode::decode,
    "disabled" => disabled: Decode::decode,
});

/// Carries the raw body's language, which decides the content type when
/// no header says.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BodyOptions {
    pub raw: RawOptions,
}

decode_struct!(BodyOptions, "BodyOptions", "postman.BodyOptions", {
    "raw" => raw: Decode::decode,
});

/// The options of a raw body.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RawOptions {
    /// `json`, `xml`, `html`, `javascript` or `text`.
    pub language: String,
}

decode_struct!(RawOptions, "", "struct { Language string \"json:\\\"language\\\"\" }", {
    "language" => language: Decode::decode,
});

/// One urlencoded or form-data field.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FormField {
    pub key: String,
    pub value: String,
    /// `text` or `file`.
    pub r#type: String,
    /// A path, or the first of a list of paths, for a file field.
    pub src: String,
    pub content_type: String,
    pub disabled: bool,
}

decode_struct!(FormField, "FormField", "postman.FormField", {
    "key" => key: Decode::decode,
    "value" => value: Decode::decode,
    "type" => r#type: Decode::decode,
    "src" => src: flex_text,
    "contentType" => content_type: Decode::decode,
    "disabled" => disabled: Decode::decode,
});

/// A whole-body file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BodyFile {
    pub src: String,
}

decode_struct!(BodyFile, "BodyFile", "postman.BodyFile", {
    "src" => src: Decode::decode,
});

/// A GraphQL body: the query and its variables as JSON text.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GraphQL {
    pub query: String,
    pub variables: String,
}

decode_struct!(GraphQL, "GraphQL", "postman.GraphQL", {
    "query" => query: Decode::decode,
    "variables" => variables: Decode::decode,
});

/// An auth block on the collection, a folder or a request.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Auth {
    pub r#type: String,
    pub bearer: Vec<AuthParam>,
    pub basic: Vec<AuthParam>,
    pub apikey: Vec<AuthParam>,
    pub awsv4: Vec<AuthParam>,
    pub oauth2: Vec<AuthParam>,
    pub digest: Vec<AuthParam>,
}

decode_struct!(Auth, "Auth", "postman.Auth", {
    "type" => r#type: Decode::decode,
    "bearer" => bearer: Decode::decode,
    "basic" => basic: Decode::decode,
    "apikey" => apikey: Decode::decode,
    "awsv4" => awsv4: Decode::decode,
    "oauth2" => oauth2: Decode::decode,
    "digest" => digest: Decode::decode,
});

/// One key/value of an auth block.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AuthParam {
    pub key: String,
    pub value: String,
}

decode_struct!(AuthParam, "AuthParam", "postman.AuthParam", {
    "key" => key: Decode::decode,
    "value" => value: flex_text,
});

/// The value of a key in an auth block's parameters; empty when absent.
pub(crate) fn param<'a>(ps: &'a [AuthParam], key: &str) -> &'a str {
    ps.iter()
        .find(|p| p.key == key)
        .map_or("", |p| p.value.as_str())
}

/// A script hook: `listen` is `prerequest` or `test`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Event {
    pub listen: String,
    pub script: Script,
}

decode_struct!(Event, "Event", "postman.Event", {
    "listen" => listen: Decode::decode,
    "script" => script: Decode::decode,
});

/// Holds the lines of a script. The file gives a list of lines, or one
/// string, which is split at its line breaks.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Script {
    pub exec: Vec<String>,
}

decode_struct!(Script, "Script", "postman.Script", {
    "exec" => exec: flex_lines,
});

/// Reads a string that the format also allows as an object
/// (`{"content": "..."}` for descriptions), a number, a boolean or a list
/// (`src` of a file field). Anything else becomes its JSON text.
fn flex_text(out: &mut String, d: &mut Decoder, v: &Json) {
    match text_of(v) {
        Ok(text) => *out = text,
        // A number too large to be one.
        Err(number) => d.adopt(TypeError {
            value: format!("number {number}"),
            typ: "float64".to_string(),
            holder: "",
            field: String::new(),
        }),
    }
}

fn text_of(v: &Json) -> Result<String, String> {
    // A number anywhere in the value has to be one, used or not.
    let marshalled = v.marshal()?;
    Ok(match v {
        Json::Null => String::new(),
        Json::Str(s) => s.clone(),
        Json::Bool(b) => b.to_string(),
        Json::Number(n) => number_text(n)?,
        Json::Object(_) => match (v.member("content"), v.member("src")) {
            (Some(Json::Str(content)), _) => content.clone(),
            (_, Some(Json::Str(src))) => src.clone(),
            _ => marshalled,
        },
        Json::Array(items) => match items.first() {
            Some(first) => text_of(first)?,
            None => String::new(),
        },
    })
}

/// Reads a script body: a list of lines, or one string.
fn flex_lines(out: &mut Vec<String>, d: &mut Decoder, v: &Json) {
    let mut lines: Vec<String> = Vec::new();
    let mut attempt = Decoder::default();
    lines.decode(&mut attempt, v);
    if attempt.err.is_none() {
        *out = lines;
        return;
    }
    let mut attempt = Decoder::default();
    let mut one = String::new();
    one.decode(&mut attempt, v);
    match attempt.err {
        Some(err) => d.adopt(err),
        None => *out = one.split('\n').map(str::to_string).collect(),
    }
}

/// A Postman environment export.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Environment {
    pub name: String,
    /// `None` when the file has no `values` list, which an environment
    /// export always has.
    pub values: Option<Vec<Variable>>,
    /// The file's `_postman_variable_scope`.
    pub scope: String,
}

decode_struct!(Environment, "Environment", "postman.Environment", {
    "name" => name: Decode::decode,
    "values" => values: Decode::decode,
    "_postman_variable_scope" => scope: Decode::decode,
});

impl Environment {
    /// Decodes an environment export; see [`Collection::from_json`].
    pub fn from_json(data: &[u8]) -> Result<Environment, String> {
        from_json(data)
    }
}

/// Whether a key is present (whatever its value) under a name, in any
/// capitalisation.
fn has_key(v: &Json, name: &str) -> bool {
    match v {
        Json::Object(members) => members.iter().any(|(k, _)| k.eq_ignore_ascii_case(name)),
        _ => false,
    }
}

/// The value under a name: the last member spelt exactly so, else the
/// last in another capitalisation.
fn lookup<'a>(v: &'a Json, name: &str) -> Option<&'a Json> {
    let Json::Object(members) = v else {
        return None;
    };
    // Members are read in order, each filling the same field.
    members
        .iter()
        .rev()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v)
}

/// A string field of a probe: `Some("")` when absent or null, `None` when
/// it holds anything but a string.
fn probe_string(v: &Json, name: &str) -> Option<String> {
    match lookup(v, name) {
        None | Some(Json::Null) => Some(String::new()),
        Some(Json::Str(s)) => Some(s.clone()),
        Some(_) => None,
    }
}

/// Reports whether JSON data is a Postman collection (any version), for
/// the import command's dispatch.
pub fn looks_like_collection(data: &[u8]) -> bool {
    let Some(root) = Json::parse(data) else {
        return false;
    };
    let mut schema = String::new();
    match &root {
        Json::Null => return false,
        Json::Object(_) => {}
        _ => return false,
    }
    match lookup(&root, "info") {
        None | Some(Json::Null) => {}
        Some(info @ Json::Object(_)) => match probe_string(info, "schema") {
            Some(s) => schema = s,
            None => return false,
        },
        Some(_) => return false,
    }
    let has_info = matches!(lookup(&root, "info"), Some(Json::Object(_)));
    if has_info && schema.contains("getpostman.com") {
        return true;
    }
    has_key(&root, "item") || has_key(&root, "order")
}

/// Reports whether a file is a Postman environment.
pub fn looks_like_environment(data: &[u8]) -> bool {
    let Some(root) = Json::parse(data) else {
        return false;
    };
    if !matches!(root, Json::Object(_)) {
        return false;
    }
    let Some(scope) = probe_string(&root, "_postman_variable_scope") else {
        return false;
    };
    scope == "environment" || (has_key(&root, "values") && scope.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn err(json: &str) -> String {
        Collection::from_json(json.as_bytes()).unwrap_err()
    }

    #[test]
    fn names_the_first_field_of_the_wrong_type() {
        for (json, want) in [
            (
                r#"{"info": {"name": 5}, "item": [{"name": 6}]}"#,
                "json: cannot unmarshal number into Go struct field Info.info.name of type string",
            ),
            (
                r#"{"item": [{"name": 6}], "info": {"name": 5}}"#,
                "json: cannot unmarshal number into Go struct field Item.item.name of type string",
            ),
            (
                r#"{"item": {}}"#,
                "json: cannot unmarshal object into Go struct field Collection.item of type []postman.Item",
            ),
            (
                r#"{"item": [1]}"#,
                "json: cannot unmarshal number into Go struct field Collection.item of type postman.Item",
            ),
            (
                r#"{"item": [{"item": [{"name": [1]}]}]}"#,
                "json: cannot unmarshal array into Go struct field Item.item.item.name of type string",
            ),
            (
                r#"{"item": [{"request": 5}]}"#,
                "json: cannot unmarshal number into Go struct field Item.item.request of type postman.plain",
            ),
            (
                r#"{"item": [{"request": {"method": 5}}]}"#,
                "json: cannot unmarshal number into Go struct field Item.item.request.method of type string",
            ),
            (
                r#"{"item": [{"request": {"url": {"query": "q"}}}]}"#,
                "json: cannot unmarshal string into Go struct field Item.item.request.url.query of type []postman.Query",
            ),
            (
                r#"{"item": [{"request": {"body": {"options": {"raw": {"language": 5}}}}}]}"#,
                "json: cannot unmarshal number into Go struct field Item.item.request.body.options.raw.language of type string",
            ),
            (
                r#"{"item": [{"event": [{"script": {"exec": ["a", 5]}}]}]}"#,
                "json: cannot unmarshal array into Go struct field Script.item.event.script.exec of type string",
            ),
            (
                r#"{"variable": [{"enabled": 1}]}"#,
                "json: cannot unmarshal number into Go struct field Variable.variable.enabled of type bool",
            ),
            (
                r#"{"auth": []}"#,
                "json: cannot unmarshal array into Go struct field Collection.auth of type postman.Auth",
            ),
            (
                r#"{"variable": [{"value": 1e999}]}"#,
                "json: cannot unmarshal number 1e999 into Go struct field Variable.variable.value of type float64",
            ),
            ("[]", "json: cannot unmarshal array into Go value of type postman.Collection"),
        ] {
            assert_eq!(err(json), want, "{json}");
        }
    }

    #[test]
    fn reads_the_shapes_the_format_allows() {
        let c = Collection::from_json(
            br#"{"INFO": {"Name": "n", "description": {"content": "d", "type": "text/plain"}},
              "item": [
                {"name": "short", "request": "https://x/a"},
                {"name": "long", "request": {"url": "https://x/b", "method": null,
                  "body": {"mode": "formdata", "formdata": [{"key": "f", "src": ["a.txt", "b.txt"]}]}},
                  "event": [{"listen": "test", "script": {"exec": "one\ntwo"}},
                            {"listen": "test", "script": {"exec": ["x", null]}}]},
                {"name": "folder", "request": null, "item": []}
              ],
              "variable": [{"key": "n", "value": 20}, {"key": "b", "value": true},
                           {"key": "o", "value": {"b": 1.50, "a": "<"}}, {"key": "e", "value": []},
                           {"key": "off", "value": null, "enabled": false}]}"#,
        )
        .unwrap();
        assert_eq!(c.info.name, "n");
        assert_eq!(c.info.description, "d");
        let items = c.item.unwrap();
        let short = items[0].request.as_ref().unwrap();
        assert_eq!(
            (short.method.as_str(), short.url.raw.as_str()),
            ("GET", "https://x/a")
        );
        let long = items[1].request.as_ref().unwrap();
        assert_eq!(
            (long.method.as_str(), long.url.raw.as_str()),
            ("", "https://x/b")
        );
        assert_eq!(long.body.as_ref().unwrap().formdata[0].src, "a.txt");
        assert_eq!(items[1].event[0].script.exec, ["one", "two"]);
        assert_eq!(items[1].event[1].script.exec, ["x", ""]);
        assert!(items[2].is_folder() && !items[1].is_folder());
        let values: Vec<&str> = c.variable.iter().map(|v| v.value.as_str()).collect();
        assert_eq!(
            values,
            ["20", "true", "{\"a\":\"\\u003c\",\"b\":1.5}", "", ""]
        );
        assert!(c.variable[0].active() && !c.variable[4].active());
        assert!(Collection::from_json(b"{}").unwrap().item.is_none());
    }

    #[test]
    fn detects_collections_and_environments() {
        for (json, want) in [
            (
                r#"{"info": {"schema": "https://schema.getpostman.com/json/collection/v2.1.0/collection.json"}}"#,
                true,
            ),
            (r#"{"item": null}"#, true),
            (r#"{"ORDER": []}"#, true),
            (r#"{"info": null, "item": []}"#, true),
            (r#"{"info": 5, "item": []}"#, false),
            (r#"{"info": {"schema": 5}, "item": []}"#, false),
            (r#"{"openapi": "3.1.0"}"#, false),
            ("[]", false),
            ("null", false),
            ("{", false),
        ] {
            assert_eq!(looks_like_collection(json.as_bytes()), want, "{json}");
        }
        for (json, want) in [
            (
                r#"{"name": "x", "values": [], "_postman_variable_scope": "environment"}"#,
                true,
            ),
            (r#"{"values": null}"#, true),
            (r#"{"_postman_variable_scope": "environment"}"#, true),
            (
                r#"{"values": [], "_postman_variable_scope": "globals"}"#,
                false,
            ),
            (r#"{"values": [], "_postman_variable_scope": 1}"#, false),
            (r#"{"item": []}"#, false),
            ("[]", false),
        ] {
            assert_eq!(looks_like_environment(json.as_bytes()), want, "{json}");
        }
    }
}
