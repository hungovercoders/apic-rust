use std::collections::{BTreeMap, BTreeSet};

use apic_gocompat::quote;
use serde_json::{Map, Value};

use super::resolve::{Draft, Info, Resolved};
use super::show::{display, display_list, display_text, display_value, quoted_list, rat_string};
use super::{Id, Schema};

/// What a schema and the schemas it applied in place (`allOf`, `$ref` and
/// the like) evaluated of one instance, for `unevaluatedItems` and
/// `unevaluatedProperties`.
#[derive(Default)]
struct Annotations {
    /// All items were evaluated.
    all_items: bool,
    /// One past the largest index evaluated by `prefixItems`.
    end_index: usize,
    /// The indexes evaluated by `contains`.
    evaluated_indexes: BTreeSet<usize>,
    /// All properties were evaluated.
    all_properties: bool,
    evaluated_properties: BTreeSet<String>,
}

impl Annotations {
    fn merge(&mut self, other: Annotations) {
        self.all_items |= other.all_items;
        self.end_index = self.end_index.max(other.end_index);
        self.evaluated_indexes.extend(other.evaluated_indexes);
        self.all_properties |= other.all_properties;
        self.evaluated_properties.extend(other.evaluated_properties);
    }
}

/// How many schemas may be applied inside one another.
const MAX_DEPTH: usize = 1024;

/// The stack validation runs on: room for MAX_DEPTH nested schemas.
const STACK_SIZE: usize = 128 << 20;

/// The state of one validation: the schemas being validated against,
/// outermost first. These are the scopes a dynamic reference looks in.
struct State<'a> {
    rs: &'a Resolved,
    stack: Vec<Id>,
}

fn number(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        _ => None,
    }
}

fn json_type(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(n) => {
            if n.as_f64().is_some_and(|f| f.fract() == 0.0) {
                "integer"
            } else {
                "number"
            }
        }
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Whether two JSON values are the same value: numbers by what they are
/// worth, objects whatever the order of their keys.
fn equal(x: &Value, y: &Value) -> bool {
    match (x, y) {
        (Value::Number(_), Value::Number(_)) => number(x) == number(y),
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(x, y)| equal(x, y))
        }
        (Value::Object(a), Value::Object(b)) => {
            a.len() == b.len() && a.iter().all(|(k, x)| b.get(k).is_some_and(|y| equal(x, y)))
        }
        (Value::String(a), Value::String(b)) => a == b,
        (Value::Bool(a), Value::Bool(b)) => a == b,
        (Value::Null, Value::Null) => true,
        _ => false,
    }
}

impl Resolved {
    /// Validates a JSON value against the schema. The error says where in
    /// the schema the value failed and why.
    pub fn validate(&self, instance: &Value) -> Result<(), String> {
        if !self.valid_version() {
            return Err(format!(
                "cannot validate version {}, supported versions: draft-07 and draft 2020-12",
                self.version()
            ));
        }
        // Schemas nest and refer to one another, so validating recurses;
        // a thread with a stack sized for the deepest nesting allowed
        // keeps that off the caller's stack.
        let run = || {
            let mut st = State {
                rs: self,
                stack: Vec::new(),
            };
            st.validate(instance, 0, None)
        };
        std::thread::scope(|scope| {
            match std::thread::Builder::new()
                .stack_size(STACK_SIZE)
                .spawn_scoped(scope, run)
            {
                Ok(handle) => handle
                    .join()
                    .unwrap_or_else(|_| Err("the schema could not be validated".to_string())),
                Err(e) => Err(format!("the schema could not be validated: {e}")),
            }
        })
    }
}

impl State<'_> {
    fn validate(
        &mut self,
        instance: &Value,
        schema: Id,
        caller: Option<&mut Annotations>,
    ) -> Result<(), String> {
        // A schema that refers to itself without consuming any of the
        // instance would never finish.
        if self.stack.len() >= MAX_DEPTH {
            return Err("the schema refers to itself without end".to_string());
        }
        self.stack.push(schema);
        let result = self.check(instance, schema, caller);
        self.stack.pop();
        result.map_err(|e| format!("validating {}: {}", self.rs.schema_string(schema), e))
    }

    fn valid(&mut self, instance: &Value, schema: Id, anns: Option<&mut Annotations>) -> bool {
        self.validate(instance, schema, anns).is_ok()
    }

    fn names(&self, ids: &[Id]) -> String {
        let names: Vec<String> = ids.iter().map(|&id| self.rs.schemas[id].name()).collect();
        format!("[{}]", names.join(" "))
    }

    fn check(
        &mut self,
        instance: &Value,
        id: Id,
        caller: Option<&mut Annotations>,
    ) -> Result<(), String> {
        let rs = self.rs;
        let schema = &rs.schemas[id];
        let info = &rs.infos[id];
        // All the annotations for this call and the calls under it.
        let mut anns = Annotations::default();

        if !schema.ref_.is_empty() {
            if let Some(target) = info.resolved_ref {
                self.validate(instance, target, Some(&mut anns))?;
            }
            // In draft-07 everything beside a $ref is ignored.
            if rs.draft == Draft::Draft7 {
                return Ok(());
            }
        }

        check_type(schema, instance)?;
        check_values(schema, instance)?;
        if let Some(n) = number(instance) {
            check_number(schema, n)?;
        }
        if let Value::String(str) = instance {
            check_string(schema, info, str)?;
        }
        self.check_dynamic_ref(instance, id, &mut anns)?;
        // Logic. These come before arrays and objects because an item or
        // property one of them evaluates is not left for
        // unevaluatedItems or unevaluatedProperties.
        self.check_logic(instance, id, &mut anns)?;
        if let Value::Array(items) = instance {
            self.check_array(instance, items, id, &mut anns)?;
        }
        if let Value::Object(object) = instance {
            self.check_object(instance, object, id, &mut anns)?;
        }

        if let Some(caller) = caller {
            // The caller wants to know what was validated here.
            caller.merge(anns);
        }
        Ok(())
    }

    /// `$dynamicRef`.
    fn check_dynamic_ref(
        &mut self,
        instance: &Value,
        id: Id,
        anns: &mut Annotations,
    ) -> Result<(), String> {
        let rs = self.rs;
        let info = &rs.infos[id];
        if rs.schemas[id].dynamic_ref.is_empty() {
            return Ok(());
        }
        if let Some(target) = info.resolved_dynamic_ref {
            // The same as $ref.
            return self.validate(instance, target, Some(anns));
        }
        // Look for the base of the outermost schema on the stack
        // with this dynamic anchor: the one farthest from here.
        // The base, because it is the scope for anchors; the
        // schema found need not be on the stack itself.
        let target = self.stack.iter().find_map(|&s| {
            let base = rs.infos[s].base;
            rs.infos[base]
                .anchors
                .get(&info.dynamic_ref_anchor)
                .filter(|a| a.dynamic)
                .map(|a| a.schema)
        });
        let Some(target) = target else {
            return Err(format!(
                "missing dynamic anchor {}",
                quote(&info.dynamic_ref_anchor)
            ));
        };
        self.validate(instance, target, Some(anns))
    }

    /// `allOf`, `anyOf`, `oneOf`, `not` and `if`.
    fn check_logic(
        &mut self,
        instance: &Value,
        id: Id,
        anns: &mut Annotations,
    ) -> Result<(), String> {
        let rs = self.rs;
        let schema = &rs.schemas[id];
        if let Some(all) = &schema.all_of {
            for &ss in all {
                self.validate(instance, ss, Some(anns))?;
            }
        }
        if let Some(any) = &schema.any_of {
            // All are visited, to collect annotations.
            let mut errs = Vec::new();
            for &ss in any {
                if let Err(e) = self.validate(instance, ss, Some(anns)) {
                    errs.push(e);
                }
            }
            if errs.len() == any.len() {
                return Err(format!(
                    "anyOf: did not validate against any of {}:\n{}",
                    self.names(any),
                    errs.join("\n")
                ));
            }
        }
        if let Some(one) = &schema.one_of {
            // Exactly one.
            let mut ok: Option<Id> = None;
            for &ss in one {
                if self.valid(instance, ss, Some(anns)) {
                    if let Some(first) = ok {
                        return Err(format!(
                            "oneOf: validated against both {} and {}",
                            rs.schemas[first].name(),
                            rs.schemas[ss].name()
                        ));
                    }
                    ok = Some(ss);
                }
            }
            if ok.is_none() {
                return Err(format!(
                    "oneOf: did not validate against any of {}",
                    self.names(one)
                ));
            }
        }
        if let Some(not) = schema.not {
            // Annotations from "not" are ignored.
            if self.valid(instance, not, None) {
                return Err(format!("not: validated against {}", rs.schemas[not].name()));
            }
        }
        if let Some(cond) = schema.if_ {
            let branch = if self.valid(instance, cond, Some(anns)) {
                schema.then
            } else {
                schema.else_
            };
            if let Some(ss) = branch {
                self.validate(instance, ss, Some(anns))?;
            }
        }
        Ok(())
    }

    /// The keywords for an array.
    fn check_array(
        &mut self,
        instance: &Value,
        items: &[Value],
        id: Id,
        anns: &mut Annotations,
    ) -> Result<(), String> {
        let schema = &self.rs.schemas[id];
        self.check_items(items, id, anns)?;
        self.check_contains(instance, items, id, anns)?;
        let len = items.len() as i64;
        if let Some(m) = schema.min_items.filter(|&m| len < m) {
            return Err(format!("minItems: array length {len} is less than {m}"));
        }
        if let Some(m) = schema.max_items.filter(|&m| len > m) {
            return Err(format!("maxItems: array length {len} is greater than {m}"));
        }
        if schema.unique_items {
            for (i, item) in items.iter().enumerate() {
                if let Some(j) = items[..i].iter().position(|earlier| equal(item, earlier)) {
                    return Err(format!("uniqueItems: array items {i} and {j} are equal"));
                }
            }
        }
        if let Some(unevaluated) = schema.unevaluated_items {
            if !anns.all_items {
                // Every item nothing has validated yet, the in-place
                // schemas such as allOf included.
                for (i, item) in items.iter().enumerate().skip(anns.end_index) {
                    if !anns.evaluated_indexes.contains(&i) {
                        self.validate(item, unevaluated, None)?;
                    }
                }
                anns.all_items = true;
            }
        }
        Ok(())
    }

    /// `items` and the keywords beside it that say which schema each item
    /// of an array is validated against.
    fn check_items(
        &mut self,
        items: &[Value],
        id: Id,
        anns: &mut Annotations,
    ) -> Result<(), String> {
        let rs = self.rs;
        let schema = &rs.schemas[id];
        // The items are instances in their own right: validating them
        // collects no annotations for this one.
        if rs.draft == Draft::Draft7 {
            // additionalItems applies after an items array, and is
            // ignored beside a single items schema or none.
            if let Some(array) = &schema.items_array {
                for (item, &ischema) in items.iter().zip(array) {
                    self.validate(item, ischema, None)?;
                }
                anns.end_index = anns.end_index.max(array.len().min(items.len()));
                if let Some(additional) = schema.additional_items {
                    for item in items.iter().skip(array.len()) {
                        self.validate(item, additional, None)?;
                    }
                    anns.all_items = true;
                }
            } else if let Some(ischema) = schema.items {
                for item in items {
                    self.validate(item, ischema, None)?;
                }
                anns.all_items = true;
            }
        } else {
            // items applies after prefixItems.
            for (item, &ischema) in items.iter().zip(&schema.prefix_items) {
                self.validate(item, ischema, None)?;
            }
            anns.end_index = anns
                .end_index
                .max(schema.prefix_items.len().min(items.len()));
            if let Some(ischema) = schema.items {
                for item in items.iter().skip(schema.prefix_items.len()) {
                    self.validate(item, ischema, None)?;
                }
                anns.all_items = true;
            }
        }
        Ok(())
    }

    /// `contains`, `minContains` and `maxContains`.
    fn check_contains(
        &mut self,
        instance: &Value,
        items: &[Value],
        id: Id,
        anns: &mut Annotations,
    ) -> Result<(), String> {
        let rs = self.rs;
        let schema = &rs.schemas[id];
        let Some(contains) = schema.contains else {
            return Ok(());
        };
        let mut n_contains = 0i64;
        for (i, item) in items.iter().enumerate() {
            if self.valid(item, contains, None) {
                n_contains += 1;
                anns.evaluated_indexes.insert(i);
            }
        }
        if n_contains == 0 && schema.min_contains.is_none_or(|m| m > 0) {
            return Err(format!(
                "contains: {} does not have an item matching {}",
                display_text(instance),
                rs.schemas[contains].name()
            ));
        }
        if let Some(m) = schema.min_contains.filter(|&m| n_contains < m) {
            return Err(format!(
                "minContains: contains validated {n_contains} items, less than {m}"
            ));
        }
        if let Some(m) = schema.max_contains.filter(|&m| n_contains > m) {
            return Err(format!(
                "maxContains: contains validated {n_contains} items, greater than {m}"
            ));
        }
        Ok(())
    }

    /// The keywords for an object.
    fn check_object(
        &mut self,
        instance: &Value,
        object: &Map<String, Value>,
        id: Id,
        anns: &mut Annotations,
    ) -> Result<(), String> {
        let schema = &self.rs.schemas[id];
        let eval_props = self.check_properties(object, id)?;
        anns.evaluated_properties.extend(eval_props);
        if let Some(names) = schema.property_names {
            for prop in object.keys() {
                self.validate(&Value::String(prop.clone()), names, None)?;
            }
        }

        let n = object.len() as i64;
        if let Some(m) = schema.min_properties.filter(|&m| n < m) {
            return Err(format!(
                "minProperties: object has {n} properties, less than {m}"
            ));
        }
        if let Some(m) = schema.max_properties.filter(|&m| n > m) {
            return Err(format!(
                "maxProperties: object has {n} properties, greater than {m}"
            ));
        }

        if let Some(required) = &schema.required {
            let m = missing(object, required);
            if !m.is_empty() {
                return Err(format!("required: missing properties: {}", quoted_list(&m)));
            }
        }
        self.check_dependencies(instance, object, id, anns)?;

        if let Some(unevaluated) = schema.unevaluated_properties {
            if !anns.all_properties {
                // Like additionalProperties, but going by the in-place
                // keywords such as allOf as well as the siblings.
                for (prop, val) in object {
                    if !anns.evaluated_properties.contains(prop) {
                        self.validate(val, unevaluated, None)?;
                    }
                }
                // After this every property has been validated.
                anns.all_properties = true;
            }
        }
        Ok(())
    }

    /// `properties`, `patternProperties` and `additionalProperties`.
    /// Returns the properties this schema itself evaluated, which is what
    /// additionalProperties goes by: not those of allOf and so on.
    fn check_properties(
        &mut self,
        object: &Map<String, Value>,
        id: Id,
    ) -> Result<BTreeSet<String>, String> {
        let rs = self.rs;
        let schema = &rs.schemas[id];
        let info = &rs.infos[id];
        let mut eval_props: BTreeSet<String> = BTreeSet::new();
        for (prop, &sub) in schema.properties.iter().flatten() {
            // A property the instance does not have is fine.
            let Some(val) = object.get(prop) else {
                continue;
            };
            self.validate(val, sub, None)?;
            eval_props.insert(prop.clone());
        }
        if !info.pattern_properties.is_empty() {
            for (prop, val) in object {
                // Every pattern that matches applies.
                for (re, sub) in &info.pattern_properties {
                    if re.is_match(prop) {
                        self.validate(val, *sub, None)?;
                        eval_props.insert(prop.clone());
                    }
                }
            }
        }
        if let Some(additional) = schema.additional_properties {
            if rs.is_false(additional) {
                // One message naming every property that is not
                // allowed reads better than the first one's failure.
                let disallowed: Vec<String> = object
                    .keys()
                    .filter(|k| !eval_props.contains(*k))
                    .cloned()
                    .collect();
                if !disallowed.is_empty() {
                    return Err(format!(
                        "unexpected additional properties {}",
                        quoted_list(&disallowed)
                    ));
                }
            } else {
                for (prop, val) in object {
                    if !eval_props.contains(prop) {
                        self.validate(val, additional, None)?;
                        eval_props.insert(prop.clone());
                    }
                }
            }
        }
        Ok(eval_props)
    }

    /// `dependentRequired` and `dependentSchemas`, or draft-07's
    /// `dependencies`.
    fn check_dependencies(
        &mut self,
        instance: &Value,
        object: &Map<String, Value>,
        id: Id,
        anns: &mut Annotations,
    ) -> Result<(), String> {
        let rs = self.rs;
        let schema = &rs.schemas[id];
        let empty_strings = BTreeMap::new();
        let (dep_required, dep_schemas) = if rs.draft == Draft::Draft7 {
            (&schema.dependency_strings, &schema.dependency_schemas)
        } else {
            (&schema.dependent_required, &schema.dependent_schemas)
        };
        // For each name both in the instance and under the keyword,
        // every property it lists must be in the instance too.
        for (dprop, reqs) in dep_required.as_ref().unwrap_or(&empty_strings) {
            if object.contains_key(dprop) {
                let m = missing(object, reqs);
                if !m.is_empty() {
                    return Err(format!(
                        "dependentRequired[{}]: missing properties {}",
                        quote(dprop),
                        quoted_list(&m)
                    ));
                }
            }
        }
        for (dprop, &ss) in dep_schemas.iter().flatten() {
            if object.contains_key(dprop) {
                self.validate(instance, ss, Some(anns))?;
            }
        }
        Ok(())
    }
}

/// The properties of `props` the object does not have.
fn missing(object: &Map<String, Value>, props: &[String]) -> Vec<String> {
    props
        .iter()
        .filter(|p| !object.contains_key(*p))
        .cloned()
        .collect()
}

/// `type`.
fn check_type(schema: &Schema, instance: &Value) -> Result<(), String> {
    if schema.type_.is_empty() && schema.types.is_none() {
        return Ok(());
    }
    let got = json_type(instance);
    if !schema.type_.is_empty() {
        // "number" takes integers too.
        if !(got == schema.type_ || got == "integer" && schema.type_ == "number") {
            return Err(format!(
                "type: {} has type {}, want {}",
                display(Some(instance)),
                quote(got),
                quote(&schema.type_)
            ));
        }
    } else if let Some(types) = &schema.types {
        let has = |t: &str| types.iter().any(|x| x == t);
        if !(has(got) || got == "integer" && has("number")) {
            return Err(format!(
                "type: {} has type {}, want one of {}",
                display(Some(instance)),
                quote(got),
                quote(&types.join(", "))
            ));
        }
    }
    Ok(())
}

/// `enum` and `const`.
fn check_values(schema: &Schema, instance: &Value) -> Result<(), String> {
    if let Some(values) = &schema.enum_ {
        if !values.iter().any(|e| equal(e, instance)) {
            return Err(format!(
                "enum: {} does not equal any of: {}",
                display(Some(instance)),
                display_list(values)
            ));
        }
    }
    if let Some(want) = &schema.const_ {
        if !equal(want, instance) {
            return Err(format!(
                "const: {} does not equal {}",
                display(Some(instance)),
                display_value(want)
            ));
        }
    }
    Ok(())
}

/// The keywords for a number.
fn check_number(schema: &Schema, n: f64) -> Result<(), String> {
    if let Some(m) = schema.multiple_of {
        if (n / m).fract() != 0.0 {
            return Err(format!(
                "multipleOf: {} is not a multiple of {:.6}",
                rat_string(n),
                m
            ));
        }
    }
    if let Some(m) = schema.minimum.filter(|&m| n < m) {
        return Err(format!("minimum: {} is less than {:.6}", rat_string(n), m));
    }
    if let Some(m) = schema.maximum.filter(|&m| n > m) {
        return Err(format!(
            "maximum: {} is greater than {:.6}",
            rat_string(n),
            m
        ));
    }
    if let Some(m) = schema.exclusive_minimum.filter(|&m| n <= m) {
        return Err(format!(
            "exclusiveMinimum: {} is less than or equal to {:.6}",
            rat_string(n),
            m
        ));
    }
    if let Some(m) = schema.exclusive_maximum.filter(|&m| n >= m) {
        return Err(format!(
            "exclusiveMaximum: {} is greater than or equal to {:.6}",
            rat_string(n),
            m
        ));
    }
    Ok(())
}

/// The keywords for a string.
fn check_string(schema: &Schema, info: &Info, str: &str) -> Result<(), String> {
    let n = str.chars().count() as i64;
    if let Some(m) = schema.min_length.filter(|&m| n < m) {
        return Err(format!(
            "minLength: {} contains {} Unicode code points, fewer than {}",
            quote(str),
            n,
            m
        ));
    }
    if let Some(m) = schema.max_length.filter(|&m| n > m) {
        return Err(format!(
            "maxLength: {} contains {} Unicode code points, more than {}",
            quote(str),
            n,
            m
        ));
    }
    if let Some(re) = &info.pattern {
        if !re.is_match(str) {
            return Err(format!(
                "pattern: {} does not match regular expression {}",
                quote(str),
                quote(&schema.pattern)
            ));
        }
    }
    Ok(())
}
