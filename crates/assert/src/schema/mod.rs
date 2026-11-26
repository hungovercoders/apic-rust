//! JSON Schema, draft 2020-12 and draft-07, for `matchesSchema`: a schema
//! is read from JSON, its references are resolved within the one file,
//! and a value is validated against it. A reference to another file is
//! refused. A failure says where in the schema it happened and why.

mod resolve;
mod show;
mod uri;
mod validate;

use std::collections::BTreeMap;

use apic_gocompat::quote;
use serde_json::Value;

pub use resolve::Resolved;

/// A schema's index in the document it was read from; 0 is the root.
pub(crate) type Id = usize;

/// One schema or subschema. The keywords are fields; a subschema is the
/// index of another schema of the same document.
#[derive(Debug, Default)]
pub(crate) struct Schema {
    /// Neither `true`, `{}` nor anything else that says nothing.
    pub nonzero: bool,
    pub id: String,
    pub schema: String,
    pub ref_: String,
    pub defs: Option<BTreeMap<String, Id>>,
    pub definitions: Option<BTreeMap<String, Id>>,
    pub dependency_schemas: Option<BTreeMap<String, Id>>,
    pub dependency_strings: Option<BTreeMap<String, Vec<String>>>,
    pub anchor: String,
    pub dynamic_anchor: String,
    pub dynamic_ref: String,
    pub vocabulary: bool,
    pub type_: String,
    pub types: Option<Vec<String>>,
    pub enum_: Option<Vec<Value>>,
    pub const_: Option<Value>,
    pub multiple_of: Option<f64>,
    pub minimum: Option<f64>,
    pub maximum: Option<f64>,
    pub exclusive_minimum: Option<f64>,
    pub exclusive_maximum: Option<f64>,
    pub min_length: Option<i64>,
    pub max_length: Option<i64>,
    pub pattern: String,
    pub prefix_items: Vec<Id>,
    pub items: Option<Id>,
    pub items_array: Option<Vec<Id>>,
    pub min_items: Option<i64>,
    pub max_items: Option<i64>,
    pub additional_items: Option<Id>,
    pub unique_items: bool,
    pub contains: Option<Id>,
    pub min_contains: Option<i64>,
    pub max_contains: Option<i64>,
    pub unevaluated_items: Option<Id>,
    pub min_properties: Option<i64>,
    pub max_properties: Option<i64>,
    pub required: Option<Vec<String>>,
    pub dependent_required: Option<BTreeMap<String, Vec<String>>>,
    pub properties: Option<BTreeMap<String, Id>>,
    pub pattern_properties: Option<BTreeMap<String, Id>>,
    pub additional_properties: Option<Id>,
    pub property_names: Option<Id>,
    pub unevaluated_properties: Option<Id>,
    pub all_of: Option<Vec<Id>>,
    pub any_of: Option<Vec<Id>>,
    pub one_of: Option<Vec<Id>>,
    pub not: Option<Id>,
    pub if_: Option<Id>,
    pub then: Option<Id>,
    pub else_: Option<Id>,
    pub dependent_schemas: Option<BTreeMap<String, Id>>,
    pub content_schema: Option<Id>,
}

impl Schema {
    /// How the schema is named in a message that lists schemas.
    pub fn name(&self) -> String {
        if !self.id.is_empty() {
            return self.id.clone();
        }
        let anchor = if self.anchor.is_empty() {
            &self.dynamic_anchor
        } else {
            &self.anchor
        };
        if !anchor.is_empty() {
            return format!("anchor {anchor}");
        }
        "<anonymous schema>".to_string()
    }
}

/// The schemas of one document, the root first. `None` is a subschema
/// written as `null`, which is refused when the document is resolved.
#[derive(Debug, Default)]
pub(crate) struct Document {
    pub schemas: Vec<Option<Schema>>,
}

fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn mismatch(v: &Value, field: &str, want: &str) -> String {
    let what = match v {
        Value::Number(n) if want == "float64" => format!("number {n}"),
        v => type_name(v).to_string(),
    };
    if field.is_empty() {
        return format!("json: cannot unmarshal {what} into Go value of type {want}");
    }
    format!("json: cannot unmarshal {what} into Go struct field .schemaWithoutMethods.{field} of type {want}")
}

const SCHEMA_TYPE: &str = "jsonschema.Schema";

impl Document {
    /// Reads a schema document. The error is what is wrong with it as
    /// JSON for a schema: a keyword of the wrong type, say.
    pub fn parse(v: &Value) -> Result<Document, String> {
        let mut doc = Document::default();
        doc.schemas.push(None);
        // A document that is just null reads as false.
        let v = if v.is_null() { &Value::Bool(false) } else { v };
        doc.schemas[0] = doc.read(v, "")?;
        Ok(doc)
    }

    fn add(&mut self, v: &Value, field: &str) -> Result<Id, String> {
        let id = self.schemas.len();
        self.schemas.push(None);
        self.schemas[id] = self.read(v, field)?;
        Ok(id)
    }

    /// A keyword whose value is one schema; `null` leaves it unset.
    fn one(&mut self, v: &Value, field: &str) -> Result<Option<Id>, String> {
        if v.is_null() {
            return Ok(None);
        }
        self.add(v, field).map(Some)
    }

    fn list(&mut self, v: &Value, field: &str) -> Result<Option<Vec<Id>>, String> {
        match v {
            Value::Null => Ok(None),
            Value::Array(a) => a
                .iter()
                .map(|e| self.add(e, field))
                .collect::<Result<_, _>>()
                .map(Some),
            v => Err(mismatch(v, field, "[]*jsonschema.Schema")),
        }
    }

    fn map(&mut self, v: &Value, field: &str) -> Result<Option<BTreeMap<String, Id>>, String> {
        match v {
            Value::Null => Ok(None),
            Value::Object(o) => {
                let mut out = BTreeMap::new();
                for (k, e) in o {
                    out.insert(k.clone(), self.add(e, field)?);
                }
                Ok(Some(out))
            }
            v => Err(mismatch(v, field, "map[string]*jsonschema.Schema")),
        }
    }

    /// Reads one schema: `true`, `false` or an object of keywords. `null`
    /// is no schema at all.
    fn read(&mut self, v: &Value, field: &str) -> Result<Option<Schema>, String> {
        let mut s = Schema::default();
        let o = match v {
            Value::Null => return Ok(None),
            Value::Bool(true) => return Ok(Some(s)),
            Value::Bool(false) => {
                // false is {"not": {}}.
                let not = self.schemas.len();
                self.schemas.push(Some(Schema::default()));
                s.not = Some(not);
                s.nonzero = true;
                return Ok(Some(s));
            }
            Value::Object(o) => o,
            v => return Err(mismatch(v, field, SCHEMA_TYPE)),
        };
        for (key, v) in o {
            let key = key.as_str();
            // A null leaves most keywords unset; the ones read from the
            // raw text see it.
            if v.is_null() && !matches!(key, "type" | "items" | "const" | "dependencies") {
                continue;
            }
            s.nonzero = true;
            // "default" and keywords this reader does not know say
            // nothing about validity.
            if let Some((_, keyword)) = KEYWORDS.iter().find(|(name, _)| *name == key) {
                self.read_keyword(&mut s, keyword, v, key)?;
            }
        }
        Ok(Some(s))
    }

    /// Reads one keyword's value into the schema.
    fn read_keyword(
        &mut self,
        s: &mut Schema,
        keyword: &Keyword,
        v: &Value,
        key: &str,
    ) -> Result<(), String> {
        match keyword {
            Keyword::Str(field) => *field(s) = string(v, key)?,
            Keyword::UnusedStr => {
                string(v, key)?;
            }
            Keyword::UnusedBool => {
                boolean(v, key)?;
            }
            Keyword::Map(field) => *field(s) = self.map(v, key)?,
            Keyword::List(field) => *field(s) = self.list(v, key)?,
            Keyword::One(field) => *field(s) = self.one(v, key)?,
            Keyword::Float(field) => *field(s) = Some(float(v, key)?),
            Keyword::Int(field) => *field(s) = Some(integer(v)?),
            Keyword::Other(read) => read(self, s, v, key)?,
        }
        Ok(())
    }

    fn read_unique_items(&mut self, s: &mut Schema, v: &Value, key: &str) -> Result<(), String> {
        s.unique_items = boolean(v, key)?;
        Ok(())
    }

    fn read_examples(&mut self, _: &mut Schema, v: &Value, key: &str) -> Result<(), String> {
        if !v.is_array() {
            return Err(mismatch(v, key, "[]interface {}"));
        }
        Ok(())
    }

    fn read_vocabulary(&mut self, s: &mut Schema, v: &Value, key: &str) -> Result<(), String> {
        let Value::Object(o) = v else {
            return Err(mismatch(v, key, "map[string]bool"));
        };
        for e in o.values() {
            if !e.is_null() {
                boolean(e, key)?;
            }
        }
        s.vocabulary = true;
        Ok(())
    }

    fn read_prefix_items(&mut self, s: &mut Schema, v: &Value, key: &str) -> Result<(), String> {
        s.prefix_items = self.list(v, key)?.unwrap_or_default();
        Ok(())
    }

    fn read_type(&mut self, s: &mut Schema, v: &Value, _: &str) -> Result<(), String> {
        match v {
            Value::String(t) => s.type_ = t.clone(),
            Value::Array(a) => {
                let mut types = Vec::new();
                for e in a {
                    match e {
                        Value::Null => types.push(String::new()),
                        Value::String(t) => types.push(t.clone()),
                        e => return Err(mismatch(e, "", "string")),
                    }
                }
                s.types = Some(types);
            }
            v => {
                return Err(format!(
                    "invalid value for \"type\": {}",
                    quote(&v.to_string())
                ))
            }
        }
        Ok(())
    }

    fn read_items(&mut self, s: &mut Schema, v: &Value, _: &str) -> Result<(), String> {
        match v {
            Value::Array(a) => {
                let mut ids = Vec::new();
                for e in a {
                    ids.push(self.add(e, "")?);
                }
                s.items_array = Some(ids);
            }
            // Here a null reads as false.
            Value::Null => s.items = Some(self.add(&Value::Bool(false), "")?),
            v => s.items = Some(self.add(v, "")?),
        }
        Ok(())
    }

    fn read_dependencies(&mut self, s: &mut Schema, v: &Value, key: &str) -> Result<(), String> {
        match v {
            Value::Null => {}
            Value::Object(o) => {
                for (k, d) in o {
                    if d.is_array() {
                        let names = strings(d, "")?;
                        s.dependency_strings
                            .get_or_insert_with(BTreeMap::new)
                            .insert(k.clone(), names);
                    } else {
                        let id = self.add(if d.is_null() { &Value::Bool(false) } else { d }, "")?;
                        s.dependency_schemas
                            .get_or_insert_with(BTreeMap::new)
                            .insert(k.clone(), id);
                    }
                }
            }
            v => return Err(mismatch(v, key, "map[string]json.RawMessage")),
        }
        Ok(())
    }

    fn read_const(&mut self, s: &mut Schema, v: &Value, _: &str) -> Result<(), String> {
        s.const_ = Some(v.clone());
        Ok(())
    }

    fn read_enum(&mut self, s: &mut Schema, v: &Value, key: &str) -> Result<(), String> {
        match v {
            Value::Array(a) => s.enum_ = Some(a.clone()),
            v => return Err(mismatch(v, key, "[]interface {}")),
        }
        Ok(())
    }

    fn read_required(&mut self, s: &mut Schema, v: &Value, key: &str) -> Result<(), String> {
        s.required = Some(strings(v, key)?);
        Ok(())
    }

    fn read_dependent_required(
        &mut self,
        s: &mut Schema,
        v: &Value,
        key: &str,
    ) -> Result<(), String> {
        let Value::Object(o) = v else {
            return Err(mismatch(v, key, "map[string][]string"));
        };
        let mut out = BTreeMap::new();
        for (k, names) in o {
            let names = if names.is_null() {
                Vec::new()
            } else {
                strings(names, key)?
            };
            out.insert(k.clone(), names);
        }
        s.dependent_required = Some(out);
        Ok(())
    }
}

/// How a keyword's value is read, and the field of the schema it fills.
enum Keyword {
    Str(fn(&mut Schema) -> &mut String),
    /// A string that is checked and not kept.
    UnusedStr,
    /// A boolean that is checked and not kept.
    UnusedBool,
    /// A map of names to subschemas.
    Map(fn(&mut Schema) -> &mut Option<BTreeMap<String, Id>>),
    /// A list of subschemas.
    List(fn(&mut Schema) -> &mut Option<Vec<Id>>),
    /// One subschema.
    One(fn(&mut Schema) -> &mut Option<Id>),
    Float(fn(&mut Schema) -> &mut Option<f64>),
    Int(fn(&mut Schema) -> &mut Option<i64>),
    /// A keyword with a reader of its own.
    Other(fn(&mut Document, &mut Schema, &Value, &str) -> Result<(), String>),
}

/// The keywords the reader knows.
const KEYWORDS: &[(&str, Keyword)] = &[
    ("$id", Keyword::Str(|s| &mut s.id)),
    ("$schema", Keyword::Str(|s| &mut s.schema)),
    ("$ref", Keyword::Str(|s| &mut s.ref_)),
    ("$anchor", Keyword::Str(|s| &mut s.anchor)),
    ("$dynamicAnchor", Keyword::Str(|s| &mut s.dynamic_anchor)),
    ("$dynamicRef", Keyword::Str(|s| &mut s.dynamic_ref)),
    ("pattern", Keyword::Str(|s| &mut s.pattern)),
    ("$comment", Keyword::UnusedStr),
    ("title", Keyword::UnusedStr),
    ("description", Keyword::UnusedStr),
    ("contentEncoding", Keyword::UnusedStr),
    ("contentMediaType", Keyword::UnusedStr),
    ("format", Keyword::UnusedStr),
    ("deprecated", Keyword::UnusedBool),
    ("readOnly", Keyword::UnusedBool),
    ("writeOnly", Keyword::UnusedBool),
    ("$defs", Keyword::Map(|s| &mut s.defs)),
    ("definitions", Keyword::Map(|s| &mut s.definitions)),
    (
        "dependentSchemas",
        Keyword::Map(|s| &mut s.dependent_schemas),
    ),
    ("properties", Keyword::Map(|s| &mut s.properties)),
    (
        "patternProperties",
        Keyword::Map(|s| &mut s.pattern_properties),
    ),
    ("allOf", Keyword::List(|s| &mut s.all_of)),
    ("anyOf", Keyword::List(|s| &mut s.any_of)),
    ("oneOf", Keyword::List(|s| &mut s.one_of)),
    ("additionalItems", Keyword::One(|s| &mut s.additional_items)),
    ("contains", Keyword::One(|s| &mut s.contains)),
    (
        "unevaluatedItems",
        Keyword::One(|s| &mut s.unevaluated_items),
    ),
    (
        "additionalProperties",
        Keyword::One(|s| &mut s.additional_properties),
    ),
    ("propertyNames", Keyword::One(|s| &mut s.property_names)),
    (
        "unevaluatedProperties",
        Keyword::One(|s| &mut s.unevaluated_properties),
    ),
    ("not", Keyword::One(|s| &mut s.not)),
    ("if", Keyword::One(|s| &mut s.if_)),
    ("then", Keyword::One(|s| &mut s.then)),
    ("else", Keyword::One(|s| &mut s.else_)),
    ("contentSchema", Keyword::One(|s| &mut s.content_schema)),
    ("multipleOf", Keyword::Float(|s| &mut s.multiple_of)),
    ("minimum", Keyword::Float(|s| &mut s.minimum)),
    ("maximum", Keyword::Float(|s| &mut s.maximum)),
    (
        "exclusiveMinimum",
        Keyword::Float(|s| &mut s.exclusive_minimum),
    ),
    (
        "exclusiveMaximum",
        Keyword::Float(|s| &mut s.exclusive_maximum),
    ),
    ("minLength", Keyword::Int(|s| &mut s.min_length)),
    ("maxLength", Keyword::Int(|s| &mut s.max_length)),
    ("minItems", Keyword::Int(|s| &mut s.min_items)),
    ("maxItems", Keyword::Int(|s| &mut s.max_items)),
    ("minProperties", Keyword::Int(|s| &mut s.min_properties)),
    ("maxProperties", Keyword::Int(|s| &mut s.max_properties)),
    ("minContains", Keyword::Int(|s| &mut s.min_contains)),
    ("maxContains", Keyword::Int(|s| &mut s.max_contains)),
    ("uniqueItems", Keyword::Other(Document::read_unique_items)),
    ("examples", Keyword::Other(Document::read_examples)),
    ("$vocabulary", Keyword::Other(Document::read_vocabulary)),
    ("prefixItems", Keyword::Other(Document::read_prefix_items)),
    ("type", Keyword::Other(Document::read_type)),
    ("items", Keyword::Other(Document::read_items)),
    ("dependencies", Keyword::Other(Document::read_dependencies)),
    ("const", Keyword::Other(Document::read_const)),
    ("enum", Keyword::Other(Document::read_enum)),
    ("required", Keyword::Other(Document::read_required)),
    (
        "dependentRequired",
        Keyword::Other(Document::read_dependent_required),
    ),
];

fn string(v: &Value, field: &str) -> Result<String, String> {
    match v {
        Value::String(s) => Ok(s.clone()),
        v => Err(mismatch(v, field, "string")),
    }
}

fn boolean(v: &Value, field: &str) -> Result<bool, String> {
    match v {
        Value::Bool(b) => Ok(*b),
        v => Err(mismatch(v, field, "bool")),
    }
}

fn float(v: &Value, field: &str) -> Result<f64, String> {
    match v {
        Value::Number(n) => n.as_f64().ok_or_else(|| mismatch(v, field, "float64")),
        v => Err(mismatch(v, field, "float64")),
    }
}

fn strings(v: &Value, field: &str) -> Result<Vec<String>, String> {
    let Value::Array(a) = v else {
        return Err(mismatch(v, field, "[]string"));
    };
    a.iter()
        .map(|e| match e {
            Value::Null => Ok(String::new()),
            Value::String(s) => Ok(s.clone()),
            e => Err(mismatch(e, field, "string")),
        })
        .collect()
}

/// A keyword that counts things: a whole number, which `2.0` is and `1e2`
/// is not.
fn integer(v: &Value) -> Result<i64, String> {
    let text = v.to_string();
    let i = if text.contains('.') {
        let f = v.as_f64().ok_or("not a number")?;
        let i = f as i64;
        if i as f64 != f {
            return Err("not an integer value".to_string());
        }
        i
    } else {
        v.as_i64().ok_or("cannot be unmarshaled into an int")?
    };
    if i < i64::from(i32::MIN) || i > i64::from(i32::MAX) {
        return Err("integer is out of range".to_string());
    }
    Ok(i)
}
