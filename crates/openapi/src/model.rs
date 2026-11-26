//! A minimal, order-preserving view of an OpenAPI 3 file. JSON documents
//! parse the same way since JSON is YAML.

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::LazyLock;

use apic_gocompat::quote;
use apic_project::YamlScalar;
use regex::Regex;

use crate::value::{Object, Value};
use crate::yaml::{self, Kind, Node, NodeId};

/// A node of the document, or nothing: a key that is absent, a reference
/// that could not be followed.
pub(crate) type Ref = Option<NodeId>;

/// The document: its nodes, its version and the first structural problem
/// met while resolving references.
pub(crate) struct Document {
    /// Nodes are only ever added (an OpenAPI 3.1 reference with keys next
    /// to it resolves to a node made for the occasion), so an id stays
    /// good for the life of the document.
    nodes: RefCell<Vec<Rc<Node>>>,
    pub(crate) root: NodeId,
    /// OpenAPI 3.1: keys next to `$ref` override the target; 3.0 ignores
    /// them.
    v31: bool,
    /// The first cyclic `$ref` or alias, external or unresolvable `$ref`.
    err: RefCell<Option<String>>,
}

impl Document {
    pub(crate) fn parse(data: &[u8]) -> Result<Document, String> {
        let tree = yaml::parse(data)?;
        let nodes: Vec<Rc<Node>> = tree.nodes.into_iter().map(Rc::new).collect();
        let root = match tree.root {
            Some(root) if nodes[root].kind == Kind::Mapping => root,
            _ => return Err("document is not an object".to_string()),
        };
        let mut d = Document {
            nodes: RefCell::new(nodes),
            root,
            v31: false,
            err: RefCell::new(None),
        };
        let version = d.str(d.get(Some(root), "openapi"));
        d.v31 = version.starts_with("3.1");
        if !version.starts_with("3.") {
            if !d.str(d.get(Some(root), "swagger")).is_empty() {
                return Err("the document is Swagger 2.0, which is not supported; convert it to OpenAPI 3 first".to_string());
            }
            return Err("not an OpenAPI 3 document (missing `openapi: 3.x`)".to_string());
        }
        Ok(d)
    }

    pub(crate) fn node(&self, id: NodeId) -> Rc<Node> {
        Rc::clone(&self.nodes.borrow()[id])
    }

    fn add(&self, node: Node) -> NodeId {
        let mut nodes = self.nodes.borrow_mut();
        nodes.push(Rc::new(node));
        nodes.len() - 1
    }

    /// The first structural problem met so far.
    pub(crate) fn err(&self) -> Option<String> {
        self.err.borrow().clone()
    }

    fn fail(&self, err: String) {
        let mut slot = self.err.borrow_mut();
        if slot.is_none() {
            *slot = Some(err);
        }
    }

    /// A mapping's key/value pairs in document order, with `$ref` and
    /// aliases resolved on the mapping itself.
    pub(crate) fn entries(&self, n: Ref) -> Vec<(String, NodeId)> {
        let Some(n) = self.resolve(n) else {
            return Vec::new();
        };
        let node = self.node(n);
        if node.kind != Kind::Mapping {
            return Vec::new();
        }
        node.content
            .chunks_exact(2)
            .map(|pair| (self.node(pair[0]).value.clone(), pair[1]))
            .collect()
    }

    /// The value for a key in a mapping, resolved, or nothing. Use it for
    /// OpenAPI objects; free-form data (examples, defaults) goes through
    /// `get_raw`.
    pub(crate) fn get(&self, n: Ref, key: &str) -> Ref {
        self.get_raw(n, key).and_then(|v| self.resolve(Some(v)))
    }

    /// The value for a key without `$ref` resolution, so an example
    /// payload that happens to contain a `$ref` key is kept as data.
    pub(crate) fn get_raw(&self, n: Ref, key: &str) -> Ref {
        self.entries(n)
            .into_iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v)
    }

    /// A sequence's elements without `$ref` resolution.
    pub(crate) fn items_raw(&self, n: Ref) -> Vec<NodeId> {
        let Some(n) = self.resolve(n) else {
            return Vec::new();
        };
        let node = self.node(n);
        if node.kind != Kind::Sequence {
            return Vec::new();
        }
        node.content.clone()
    }

    /// A sequence's elements, resolved.
    pub(crate) fn items(&self, n: Ref) -> Vec<Ref> {
        self.items_raw(n)
            .into_iter()
            .map(|c| self.resolve(Some(c)))
            .collect()
    }

    /// Follows YAML aliases and local `$ref` pointers such as
    /// `#/components/schemas/Pet`. A cycle is recorded as the document's
    /// error and yields nothing. External references are unsupported and
    /// recorded too.
    pub(crate) fn resolve(&self, n: Ref) -> Ref {
        self.resolve_from(n, &mut HashSet::new())
    }

    /// `resolve` with the cycle-detection state threaded through, so a 3.1
    /// sibling merge cannot restart the walk on its own target.
    fn resolve_from(&self, n: Ref, visited: &mut HashSet<NodeId>) -> Ref {
        let mut cur = n?;
        loop {
            let node = self.node(cur);
            if !visited.insert(cur) {
                self.fail(format!("cyclic $ref or alias at line {}", node.line));
                return None;
            }
            if node.kind == Kind::Alias {
                cur = node.alias?;
                continue;
            }
            if node.kind != Kind::Mapping {
                return Some(cur);
            }
            let mut reference = String::new();
            let mut siblings: Vec<NodeId> = Vec::new();
            for pair in node.content.chunks_exact(2) {
                // A Reference Object's $ref is a string; a data map (say a
                // schema's `properties`) may legitimately hold a key named
                // $ref.
                let (key, value) = (self.node(pair[0]), self.node(pair[1]));
                if key.value == "$ref" && value.kind == Kind::Scalar && value.tag == "!!str" {
                    reference.clone_from(&value.value);
                    continue;
                }
                siblings.extend_from_slice(pair);
            }
            if reference.is_empty() {
                return Some(cur);
            }
            if reference != "#" && !reference.starts_with("#/") {
                self.fail(format!(
                    "unsupported external $ref {} at line {}: only local #/ references are resolved",
                    quote(&reference),
                    node.line
                ));
                return Some(cur);
            }
            // A bare `#` is the whole document (RFC 6901's empty pointer).
            let target = if reference == "#" {
                Some(self.root)
            } else {
                self.pointer(&reference[2..])
            };
            let Some(target) = target else {
                self.fail(format!("unresolvable {reference} at line {}", node.line));
                return Some(cur);
            };
            if !siblings.is_empty() && self.v31 {
                // OpenAPI 3.1 allows keys next to $ref; they override the
                // target's. In 3.0 a Reference Object's siblings are ignored.
                let target = self.resolve_from(Some(target), visited)?; // a cycle is already recorded
                let target_node = self.node(target);
                if target_node.kind == Kind::Mapping {
                    let overridden: HashSet<String> = siblings
                        .chunks_exact(2)
                        .map(|pair| self.node(pair[0]).value.clone())
                        .collect();
                    let mut content = Vec::new();
                    for pair in target_node.content.chunks_exact(2) {
                        if !overridden.contains(&self.node(pair[0]).value) {
                            content.extend_from_slice(pair);
                        }
                    }
                    content.extend(siblings);
                    return Some(self.add(Node::mapping(&target_node.tag, content)));
                }
                cur = target;
                continue;
            }
            cur = target;
        }
    }

    /// Walks a JSON-pointer path (already stripped of `#/`) from the root.
    fn pointer(&self, path: &str) -> Ref {
        let mut cur = self.root;
        for seg in path.split('/') {
            // Fragments are URI-encoded before JSON Pointer escaping applies.
            let seg = path_unescape(seg).unwrap_or_else(|| seg.to_string());
            let seg = seg.replace("~1", "/").replace("~0", "~");
            let mut node = self.node(cur);
            if node.kind == Kind::Alias {
                node = self.node(node.alias?);
            }
            let next = match node.kind {
                Kind::Mapping => node
                    .content
                    .chunks_exact(2)
                    .find(|pair| self.node(pair[0]).value == seg)
                    .map(|pair| pair[1]),
                Kind::Sequence => atoi(&seg).and_then(|i| node.content.get(i).copied()),
                _ => None,
            };
            cur = next?;
        }
        Some(cur)
    }

    /// A scalar's text; empty for anything else.
    pub(crate) fn str(&self, n: Ref) -> String {
        match n {
            Some(n) => {
                let node = self.node(n);
                if node.kind == Kind::Scalar {
                    node.value.clone()
                } else {
                    String::new()
                }
            }
            None => String::new(),
        }
    }

    pub(crate) fn boolean(&self, n: Ref) -> bool {
        self.str(n).eq_ignore_ascii_case("true")
    }

    pub(crate) fn is_sequence(&self, n: Ref) -> bool {
        n.is_some_and(|n| self.node(n).kind == Kind::Sequence)
    }

    /// Converts a node into a plain value, keeping object key order.
    /// Aliases are followed, but a node already on the current path (a
    /// recursive anchor) decodes to null instead of recursing forever.
    pub(crate) fn decode(&self, n: Ref) -> Value {
        self.decode_guarded(n, &mut HashSet::new())
    }

    fn decode_guarded(&self, n: Ref, path: &mut HashSet<NodeId>) -> Value {
        let Some(id) = n else { return Value::Null };
        if !path.insert(id) {
            return Value::Null;
        }
        let node = self.node(id);
        let value = match node.kind {
            Kind::Alias => self.decode_guarded(node.alias, path),
            Kind::Mapping => {
                let mut obj = Object::default();
                for pair in node.content.chunks_exact(2) {
                    let value = self.decode_guarded(Some(pair[1]), path);
                    obj.set(&self.node(pair[0]).value, value);
                }
                Value::Object(obj)
            }
            Kind::Sequence => Value::Array(
                node.content
                    .iter()
                    .map(|&c| self.decode_guarded(Some(c), path))
                    .collect(),
            ),
            Kind::Scalar => decode_scalar(&node),
        };
        path.remove(&id);
        value
    }
}

/// The JSON number grammar (RFC 8259 §6).
static JSON_NUMBER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^-?(0|[1-9][0-9]*)(\.[0-9]+)?([eE][+-]?[0-9]+)?$").expect("a valid pattern")
});

fn decode_scalar(node: &Node) -> Value {
    let text = || Value::Str(node.value.clone());
    if node.tag == "!!timestamp" {
        return text(); // keep `2025-01-01` as written, not as a point in time
    }
    if node.plain && JSON_NUMBER.is_match(&node.value) {
        // A plain scalar spelt as a JSON number keeps its exact text:
        // decoding would round 18446744073709551616 to a float and turn
        // 1e400 into a string. Quoted or tagged text is not a number.
        return Value::Number(node.value.clone());
    }
    if node.tag == "!!binary" {
        return match base64_decode(&node.value) {
            Some(data) => Value::Str(String::from_utf8_lossy(&data).into_owned()),
            None => text(),
        };
    }
    if !matches!(node.tag.as_str(), "!!bool" | "!!int" | "!!float" | "!!null") {
        return text();
    }
    // A scalar under a tag it does not fit keeps its text.
    match (yaml::resolve(&node.value), node.tag.as_str()) {
        (YamlScalar::Null, "!!null") => Value::Null,
        (YamlScalar::Bool(b), "!!bool") => Value::Bool(b),
        (YamlScalar::Int(n), "!!int") => Value::Int(n),
        (YamlScalar::Uint(n), "!!int") => Value::Uint(n),
        (YamlScalar::Int(n), "!!float") => Value::Float(n as f64),
        (YamlScalar::Uint(n), "!!float") => Value::Float(n as f64),
        // JSON has no .inf or .nan; keep the spelling as text.
        (YamlScalar::Float(f), "!!float") if f.is_finite() => Value::Float(f),
        _ => text(),
    }
}

/// Decodes padded base64 in the standard alphabet, line breaks ignored;
/// `None` for anything else.
fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let symbols: Vec<u8> = s.bytes().filter(|b| !matches!(b, b'\r' | b'\n')).collect();
    if symbols.len() % 4 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(symbols.len() / 4 * 3);
    let groups = symbols.len() / 4;
    for (g, chunk) in symbols.chunks_exact(4).enumerate() {
        let pad = chunk.iter().rev().take_while(|&&b| b == b'=').count();
        if pad > 2 || (pad > 0 && g + 1 != groups) {
            return None;
        }
        let mut bits: u32 = 0;
        for &b in &chunk[..4 - pad] {
            let v = match b {
                b'A'..=b'Z' => b - b'A',
                b'a'..=b'z' => b - b'a' + 26,
                b'0'..=b'9' => b - b'0' + 52,
                b'+' => 62,
                b'/' => 63,
                _ => return None,
            };
            bits = bits << 6 | u32::from(v);
        }
        bits <<= 6 * pad as u32;
        let bytes = bits.to_be_bytes();
        out.extend_from_slice(&bytes[1..4 - pad]);
    }
    Some(out)
}

/// A whole number written in decimal digits, with an optional sign.
fn atoi(s: &str) -> Option<usize> {
    let digits = s.strip_prefix('+').unwrap_or(s);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// Decodes `%XX` escapes; `None` when one is malformed.
pub(crate) fn path_unescape(s: &str) -> Option<String> {
    if !s.contains('%') {
        return Some(s.to_string());
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = bytes.get(i + 1..i + 3)?;
            let hi = (hex[0] as char).to_digit(16)?;
            let lo = (hex[1] as char).to_digit(16)?;
            out.push((hi * 16 + lo) as u8);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    Some(String::from_utf8_lossy(&out).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_scalars_as_written() {
        let d = Document::parse(
            b"openapi: 3.0.0\nx: [1, 0x1F, 1_000, .5, .inf, \"12\", 2025-01-01, ~, true, !!float 3, !!int x, 0.10, <<, +7]\n",
        )
        .unwrap();
        let v = d.decode(d.get_raw(Some(d.root), "x"));
        assert_eq!(
            v.to_json(),
            "[1,31,1000,0.5,\".inf\",\"12\",\"2025-01-01\",null,true,3,\"x\",0.10,\"\\u003c\\u003c\",7]"
        );
    }

    #[test]
    fn unescapes_pointer_segments() {
        assert_eq!(path_unescape("User%20Profile").unwrap(), "User Profile");
        assert_eq!(path_unescape("a+b").unwrap(), "a+b");
        assert!(path_unescape("bad%zz").is_none());
        assert!(path_unescape("bad%2").is_none());
    }

    #[test]
    fn decodes_base64() {
        assert_eq!(base64_decode("aGk=").unwrap(), b"hi");
        assert_eq!(base64_decode("aGkh\nYQ==").unwrap(), b"hi!a");
        assert_eq!(base64_decode("").unwrap(), b"");
        assert!(base64_decode("aGk").is_none());
        assert!(base64_decode("a=k=").is_none());
        assert!(base64_decode("aG=k").is_none());
    }
}
