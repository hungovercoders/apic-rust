//! JSON Schema, draft 2020-12 and draft-07, for `matchesSchema`: a schema
//! is read from JSON, its references are resolved within the one file,
//! and a value is validated against it. A reference to another file is
//! refused. A failure says where in the schema it happened and why.

mod gofmt;
mod resolve;
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
        let anchor = if self.anchor.is_empty() { &self.dynamic_anchor } else { &self.anchor };
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
        let root = doc.read(v, "")?.unwrap_or_default();
        doc.schemas[0] = Some(root);
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
            Value::Array(a) => a.iter().map(|e| self.add(e, field)).collect::<Result<_, _>>().map(Some),
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
            match key {
                "$id" => s.id = string(v, key)?,
                "$schema" => s.schema = string(v, key)?,
                "$ref" => s.ref_ = string(v, key)?,
                "$anchor" => s.anchor = string(v, key)?,
                "$dynamicAnchor" => s.dynamic_anchor = string(v, key)?,
                "$dynamicRef" => s.dynamic_ref = string(v, key)?,
                "pattern" => s.pattern = string(v, key)?,
                "$comment" | "title" | "description" | "contentEncoding" | "contentMediaType" | "format" => {
                    string(v, key)?;
                }
                "deprecated" | "readOnly" | "writeOnly" => {
                    boolean(v, key)?;
                }
                "uniqueItems" => s.unique_items = boolean(v, key)?,
                "examples" => {
                    if !v.is_array() {
                        return Err(mismatch(v, key, "[]interface {}"));
                    }
                }
                "$vocabulary" => match v {
                    Value::Object(o) => {
                        for e in o.values() {
                            if !e.is_null() {
                                boolean(e, key)?;
                            }
                        }
                        s.vocabulary = true;
                    }
                    v => return Err(mismatch(v, key, "map[string]bool")),
                },
                "$defs" => s.defs = self.map(v, key)?,
                "definitions" => s.definitions = self.map(v, key)?,
                "dependentSchemas" => s.dependent_schemas = self.map(v, key)?,
                "properties" => s.properties = self.map(v, key)?,
                "patternProperties" => s.pattern_properties = self.map(v, key)?,
                "prefixItems" => s.prefix_items = self.list(v, key)?.unwrap_or_default(),
                "allOf" => s.all_of = self.list(v, key)?,
                "anyOf" => s.any_of = self.list(v, key)?,
                "oneOf" => s.one_of = self.list(v, key)?,
                "additionalItems" => s.additional_items = self.one(v, key)?,
                "contains" => s.contains = self.one(v, key)?,
                "unevaluatedItems" => s.unevaluated_items = self.one(v, key)?,
                "additionalProperties" => s.additional_properties = self.one(v, key)?,
                "propertyNames" => s.property_names = self.one(v, key)?,
                "unevaluatedProperties" => s.unevaluated_properties = self.one(v, key)?,
                "not" => s.not = self.one(v, key)?,
                "if" => s.if_ = self.one(v, key)?,
                "then" => s.then = self.one(v, key)?,
                "else" => s.else_ = self.one(v, key)?,
                "contentSchema" => s.content_schema = self.one(v, key)?,
                "type" => match v {
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
                    v => return Err(format!("invalid value for \"type\": {}", quote(&v.to_string()))),
                },
                "items" => match v {
                    Value::Array(a) => {
                        let mut ids = Vec::new();
                        for e in a {
                            ids.push(self.add(e, "")?);
                        }
                        s.items_array = Some(ids);
                    }
                    // A null is the schema that says nothing.
                    Value::Null => s.items = Some(self.add(&Value::Bool(true), "")?),
                    v => s.items = Some(self.add(v, "")?),
                },
                "dependencies" => match v {
                    Value::Null => {}
                    Value::Object(o) => {
                        for (k, d) in o {
                            if d.is_array() {
                                let names = strings(d, "")?;
                                s.dependency_strings.get_or_insert_with(BTreeMap::new).insert(k.clone(), names);
                            } else {
                                let id = self.add(if d.is_null() { &Value::Bool(true) } else { d }, "")?;
                                s.dependency_schemas.get_or_insert_with(BTreeMap::new).insert(k.clone(), id);
                            }
                        }
                    }
                    v => return Err(mismatch(v, key, "map[string]json.RawMessage")),
                },
                "const" => s.const_ = Some(v.clone()),
                "enum" => match v {
                    Value::Array(a) => s.enum_ = Some(a.clone()),
                    v => return Err(mismatch(v, key, "[]interface {}")),
                },
                "multipleOf" => s.multiple_of = Some(float(v, key)?),
                "minimum" => s.minimum = Some(float(v, key)?),
                "maximum" => s.maximum = Some(float(v, key)?),
                "exclusiveMinimum" => s.exclusive_minimum = Some(float(v, key)?),
                "exclusiveMaximum" => s.exclusive_maximum = Some(float(v, key)?),
                "minLength" => s.min_length = Some(integer(v)?),
                "maxLength" => s.max_length = Some(integer(v)?),
                "minItems" => s.min_items = Some(integer(v)?),
                "maxItems" => s.max_items = Some(integer(v)?),
                "minProperties" => s.min_properties = Some(integer(v)?),
                "maxProperties" => s.max_properties = Some(integer(v)?),
                "minContains" => s.min_contains = Some(integer(v)?),
                "maxContains" => s.max_contains = Some(integer(v)?),
                "required" => s.required = Some(strings(v, key)?),
                "dependentRequired" => match v {
                    Value::Object(o) => {
                        let mut out = BTreeMap::new();
                        for (k, names) in o {
                            let names = if names.is_null() { Vec::new() } else { strings(names, key)? };
                            out.insert(k.clone(), names);
                        }
                        s.dependent_required = Some(out);
                    }
                    v => return Err(mismatch(v, key, "map[string][]string")),
                },
                // "default" and keywords this reader does not know say
                // nothing about validity.
                _ => {}
            }
        }
        Ok(Some(s))
    }
}

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
