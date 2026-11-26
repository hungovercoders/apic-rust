//! A JSON Schema as a tree of the keywords the generator uses, written out
//! with its keys in one fixed order so the files do not churn.

use std::collections::BTreeMap;

use apic_gocompat::{escape_html, indent_json, json_string};
use serde_json::Value;

/// One schema. An unset keyword is left out of the file.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Schema {
    /// `type`, as one name.
    pub ty: &'static str,
    /// `type`, as a list of names; at most one of this and `ty` is set.
    pub types: Vec<&'static str>,
    /// `properties`; written even when empty, unlike when it is `None`.
    pub properties: Option<BTreeMap<String, Schema>>,
    pub items: Option<Box<Schema>>,
    pub id: String,
    pub schema: &'static str,
    pub reference: &'static str,
    pub defs: BTreeMap<String, Schema>,
    pub title: &'static str,
    pub description: String,
    pub examples: Vec<Value>,
    pub enumeration: Vec<Value>,
    pub minimum: Option<f64>,
    pub pattern: &'static str,
    pub required: Vec<&'static str>,
    pub additional_properties: Option<Box<Schema>>,
    pub property_names: Option<Box<Schema>>,
    pub one_of: Vec<Schema>,
    pub not: Option<Box<Schema>>,
}

impl Schema {
    /// A schema of one type.
    pub fn of(ty: &'static str) -> Schema {
        Schema {
            ty,
            ..Schema::default()
        }
    }

    /// A schema of one type with its description.
    pub fn described(ty: &'static str, description: &str) -> Schema {
        Schema::of(ty).desc(description)
    }

    /// A reference to another schema of the same file.
    pub fn refer(reference: &'static str) -> Schema {
        Schema {
            reference,
            ..Schema::default()
        }
    }

    /// The schema nothing validates against.
    pub fn never() -> Schema {
        Schema {
            not: Some(Box::default()),
            ..Schema::default()
        }
    }

    pub fn desc(mut self, description: &str) -> Schema {
        self.description = description.to_string();
        self
    }

    pub fn props<const N: usize>(mut self, props: [(&str, Schema); N]) -> Schema {
        self.properties = Some(
            props
                .into_iter()
                .map(|(name, s)| (name.to_string(), s))
                .collect(),
        );
        self
    }

    pub fn additional(mut self, s: Schema) -> Schema {
        self.additional_properties = Some(Box::new(s));
        self
    }

    pub fn one_of_strings<const N: usize>(mut self, values: [&str; N]) -> Schema {
        self.enumeration = values.iter().map(|v| Value::from(*v)).collect();
        self
    }

    /// The schema as compact JSON: `type`, `properties` and `items` first,
    /// then the rest in the order the specification introduces them. The
    /// empty schema is `true` and the schema that forbids everything is
    /// `false`.
    fn compact(&self) -> String {
        let mut members: Vec<String> = Vec::new();
        let mut put = |key: &str, value: String| members.push(format!("\"{key}\":{value}"));
        let text = |s: &str| json_string(s, true);
        let value = |v: &Value| escape_html(&v.to_string());
        let list = |items: Vec<String>| format!("[{}]", items.join(","));
        let map = |m: &BTreeMap<String, Schema>| {
            let members: Vec<String> = m
                .iter()
                .map(|(k, s)| format!("{}:{}", text(k), s.compact()))
                .collect();
            format!("{{{}}}", members.join(","))
        };
        if !self.ty.is_empty() {
            put("type", text(self.ty));
        } else if !self.types.is_empty() {
            put("type", list(self.types.iter().map(|t| text(t)).collect()));
        }
        if let Some(props) = &self.properties {
            put("properties", map(props));
        }
        if let Some(items) = &self.items {
            put("items", items.compact());
        }
        if !self.id.is_empty() {
            put("$id", text(&self.id));
        }
        if !self.schema.is_empty() {
            put("$schema", text(self.schema));
        }
        if !self.reference.is_empty() {
            put("$ref", text(self.reference));
        }
        if !self.defs.is_empty() {
            put("$defs", map(&self.defs));
        }
        if !self.title.is_empty() {
            put("title", text(self.title));
        }
        if !self.description.is_empty() {
            put("description", text(&self.description));
        }
        if !self.examples.is_empty() {
            put("examples", list(self.examples.iter().map(value).collect()));
        }
        if !self.enumeration.is_empty() {
            put("enum", list(self.enumeration.iter().map(value).collect()));
        }
        if let Some(min) = self.minimum {
            put("minimum", apic_gocompat::format_float_json(min));
        }
        if !self.pattern.is_empty() {
            put("pattern", text(self.pattern));
        }
        if !self.required.is_empty() {
            put(
                "required",
                list(self.required.iter().map(|r| text(r)).collect()),
            );
        }
        if let Some(s) = &self.additional_properties {
            put("additionalProperties", s.compact());
        }
        if let Some(s) = &self.property_names {
            put("propertyNames", s.compact());
        }
        if !self.one_of.is_empty() {
            put(
                "oneOf",
                list(self.one_of.iter().map(Schema::compact).collect()),
            );
        }
        if let Some(s) = &self.not {
            put("not", s.compact());
        }
        let out = format!("{{{}}}", members.join(","));
        match out.as_str() {
            "{}" => "true".to_string(),
            r#"{"not":true}"# => "false".to_string(),
            _ => out,
        }
    }

    /// The schema the way the files on disk hold it: indented by two
    /// spaces, with a newline at the end.
    pub fn render(&self) -> String {
        format!("{}\n", indent_json(&self.compact(), "", "  "))
    }
}
