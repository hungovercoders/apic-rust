//! Schemas and instances with the outcome recorded for each: whether the
//! schema loads, and what a value that does not match is told.

use serde_json::Value;

use crate::Resolved;

#[test]
fn reference_outcomes() {
    let data = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/testdata/schema_reference.json"
    ))
    .unwrap();
    let rows: Value = serde_json::from_str(&data).unwrap();
    let mut failures = Vec::new();
    let mut checked = 0;
    for row in rows.as_array().unwrap() {
        let field = |k: &str| row[k].as_str().unwrap();
        let (schema, want) = (field("Schema"), field("Err"));
        let resolved = Resolved::new(&serde_json::from_str(schema).unwrap());
        let got = if row["Load"].as_bool().unwrap() {
            resolved.err().unwrap_or_default()
        } else {
            let instance: Value = serde_json::from_str(field("Instance")).unwrap();
            resolved
                .unwrap()
                .validate(&instance)
                .err()
                .unwrap_or_default()
        };
        checked += 1;
        // A keyword of the wrong type names the type it wanted, which for
        // a schema is long; and a pattern that does not compile is
        // described by the engine. Both only have to fail.
        let worded_elsewhere =
            want.contains("of type struct {") || want.contains("error parsing regexp");
        if got != want && !(worded_elsewhere && !got.is_empty()) {
            failures.push(format!(
                "{schema} on {}:\n  got  {got:?}\n  want {want:?}",
                field("Instance")
            ));
        }
    }
    assert!(checked > 200, "only {checked} cases");
    assert!(
        failures.is_empty(),
        "{} differ:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn a_schema_that_only_refers_to_itself_fails_rather_than_running_away() {
    let rs = Resolved::new(&serde_json::json!({"$ref": "#"})).unwrap();
    let err = rs.validate(&serde_json::json!(1)).unwrap_err();
    assert!(
        err.ends_with("the schema refers to itself without end"),
        "{}",
        &err[err.len() - 60..]
    );
}
