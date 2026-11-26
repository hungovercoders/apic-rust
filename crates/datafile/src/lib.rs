//! Reads the rows of a data-driven run, `apic run --data`: a CSV file
//! whose header row names the columns, or a JSON array of objects. Each
//! row becomes the variables of one iteration.

#![forbid(unsafe_code)]

mod csv;

use std::collections::BTreeMap;
use std::fmt;

use apic_gocompat::{check_json, format_float_json, json_string};
use serde_json::Value;

/// One iteration's variables, with the column order kept for display.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Row {
    /// The column names: in file order for CSV, sorted for JSON.
    pub names: Vec<String>,
    pub values: BTreeMap<String, String>,
}

/// Renders the row as `name=value` pairs, in column order.
impl fmt::Display for Row {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, n) in self.names.iter().enumerate() {
            if i > 0 {
                f.write_str(" ")?;
            }
            write!(f, "{n}={}", self.values.get(n).map_or("", String::as_str))?;
        }
        Ok(())
    }
}

/// Parses rows from `data`; `name` (the file's name, or "-" for stdin)
/// picks the format by extension, and anything but .csv that starts with
/// `[` is read as JSON.
pub fn read(data: &[u8], name: &str) -> Result<Vec<Row>, String> {
    let text = String::from_utf8_lossy(data);
    let trimmed = text.trim();
    let name = name.to_lowercase();
    if name.ends_with(".json") || (!name.ends_with(".csv") && trimmed.starts_with('[')) {
        return read_json(trimmed);
    }
    read_csv(&text)
}

fn read_csv(data: &str) -> Result<Vec<Row>, String> {
    // A spreadsheet's byte order mark.
    let mut r = csv::Reader::new(data.strip_prefix('\u{feff}').unwrap_or(data));
    let header = match r.read() {
        None => return Err("the file is empty; the first row names the variables".to_string()),
        Some(Err(err)) => return Err(format!("csv: {err}")),
        Some(Ok(header)) => header,
    };
    let header: Vec<String> = header.iter().map(|h| h.trim().to_string()).collect();
    if let Some(i) = header.iter().position(String::is_empty) {
        return Err(format!(
            "csv: column {} of the header row has no name",
            i + 1
        ));
    }
    let mut rows = Vec::new();
    while let Some(rec) = r.read() {
        let rec = rec.map_err(|err| format!("csv: {err}"))?;
        rows.push(Row {
            names: header.clone(),
            values: header.iter().cloned().zip(rec).collect(),
        });
    }
    Ok(rows)
}

fn read_json(data: &str) -> Result<Vec<Row>, String> {
    let wrap = |err: String| format!("json: want an array of objects: {err}");
    check_json(data.as_bytes()).map_err(wrap)?;
    let raw: Value = serde_json::from_str(data).map_err(|err| wrap(err.to_string()))?;
    let Value::Array(items) = raw else {
        return Err(wrap(mismatch(&raw, "[]map[string]interface {}")));
    };
    let mut rows = Vec::with_capacity(items.len());
    for obj in items {
        let fields = match obj {
            Value::Object(fields) => fields,
            Value::Null => serde_json::Map::new(),
            other => return Err(wrap(mismatch(&other, "map[string]interface {}"))),
        };
        // JSON objects have no order; the names are sorted so the heading
        // of an iteration is stable.
        let values: BTreeMap<String, String> = fields
            .into_iter()
            .map(|(k, v)| (k, stringify(&v)))
            .collect();
        rows.push(Row {
            names: values.keys().cloned().collect(),
            values,
        });
    }
    Ok(rows)
}

/// What is said of a value of the wrong kind.
fn mismatch(v: &Value, want: &str) -> String {
    let kind = match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    };
    format!("json: cannot unmarshal {kind} into Go value of type {want}")
}

/// Renders a JSON value as a variable: strings as they are, numbers
/// without an exponent, objects and arrays as JSON.
fn stringify(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.as_f64().map_or_else(|| n.to_string(), |f| f.to_string()),
        Value::Bool(b) => b.to_string(),
        Value::Null => String::new(),
        other => encode(other),
    }
}

/// Encodes a value as compact JSON, keys sorted.
fn encode(v: &Value) -> String {
    match v {
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.as_f64().map_or_else(|| n.to_string(), format_float_json),
        Value::String(s) => json_string(s, true),
        Value::Array(items) => {
            let items: Vec<String> = items.iter().map(encode).collect();
            format!("[{}]", items.join(","))
        }
        Value::Object(fields) => {
            let fields: Vec<String> = fields
                .iter()
                .map(|(k, v)| format!("{}:{}", json_string(k, true), encode(v)))
                .collect();
            format!("{{{}}}", fields.join(","))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_and_json() {
        let rows = read(
            "\u{feff}id, name\n1,\"Smith, J\"\n2,  x\n".as_bytes(),
            "rows.csv",
        )
        .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].values["name"], "Smith, J");
        assert_eq!(rows[1].values["name"], "x");
        assert_eq!(rows[0].to_string(), "id=1 name=Smith, J");

        let rows = read(
            br#"[{"id": 7, "big": 12345678901, "ok": true, "tags": ["a"], "none": null, "s": "x"}]"#,
            "-",
        )
        .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].to_string(),
            r#"big=12345678901 id=7 none= ok=true s=x tags=["a"]"#
        );

        let cases = [
            ("", "empty"),
            ("a,,b\n1,2,3\n", "column 2"),
            ("a,b\n1,2,3\n", "wrong number of fields"),
            ("[1, 2]", "array of objects"),
            (r#"{"id": 1}"#, ""),
        ];
        for (data, want) in cases {
            let name = if data.starts_with('[') || data.starts_with('{') {
                "x.json"
            } else {
                "x.csv"
            };
            let err = read(data.as_bytes(), name).expect_err(data);
            assert!(err.contains(want), "{data:?}: {err}, want {want:?}");
        }
    }

    #[test]
    fn json_values() {
        let rows = read(
            br#"[{"f": 1.50, "e": 1e21, "small": 1e-7, "neg": -0.25, "o": {"b": [1, 2.5, "<", 1e21, 1e-7], "a": null}}, null]"#,
            "rows.json",
        )
        .unwrap();
        assert_eq!(rows.len(), 2);
        let v = &rows[0].values;
        assert_eq!(v["f"], "1.5");
        assert_eq!(v["e"], "1000000000000000000000");
        assert_eq!(v["small"], "0.0000001");
        assert_eq!(v["neg"], "-0.25");
        assert_eq!(
            v["o"],
            concat!(r#"{"a":null,"b":[1,2.5,"\u"#, r#"003c",1e+21,1e-7]}"#)
        );
        assert!(rows[1].names.is_empty() && rows[1].values.is_empty());
        let rows = read(b"a,b\r\n1,2\r", "x.csv").unwrap();
        assert_eq!(rows[0].to_string(), "a=1 b=2");
    }

    #[test]
    fn error_messages() {
        let cases = [
            ("x.csv", "", "the file is empty; the first row names the variables"),
            ("x.csv", "a,b\n1,2,3\n", "csv: record on line 2: wrong number of fields"),
            ("x.csv", "a,b\n1,x\"y\n", "csv: parse error on line 2, column 4: bare \" in non-quoted-field"),
            ("x.csv", "a,b\n1,\"x\"y\n", "csv: parse error on line 2, column 5: extraneous or missing \" in quoted-field"),
            ("x.csv", "a,b\n1,\"x\ny", "csv: record on line 2; parse error on line 3, column 2: extraneous or missing \" in quoted-field"),
            ("x.csv", "a,b\n1,\"x\ny\n", "csv: record on line 2; parse error on line 3, column 3: extraneous or missing \" in quoted-field"),
            ("x.csv", "a,b\n1,\"x", "csv: parse error on line 2, column 5: extraneous or missing \" in quoted-field"),
            ("x.csv", "a,b\n 1 , \"x\" \n", "csv: parse error on line 2, column 8: extraneous or missing \" in quoted-field"),
            ("x.csv", "a,b\n1,  x\"\n", "csv: parse error on line 2, column 6: bare \" in non-quoted-field"),
            ("x.csv", "a,b\n   \n1,2\n", "csv: record on line 2: wrong number of fields"),
            ("x.json", "[1, 2]", "json: want an array of objects: json: cannot unmarshal number into Go value of type map[string]interface {}"),
            ("x.json", r#"{"id": 1}"#, "json: want an array of objects: json: cannot unmarshal object into Go value of type []map[string]interface {}"),
            ("x.json", "[", "json: want an array of objects: unexpected end of JSON input"),
            ("x.json", r#"["a"]"#, "json: want an array of objects: json: cannot unmarshal string into Go value of type map[string]interface {}"),
            ("x.json", r#"[{"a":1},]"#, "json: want an array of objects: invalid character ']' looking for beginning of value"),
            ("x.json", "[true]", "json: want an array of objects: json: cannot unmarshal bool into Go value of type map[string]interface {}"),
            ("x.json", r#""s""#, "json: want an array of objects: json: cannot unmarshal string into Go value of type []map[string]interface {}"),
        ];
        for (name, data, want) in cases {
            assert_eq!(read(data.as_bytes(), name).unwrap_err(), want, "{data:?}");
        }
    }
}
