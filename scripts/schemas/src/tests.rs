use std::path::PathBuf;

use regex::Regex;
use serde_json::{json, Value};

use crate::schema::Schema;
use crate::{config_schema, files, DIRS};

fn root() -> PathBuf {
    PathBuf::from(format!("{}/../..", env!("CARGO_MANIFEST_DIR")))
}

/// Fails when the committed schemas differ from what the generator
/// produces, so a new apic.yaml key cannot land without `task schemas`.
#[test]
fn schemas_are_current() {
    for (name, s) in files().unwrap() {
        let want = s.render();
        for dir in DIRS {
            let path = root().join(dir).join(name);
            let got = std::fs::read_to_string(&path)
                .unwrap_or_else(|err| panic!("{dir}/{name}: {err} (run `task schemas`)"));
            // A checkout with autocrlf (the Windows runner) turns the
            // committed LF file into CRLF; that is not staleness.
            assert!(
                got.replace("\r\n", "\n") == want,
                "{dir}/{name} is stale; run `task schemas`"
            );
        }
    }
}

/// A validator for the keywords the generated schemas use, resolving
/// references against the schema it was made for.
struct Resolved {
    root: Schema,
}

impl Resolved {
    fn validate(&self, v: &Value) -> Result<(), String> {
        self.check(&self.root, v, "")
    }

    fn check(&self, s: &Schema, v: &Value, at: &str) -> Result<(), String> {
        if !s.reference.is_empty() {
            let name = s
                .reference
                .strip_prefix("#/$defs/")
                .ok_or_else(|| format!("unsupported reference {}", s.reference))?;
            let target = self
                .root
                .defs
                .get(name)
                .ok_or_else(|| format!("no definition {name}"))?;
            self.check(target, v, at)?;
        }
        let types: Vec<&str> = if s.ty.is_empty() {
            s.types.clone()
        } else {
            vec![s.ty]
        };
        if !types.is_empty() && !types.iter().any(|t| has_type(v, t)) {
            return Err(format!("{at}: {v} is not of type {}", types.join(" or ")));
        }
        if !s.enumeration.is_empty() && !s.enumeration.contains(v) {
            return Err(format!("{at}: {v} is not one of the allowed values"));
        }
        if let (Some(min), Some(n)) = (s.minimum, v.as_f64()) {
            if n < min {
                return Err(format!("{at}: {n} is below {min}"));
            }
        }
        if let (false, Some(text)) = (s.pattern.is_empty(), v.as_str()) {
            if !Regex::new(s.pattern).unwrap().is_match(text) {
                return Err(format!("{at}: {text:?} does not match {}", s.pattern));
            }
        }
        if let (Some(items), Some(list)) = (&s.items, v.as_array()) {
            for (i, item) in list.iter().enumerate() {
                self.check(items, item, &format!("{at}/{i}"))?;
            }
        }
        if let Some(object) = v.as_object() {
            for name in &s.required {
                if !object.contains_key(*name) {
                    return Err(format!("{at}: {name} is required"));
                }
            }
            for (key, member) in object {
                let here = format!("{at}/{key}");
                if let Some(names) = &s.property_names {
                    self.check(names, &Value::from(key.as_str()), &here)?;
                }
                match s.properties.as_ref().and_then(|p| p.get(key)) {
                    Some(prop) => self.check(prop, member, &here)?,
                    None => {
                        if let Some(extra) = &s.additional_properties {
                            self.check(extra, member, &here)?;
                        }
                    }
                }
            }
        }
        if !s.one_of.is_empty() {
            let passing = s
                .one_of
                .iter()
                .filter(|alt| self.check(alt, v, at).is_ok())
                .count();
            if passing != 1 {
                return Err(format!(
                    "{at}: matches {passing} of the alternatives, not one"
                ));
            }
        }
        if let Some(not) = &s.not {
            if self.check(not, v, at).is_ok() {
                return Err(format!("{at}: not allowed here"));
            }
        }
        Ok(())
    }
}

fn has_type(v: &Value, ty: &str) -> bool {
    match ty {
        "object" => v.is_object(),
        "array" => v.is_array(),
        "string" => v.is_string(),
        "boolean" => v.is_boolean(),
        "null" => v.is_null(),
        "number" => v.is_number(),
        "integer" => v.as_f64().is_some_and(|n| n.fract() == 0.0),
        _ => false,
    }
}

/// Returns a schema ready to validate against.
fn resolved(name: &str) -> Resolved {
    Resolved {
        root: files().unwrap().remove(name).expect("a schema"),
    }
}

fn example_configs() -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(root().join("examples"))
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.path().join("apic.yaml"))
        .filter(|p| p.is_file())
        .collect();
    paths.sort();
    paths
}

/// Validates every apic.yaml under examples/ and the demo project.
#[test]
fn config_schema_accepts_the_projects_in_the_repo() {
    let rs = resolved("apic.schema.json");
    let mut paths = example_configs();
    paths.push(root().join("crates/demoapi/project/apic.yaml"));
    assert!(paths.len() >= 2, "expected example projects, got {paths:?}");
    for p in paths {
        let data = std::fs::read_to_string(&p).unwrap();
        let v: Value =
            serde_yaml_ng::from_str(&data).unwrap_or_else(|err| panic!("{}: {err}", p.display()));
        if let Err(err) = rs.validate(&v) {
            panic!("{} does not validate: {err}", p.display());
        }
    }
}

#[test]
fn config_schema_rejects_unknown_and_bad_values() {
    let rs = resolved("apic.schema.json");
    let good = json!({
        "env": "dev", "dir": "api", "timeout": "1m30s", "maxBodyBytes": 1024,
        "auth": {"default": "bearer {{token}}", "allowExec": true},
        "test": {"paths": ["features"]},
    });
    if let Err(err) = rs.validate(&good) {
        panic!("a full config should validate: {err}");
    }
    for (name, bad) in [
        ("misspelled key", json!({"envv": "dev"})),
        ("nested unknown key", json!({"auth": {"deflt": "x"}})),
        ("bad duration", json!({"timeout": "soon"})),
        ("negative bytes", json!({"maxBodyBytes": -1})),
        ("paths not a list", json!({"test": {"paths": "features"}})),
    ] {
        assert!(
            rs.validate(&bad).is_err(),
            "{name} should not validate: {bad}"
        );
    }
}

#[test]
fn env_schema() {
    let rs = resolved("http-client.env.schema.json");
    for p in [
        "examples/httpbin/http-client.env.json",
        "examples/spotify/http-client.private.env.json",
        "crates/demoapi/project/http-client.private.env.json",
    ] {
        let data = std::fs::read_to_string(root().join(p)).unwrap();
        let v: Value = serde_json::from_str(&data).unwrap();
        if let Err(err) = rs.validate(&v) {
            panic!("{p} does not validate: {err}");
        }
    }
    if let Err(err) = rs.validate(&json!({"dev": {"n": 1, "b": true, "z": null, "s": "x"}})) {
        panic!("scalar values should validate: {err}");
    }
    assert!(
        rs.validate(&json!({"dev": {"obj": {}}})).is_err(),
        "an object value should not validate"
    );
    assert!(
        rs.validate(&json!({"dev": "not an object"})).is_err(),
        "an environment must be an object"
    );
}

#[test]
fn session_schema() {
    let rs = resolved("session.schema.json");
    if let Err(err) = rs.validate(&json!({"envs": {"dev": {"token": "abc", "$oauth2:x": "{}"}}})) {
        panic!("a session should validate: {err}");
    }
    assert!(
        rs.validate(&json!({"vars": {}})).is_err(),
        "envs is required"
    );
}

#[test]
fn every_config_key_is_documented() {
    // The generator enforces this; pin that the keys carry their text.
    let s = config_schema().unwrap();
    let props = s.properties.as_ref().unwrap();
    for key in ["env", "dir", "timeout", "maxBodyBytes", "auth", "test"] {
        assert!(
            props.get(key).is_some_and(|p| !p.description.is_empty()),
            "{key} missing or undocumented"
        );
    }
    let auth = props["auth"].properties.as_ref().unwrap();
    assert!(
        auth["allowExec"].description.contains("exec"),
        "nested keys keep their descriptions"
    );
}
