//! GraphQL requests, written the way both editors write them: the request
//! line says `GRAPHQL` (JetBrains) or a header says `X-REQUEST-TYPE:
//! GraphQL` (REST Client), the body is the query, and an optional JSON
//! object after a blank line is the variables. apic sends them as a POST
//! with a `{"query", "variables"}` JSON body, which is what a GraphQL
//! server reads; the runner does that, this module only recognises the
//! shape.

use apic_gocompat::{check_json, compact_json, json_string};

use crate::Request;

/// The REST Client header that marks a GraphQL request. It is never sent
/// on the wire.
pub const GRAPHQL_HEADER: &str = "X-REQUEST-TYPE";

impl Request {
    /// Reports whether the request is written as a GraphQL query: the
    /// `GRAPHQL` method, or an `X-REQUEST-TYPE: GraphQL` header.
    pub fn is_graphql(&self) -> bool {
        if self.method == "GRAPHQL" {
            return true;
        }
        self.header(GRAPHQL_HEADER)
            .is_some_and(|v| v.trim().eq_ignore_ascii_case("GraphQL"))
    }

    /// Returns the query and the variables of a GraphQL request as
    /// written, and an error when the request has no query or the
    /// variables hold no placeholders and are not a JSON object. Both are
    /// empty when the body is a `< file`: the file is the query, and the
    /// runner reads it.
    pub fn graphql(&self) -> Result<(String, String), String> {
        if !self.body_file.is_empty() {
            return Ok((String::new(), String::new()));
        }
        let (query, variables) = split_graphql(&self.body);
        if query.is_empty() {
            return Err("a GraphQL request needs a query in its body".to_string());
        }
        if !variables.is_empty()
            && !variables.contains("{{")
            && check_json(variables.as_bytes()).is_err()
        {
            return Err(
                "the GraphQL variables after the blank line are not a JSON object".to_string(),
            );
        }
        Ok((query, variables))
    }
}

/// Separates a GraphQL body into the query and the variables: the
/// variables are the JSON object after the last blank line, when what
/// follows that line starts with `{`. The variables are `""` when there
/// are none. Both halves keep their placeholders.
pub fn split_graphql(body: &str) -> (String, String) {
    let lines: Vec<&str> = body.split('\n').collect();
    for i in (1..lines.len()).rev() {
        if !lines[i].trim().is_empty() {
            continue;
        }
        let rest = lines[i + 1..].join("\n");
        let rest = rest.trim();
        if rest.starts_with('{') {
            return (lines[..i].join("\n").trim().to_string(), rest.to_string());
        }
        break;
    }
    (body.trim().to_string(), String::new())
}

/// The body sent for a GraphQL request: the query as a JSON string and the
/// variables object, when there is one. The variables must be a JSON
/// object by then.
pub fn graphql_envelope(query: &str, variables: &str) -> Result<String, String> {
    let mut out = String::from("{\"query\":");
    out.push_str(&json_string(query, true));
    let v = variables.trim();
    if !v.is_empty() {
        if check_json(v.as_bytes()).is_err() || !v.starts_with('{') {
            return Err("the GraphQL variables are not a JSON object".to_string());
        }
        out.push_str(",\"variables\":");
        out.push_str(&compact_json(v, true));
    }
    out.push('}');
    Ok(out)
}
