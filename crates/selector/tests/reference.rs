//! Selections recorded from the reference behaviour: every selector form
//! against a handful of bodies, with the text, type and error each gives.

use std::time::Duration;

use apic_selector::{check, select_value, Headers, Response};

#[test]
fn reference_selections() {
    let data = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/testdata/reference.json"
    ))
    .unwrap();
    let rows: serde_json::Value = serde_json::from_str(&data).unwrap();
    let headers = Headers::from_iter([
        ("X", "one"),
        ("x", "two"),
        ("Set-Cookie", "a=\"q v\"; Path=/"),
        ("Set-Cookie", "b = sp ; HttpOnly"),
        ("Set-Cookie", "c=x\"y"),
        ("Set-Cookie", "d=1;;;"),
    ]);
    let mut failures = Vec::new();
    for row in rows.as_array().unwrap() {
        let field = |k: &str| row[k].as_str().unwrap().to_string();
        let (body, sel) = (field("Body"), field("Sel"));
        let resp = Response {
            status: 204,
            status_text: "No Content".to_string(),
            headers: headers.clone(),
            body: body.clone().into_bytes(),
            duration: Duration::from_nanos(1_234_567_890),
        };
        let (text, kind, ok, err) = match select_value(&resp, &sel) {
            Ok(Some(v)) => (v.text, v.kind.to_string(), true, String::new()),
            Ok(None) => (String::new(), "string".to_string(), false, String::new()),
            Err(e) => (String::new(), "string".to_string(), false, e.to_string()),
        };
        let err = format!(
            "{err}|{}",
            check(&sel).err().map(|e| e.to_string()).unwrap_or_default()
        );
        let want_err = field("Err");
        // The reason a regex does not compile is worded by the engine.
        let same_err =
            err == want_err || (want_err.contains("bad regex") && err.contains("bad regex"));
        if text != field("Text")
            || kind != field("Kind")
            || ok != row["OK"].as_bool().unwrap()
            || !same_err
        {
            failures.push(format!(
                "{sel} on {body:?}:\n  got  {text:?} {kind} ok={ok} err={err:?}\n  want {:?} {} ok={} err={want_err:?}",
                field("Text"),
                field("Kind"),
                row["OK"]
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} differ:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
