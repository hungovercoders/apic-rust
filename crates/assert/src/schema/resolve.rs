use std::collections::{BTreeMap, HashMap};

use apic_gocompat::quote;
use apic_selector::compile_regex;
use regex::Regex;
use serde_json::Value;

use super::uri::Uri;
use super::{Document, Id, Schema};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Draft {
    Draft7,
    Draft2020,
}

const DRAFT7: &str = "http://json-schema.org/draft-07/schema#";
const DRAFT7_SECURE: &str = "https://json-schema.org/draft-07/schema#";
const DRAFT2020: &str = "https://json-schema.org/draft/2020-12/schema";

#[derive(Clone, Debug)]
pub(crate) struct Anchor {
    pub schema: Id,
    pub dynamic: bool,
}

/// What resolving worked out about one schema.
#[derive(Debug, Default)]
pub(crate) struct Info {
    /// Where the schema is in the document, as a JSON Pointer; `root` for
    /// the document itself.
    pub path: String,
    /// The nearest schema at or above this one with its own `$id`.
    pub base: Id,
    pub uri: Option<Uri>,
    pub resolved_ref: Option<Id>,
    pub resolved_dynamic_ref: Option<Id>,
    pub dynamic_ref_anchor: String,
    pub pattern: Option<Regex>,
    pub pattern_properties: Vec<(Regex, Id)>,
    /// The anchors of the schemas this one is the base of.
    pub anchors: BTreeMap<String, Anchor>,
}

/// A schema whose references have been resolved, ready to validate with.
#[derive(Debug)]
pub struct Resolved {
    pub(crate) schemas: Vec<Schema>,
    pub(crate) infos: Vec<Info>,
    pub(crate) draft: Draft,
    resolved_uris: HashMap<String, Id>,
}

fn escape_pointer(s: &str) -> String {
    s.replace('~', "~0").replace('/', "~1")
}

/// What a keyword holds, as far as walking the document goes.
enum Field<'a> {
    One(Option<Id>),
    List(&'a [Id]),
    Map(Option<&'a BTreeMap<String, Id>>),
}

/// The keywords that hold subschemas, in the order they are walked.
fn schema_fields(s: &Schema) -> Vec<(&'static str, Field<'_>)> {
    let list = |l: &'_ Option<Vec<Id>>| -> &'_ [Id] { l.as_deref().unwrap_or(&[]) };
    let mut fields = vec![
        ("$defs", Field::Map(s.defs.as_ref())),
        ("additionalItems", Field::One(s.additional_items)),
        ("additionalProperties", Field::One(s.additional_properties)),
        ("allOf", Field::List(list(&s.all_of))),
        ("anyOf", Field::List(list(&s.any_of))),
        ("contains", Field::One(s.contains)),
        ("contentSchema", Field::One(s.content_schema)),
        ("definitions", Field::Map(s.definitions.as_ref())),
        ("dependencies", Field::Map(s.dependency_schemas.as_ref())),
        ("dependentSchemas", Field::Map(s.dependent_schemas.as_ref())),
        ("else", Field::One(s.else_)),
        ("if", Field::One(s.if_)),
        ("items", Field::One(s.items)),
    ];
    fields.extend([
        ("items", Field::List(list(&s.items_array))),
        ("not", Field::One(s.not)),
        ("oneOf", Field::List(list(&s.one_of))),
        ("patternProperties", Field::Map(s.pattern_properties.as_ref())),
        ("prefixItems", Field::List(&s.prefix_items)),
        ("properties", Field::Map(s.properties.as_ref())),
        ("propertyNames", Field::One(s.property_names)),
        ("then", Field::One(s.then)),
        ("unevaluatedItems", Field::One(s.unevaluated_items)),
        ("unevaluatedProperties", Field::One(s.unevaluated_properties)),
    ]);
    fields
}

/// A schema's subschemas with the pointer step to each.
fn children(s: &Schema) -> Vec<(String, Id)> {
    let mut out = Vec::new();
    for (name, field) in schema_fields(s) {
        match field {
            Field::One(Some(id)) => out.push((format!("/{name}"), id)),
            Field::One(None) => {}
            Field::List(ids) => {
                for (i, id) in ids.iter().enumerate() {
                    out.push((format!("/{name}/{i}"), *id));
                }
            }
            Field::Map(m) => {
                for (k, id) in m.into_iter().flatten() {
                    out.push((format!("/{name}/{}", escape_pointer(k)), *id));
                }
            }
        }
    }
    out
}

/// The type a keyword that holds no schema is described as when a
/// reference lands on it.
fn other_field_type(name: &str) -> Option<&'static str> {
    Some(match name {
        "$id" | "$schema" | "$ref" | "$comment" | "$anchor" | "$dynamicAnchor" | "$dynamicRef" | "title"
        | "description" | "pattern" | "contentEncoding" | "contentMediaType" | "format" => "string",
        "deprecated" | "readOnly" | "writeOnly" | "uniqueItems" => "bool",
        "required" => "[]string",
        "enum" | "examples" => "[]interface {}",
        "const" => "*interface {}",
        "multipleOf" | "minimum" | "maximum" | "exclusiveMinimum" | "exclusiveMaximum" => "*float64",
        "minLength" | "maxLength" | "minItems" | "maxItems" | "minContains" | "maxContains" | "minProperties"
        | "maxProperties" => "*int",
        "default" => "json.RawMessage",
        "$vocabulary" => "map[string]bool",
        "dependentRequired" => "map[string][]string",
        _ => return None,
    })
}

impl Resolved {
    /// Reads a schema from JSON and resolves the references in it. The
    /// error says what is wrong with the schema.
    pub fn new(v: &Value) -> Result<Resolved, String> {
        let doc = Document::parse(v)?;
        let draft = match doc.schemas[0].as_ref().map_or("", |s| s.schema.as_str()) {
            DRAFT7 | DRAFT7_SECURE => Draft::Draft7,
            _ => Draft::Draft2020,
        };
        // Every schema gets its place in the document; a null where a
        // schema belongs is refused here.
        let mut paths: Vec<Option<String>> = vec![None; doc.schemas.len()];
        let mut order: Vec<Id> = Vec::new();
        let mut stack = vec![(0usize, String::new())];
        while let Some((id, path)) = stack.pop() {
            let Some(s) = &doc.schemas[id] else {
                let p = if path.is_empty() { "root" } else { &path };
                return Err(format!("jsonschema: schema at {p} is nil"));
            };
            order.push(id);
            for (step, child) in children(s).into_iter().rev() {
                stack.push((child, format!("{path}{step}")));
            }
            paths[id] = Some(if path.is_empty() { "root".to_string() } else { path });
        }
        // Schemas no keyword reaches (the inside of a `false`) keep their
        // slot so that indexes hold.
        let schemas: Vec<Schema> = doc.schemas.into_iter().map(Option::unwrap_or_default).collect();
        let infos: Vec<Info> = paths
            .into_iter()
            .map(|p| Info { path: p.unwrap_or_default(), ..Info::default() })
            .collect();
        let mut rs = Resolved { schemas, infos, draft, resolved_uris: HashMap::new() };
        rs.check_local(&order)?;
        rs.resolve_uris()?;
        rs.resolve_refs(&order)?;
        Ok(rs)
    }

    /// The name a schema goes by in a failure: its `$id`, or its place in
    /// the document.
    pub(crate) fn schema_string(&self, id: Id) -> String {
        let s = &self.schemas[id];
        if !s.id.is_empty() {
            return s.id.clone();
        }
        let path = &self.infos[id].path;
        if !path.is_empty() {
            return path.clone();
        }
        "<anonymous schema>".to_string()
    }

    /// Checks each schema on its own and compiles its patterns. Every
    /// problem is reported, one to a line.
    fn check_local(&mut self, order: &[Id]) -> Result<(), String> {
        let mut errs: Vec<String> = Vec::new();
        for &id in order {
            let s = &self.schemas[id];
            let name = s.name();
            let addf = |errs: &mut Vec<String>, msg: String| errs.push(format!("jsonschema.Schema: {name}: {msg}"));
            if !s.type_.is_empty() && s.types.is_some() {
                errs.push("both Type and Types are set; at most one should be".to_string());
                continue;
            }
            if s.defs.is_some() && s.definitions.is_some() {
                errs.push("both Defs and Definitions are set; at most one should be".to_string());
                continue;
            }
            if let (Some(schemas), Some(strings)) = (&s.dependency_schemas, &s.dependency_strings) {
                if let Some(key) = schemas.keys().find(|k| strings.contains_key(*k)) {
                    errs.push(format!(
                        "dependency key {} cannot be defined as both a schema and a string array",
                        quote(key)
                    ));
                    continue;
                }
            }
            if s.vocabulary && s.schema != DRAFT2020 {
                addf(&mut errs, "cannot validate a schema with $vocabulary".to_string());
            }
            let mut pattern = None;
            if !s.pattern.is_empty() {
                match compile_regex(&s.pattern) {
                    Ok(re) => pattern = Some(re),
                    Err(e) => addf(&mut errs, format!("pattern: {e}")),
                }
            }
            let mut pattern_properties = Vec::new();
            for (re_string, sub) in s.pattern_properties.iter().flatten() {
                match compile_regex(re_string) {
                    Ok(re) => pattern_properties.push((re, *sub)),
                    Err(e) => addf(&mut errs, format!("patternProperties[{}]: {e}", quote(re_string))),
                }
            }
            let info = &mut self.infos[id];
            info.pattern = pattern;
            info.pattern_properties = pattern_properties;
        }
        if errs.is_empty() {
            Ok(())
        } else {
            Err(errs.join("\n"))
        }
    }

    fn set_anchor(&mut self, schema: Id, base: Id, anchor: &str, dynamic: bool) {
        if anchor.is_empty() {
            return;
        }
        // The first schema to claim an anchor keeps it.
        self.infos[base]
            .anchors
            .entry(anchor.to_string())
            .or_insert(Anchor { schema, dynamic });
    }

    /// Works out each schema's base and URI from the `$id`s above it, and
    /// records the anchors.
    fn resolve_uris(&mut self) -> Result<(), String> {
        self.infos[0].uri = Some(Uri::default());
        self.resolved_uris.insert(String::new(), 0);
        let mut stack = vec![(0usize, 0usize)];
        while let Some((id, mut base)) = stack.pop() {
            let (sid, has_ref, anchor, dynamic_anchor) = {
                let s = &self.schemas[id];
                (s.id.clone(), !s.ref_.is_empty(), s.anchor.clone(), s.dynamic_anchor.clone())
            };
            // In draft-07 a $ref makes its siblings, $id included, mean
            // nothing.
            if !sid.is_empty() && !(self.draft == Draft::Draft7 && has_ref) {
                let id_uri = Uri::parse(&sid)?;
                if self.draft == Draft::Draft2020 && !id_uri.fragment.is_empty() {
                    return Err(format!("$id {sid} must not have a fragment"));
                }
                if self.draft == Draft::Draft7 && !id_uri.fragment.is_empty() {
                    let name = sid.strip_prefix('#').unwrap_or(&sid).to_string();
                    self.set_anchor(id, base, &name, false);
                } else {
                    let base_uri = self.infos[base].uri.clone().unwrap_or_default();
                    let uri = base_uri.resolve(&id_uri);
                    if !uri.is_abs() {
                        return Err(format!(
                            "$id {sid} does not resolve to an absolute URI (base is {})",
                            quote(&base_uri.to_string())
                        ));
                    }
                    self.resolved_uris.insert(uri.to_string(), id);
                    self.infos[id].uri = Some(uri);
                    base = id; // the scope for anchors
                }
            }
            self.infos[id].base = base;
            if self.draft == Draft::Draft2020 {
                self.set_anchor(id, base, &anchor, false);
                self.set_anchor(id, base, &dynamic_anchor, true);
            }
            for (_, child) in children(&self.schemas[id]).into_iter().rev() {
                stack.push((child, base));
            }
        }
        Ok(())
    }

    fn resolve_refs(&mut self, order: &[Id]) -> Result<(), String> {
        for &id in order {
            let (reference, dynamic) = {
                let s = &self.schemas[id];
                (s.ref_.clone(), s.dynamic_ref.clone())
            };
            if !reference.is_empty() {
                let (target, _) = self.resolve_ref(id, &reference)?;
                self.infos[id].resolved_ref = Some(target);
            }
            if !dynamic.is_empty() {
                let (target, frag) = self.resolve_ref(id, &dynamic)?;
                if frag.is_empty() {
                    self.infos[id].resolved_dynamic_ref = Some(target);
                } else {
                    self.infos[id].dynamic_ref_anchor = frag;
                }
            }
        }
        Ok(())
    }

    /// Finds the schema a reference names. The second value is the
    /// fragment when it names a dynamic anchor.
    fn resolve_ref(&self, s: Id, reference: &str) -> Result<(Id, String), String> {
        let ref_uri = Uri::parse(reference)?;
        let base = self.infos[s].base;
        let ref_uri = self.infos[base].uri.clone().unwrap_or_default().resolve(&ref_uri);
        let fragless = ref_uri.without_fragment().to_string();
        let Some(&referenced) = self.resolved_uris.get(&fragless) else {
            return Err(format!(
                "loading {fragless}: cannot resolve remote schemas: no loader passed to Schema.Resolve"
            ));
        };
        let frag = &ref_uri.fragment;
        if !frag.is_empty() && !frag.starts_with('/') {
            let Some(anchor) = self.infos[referenced].anchors.get(frag) else {
                return Err(format!("no anchor {} in {}", quote(frag), self.schemas[s].name()));
            };
            let dynamic = if anchor.dynamic { frag.clone() } else { String::new() };
            return Ok((anchor.schema, dynamic));
        }
        let target = self
            .dereference(referenced, frag)
            .map_err(|e| format!("JSON Pointer {}: {}", quote(frag), e))?;
        Ok((target, String::new()))
    }

    /// Follows a JSON Pointer from a schema to the schema it names.
    fn dereference(&self, from: Id, pointer: &str) -> Result<Id, String> {
        enum At<'a> {
            Schema(Id),
            Nil,
            List(&'a [Id]),
            Map(Option<&'a BTreeMap<String, Id>>),
            Other(&'static str),
        }
        if pointer.is_empty() {
            return Ok(from);
        }
        let segments: Vec<String> = pointer[1..]
            .split('/')
            .map(|seg| seg.replace("~1", "/").replace("~0", "~"))
            .collect();
        let mut at = At::Schema(from);
        for seg in &segments {
            at = match at {
                At::Nil => return Err("navigated to nil reference".to_string()),
                At::Schema(id) => {
                    let s = &self.schemas[id];
                    let mut found = None;
                    for (name, field) in schema_fields(s) {
                        if name != seg {
                            continue;
                        }
                        // "items" is listed twice: the single schema wins
                        // when there is one.
                        match field {
                            Field::One(Some(id)) => {
                                found = Some(At::Schema(id));
                                break;
                            }
                            Field::One(None) => found = Some(At::Nil),
                            Field::List(l) => found = Some(At::List(l)),
                            Field::Map(m) => found = Some(At::Map(m)),
                        }
                    }
                    match found {
                        Some(at) => at,
                        None if seg == "type" => At::Other(if s.types.is_some() { "[]string" } else { "string" }),
                        None => match other_field_type(seg) {
                            Some(t) => At::Other(t),
                            None => return Err(format!("no schema field {}", quote(seg))),
                        },
                    }
                }
                At::List(l) => {
                    if seg == "-" {
                        return Err("the JSON Pointer array segment '-' is not supported".to_string());
                    }
                    if seg.len() > 1 && seg.starts_with('0') {
                        return Err(format!("segment {} has leading zeroes", quote(seg)));
                    }
                    let digits = seg.strip_prefix(['+', '-']).unwrap_or(seg);
                    let n: i64 = if !digits.is_empty() && digits.bytes().all(|c| c.is_ascii_digit()) {
                        seg.parse().map_err(|_| format!("invalid int: {}", quote(seg)))?
                    } else {
                        return Err(format!("invalid int: {}", quote(seg)));
                    };
                    if n < 0 || n >= l.len() as i64 {
                        return Err(format!("index {} is out of bounds for array of length {}", n, l.len()));
                    }
                    At::Schema(l[n as usize])
                }
                At::Map(m) => match m.and_then(|m| m.get(seg)) {
                    Some(id) => At::Schema(*id),
                    None => return Err(format!("no key {} in map", quote(seg))),
                },
                At::Other(t) => return Err(format!("value of type {t} is not a schema, slice or map")),
            };
        }
        match at {
            At::Schema(id) => Ok(id),
            At::Nil => Err("navigated to nil reference".to_string()),
            At::List(_) => Err("does not refer to a schema, but to a []*jsonschema.Schema".to_string()),
            At::Map(_) => Err("does not refer to a schema, but to a map[string]*jsonschema.Schema".to_string()),
            At::Other(t) => Err(format!("does not refer to a schema, but to a {t}")),
        }
    }

    /// Whether the schema is `false`, or `{"not": {}}` spelt out.
    pub(crate) fn is_false(&self, id: Id) -> bool {
        self.schemas[id].not.is_some_and(|not| !self.schemas[not].nonzero)
    }

    pub(crate) fn version(&self) -> &str {
        &self.schemas[0].schema
    }

    pub(crate) fn valid_version(&self) -> bool {
        matches!(self.version(), "" | DRAFT7 | DRAFT7_SECURE | DRAFT2020)
    }
}
