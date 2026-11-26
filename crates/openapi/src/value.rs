//! The values examples are made of: what a document's own examples decode
//! to and what the generator derives from a schema, with objects that keep
//! their keys in order and numbers that keep their spelling.

use std::collections::HashMap;

use apic_gocompat::{format_float_g, format_float_json, json_string};

/// An example value.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Value {
    Null,
    Bool(bool),
    Int(i64),
    /// A whole number too large for `Int`.
    Uint(u64),
    Float(f64),
    /// A number spelt as JSON spells one, kept as written:
    /// `18446744073709551616` and `1e400` survive unrounded.
    Number(String),
    Str(String),
    /// A template expression the importer generates itself (`{{$uuid}}`,
    /// `{{$isoTimestamp}}`); unlike text from the document it is meant to
    /// render.
    Placeholder(&'static str),
    Object(Object),
    Array(Vec<Value>),
}

/// The placeholder for a UUID.
pub(crate) const UUID: &str = "{{$uuid}}";
/// The placeholder for a date and time.
pub(crate) const ISO_TIMESTAMP: &str = "{{$isoTimestamp}}";

/// An object that keeps its keys in the order they were first set.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Object {
    entries: Vec<(String, Value)>,
    index: HashMap<String, usize>,
}

impl Object {
    /// Sets a key; one already there keeps its place and takes the value.
    pub(crate) fn set(&mut self, key: &str, value: Value) {
        match self.index.get(key) {
            Some(&i) => self.entries[i].1 = value,
            None => {
                self.index.insert(key.to_string(), self.entries.len());
                self.entries.push((key.to_string(), value));
            }
        }
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (&str, &Value)> {
        self.entries.iter().map(|(k, v)| (k.as_str(), v))
    }

    pub(crate) fn into_entries(self) -> Vec<(String, Value)> {
        self.entries
    }

    fn values_mut(&mut self) -> impl Iterator<Item = &mut Value> {
        self.entries.iter_mut().map(|(_, v)| v)
    }
}

impl Value {
    /// Reports whether text from the document in the value contains a
    /// template marker, which the runner would otherwise try to render.
    pub(crate) fn has_literal_braces(&self) -> bool {
        match self {
            Value::Str(s) => s.contains("{{") || s.contains("}}"),
            Value::Object(o) => o
                .iter()
                .any(|(k, v)| k.contains("{{") || v.has_literal_braces()),
            Value::Array(items) => items.iter().any(Value::has_literal_braces),
            _ => false,
        }
    }

    /// Replaces generated placeholders with fixed sample values, for
    /// bodies that must be written verbatim because they contain a literal
    /// `{{`.
    pub(crate) fn concretize(mut self) -> Value {
        self.concretize_in_place();
        self
    }

    fn concretize_in_place(&mut self) {
        match self {
            Value::Placeholder(p) => {
                *self = Value::Str(
                    if *p == UUID {
                        "00000000-0000-4000-8000-000000000000"
                    } else {
                        "2024-01-01T00:00:00Z"
                    }
                    .to_string(),
                );
            }
            Value::Object(o) => o.values_mut().for_each(Value::concretize_in_place),
            Value::Array(items) => items.iter_mut().for_each(Value::concretize_in_place),
            _ => {}
        }
    }

    /// The text of a scalar that is not a string, as it is written into a
    /// body or a header: `1`, `1.5`, `true`. `None` for anything else.
    pub(crate) fn scalar_text(&self) -> Option<String> {
        match self {
            Value::Bool(b) => Some(b.to_string()),
            Value::Int(n) => Some(n.to_string()),
            Value::Uint(n) => Some(n.to_string()),
            Value::Float(f) => Some(format_float_g(*f)),
            Value::Number(n) => Some(n.clone()),
            _ => None,
        }
    }

    /// The value as JSON on one line.
    pub(crate) fn to_json(&self) -> String {
        let mut out = String::new();
        self.write_json(&mut out, None, 0);
        out
    }

    /// The value as JSON, one member or element to a line, indented by
    /// two spaces a level.
    pub(crate) fn to_json_indent(&self) -> String {
        let mut out = String::new();
        self.write_json(&mut out, Some("  "), 0);
        out
    }

    fn write_json(&self, out: &mut String, indent: Option<&str>, depth: usize) {
        let newline = |out: &mut String, depth: usize| {
            if let Some(indent) = indent {
                out.push('\n');
                for _ in 0..depth {
                    out.push_str(indent);
                }
            }
        };
        match self {
            Value::Null => out.push_str("null"),
            Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Value::Int(n) => out.push_str(&n.to_string()),
            Value::Uint(n) => out.push_str(&n.to_string()),
            Value::Float(f) => out.push_str(&format_float_json(*f)),
            Value::Number(n) => out.push_str(n),
            Value::Str(s) => out.push_str(&json_string(s, true)),
            Value::Placeholder(p) => out.push_str(&json_string(p, true)),
            Value::Object(o) => {
                if o.entries.is_empty() {
                    out.push_str("{}");
                    return;
                }
                out.push('{');
                for (i, (k, v)) in o.entries.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    newline(out, depth + 1);
                    out.push_str(&json_string(k, true));
                    out.push(':');
                    if indent.is_some() {
                        out.push(' ');
                    }
                    v.write_json(out, indent, depth + 1);
                }
                newline(out, depth);
                out.push('}');
            }
            Value::Array(items) => {
                if items.is_empty() {
                    out.push_str("[]");
                    return;
                }
                out.push('[');
                for (i, v) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    newline(out, depth + 1);
                    v.write_json(out, indent, depth + 1);
                }
                newline(out, depth);
                out.push(']');
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn objects_keep_order_and_json_keeps_numbers() {
        let mut o = Object::default();
        o.set("b", Value::Int(1));
        o.set("a", Value::Number("1e400".to_string()));
        o.set("b", Value::Str("<&>".to_string()));
        o.set("c", Value::Array(vec![]));
        o.set("d", Value::Array(vec![Value::Float(1.5), Value::Null]));
        let v = Value::Object(o);
        assert_eq!(
            v.to_json(),
            "{\"b\":\"\\u003c\\u0026\\u003e\",\"a\":1e400,\"c\":[],\"d\":[1.5,null]}"
        );
        assert_eq!(
            v.to_json_indent(),
            "{\n  \"b\": \"\\u003c\\u0026\\u003e\",\n  \"a\": 1e400,\n  \"c\": [],\n  \"d\": [\n    1.5,\n    null\n  ]\n}"
        );
    }

    #[test]
    fn concretize_fixes_placeholders() {
        let v = Value::Array(vec![
            Value::Placeholder(UUID),
            Value::Placeholder(ISO_TIMESTAMP),
        ])
        .concretize();
        assert_eq!(
            v.to_json(),
            r#"["00000000-0000-4000-8000-000000000000","2024-01-01T00:00:00Z"]"#
        );
    }
}
