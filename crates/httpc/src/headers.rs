//! The ordered header multi-map both the client and the server use.

use std::collections::BTreeMap;

/// Returns the canonical form of a header name: the first letter and any
/// letter after a hyphen in upper case, the rest in lower case, so
/// `content-type` and `CONTENT-TYPE` both become `Content-Type`. A name
/// that is not a valid header token (it holds a space, say) is returned
/// unchanged.
pub fn canonical_header_key(name: &str) -> String {
    if name.is_empty() || !name.bytes().all(is_token_byte) {
        return name.to_string();
    }
    let mut out = String::with_capacity(name.len());
    let mut upper = true;
    for b in name.bytes() {
        let c = if upper {
            b.to_ascii_uppercase()
        } else {
            b.to_ascii_lowercase()
        };
        out.push(c as char);
        upper = b == b'-';
    }
    out
}

/// Reports whether `b` may appear in a header name or a method (an RFC
/// 9110 token).
pub(crate) fn is_token_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b)
}

/// Headers in the order they were added, with names held in canonical
/// form (`Content-Type`) and looked up without regard to case. A name may
/// repeat: `get` returns the first value, `values` all of them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Headers {
    entries: Vec<(String, String)>,
}

impl Headers {
    /// An empty set of headers.
    pub fn new() -> Headers {
        Headers::default()
    }

    /// Adds a value, keeping any the name already has.
    pub fn add(&mut self, name: &str, value: &str) {
        self.entries
            .push((canonical_header_key(name), value.to_string()));
    }

    /// Sets the name to this one value, replacing any it had. The header
    /// keeps the position of its first value when it was already present.
    pub fn set(&mut self, name: &str, value: &str) {
        let key = canonical_header_key(name);
        match self.entries.iter().position(|(k, _)| *k == key) {
            Some(i) => {
                self.entries[i].1 = value.to_string();
                let mut n = 0;
                self.entries.retain(|(k, _)| {
                    if *k == key {
                        n += 1;
                        n == 1
                    } else {
                        true
                    }
                });
            }
            None => self.entries.push((key, value.to_string())),
        }
    }

    /// The first value of the name, if it has one.
    pub fn get(&self, name: &str) -> Option<&str> {
        let key = canonical_header_key(name);
        self.entries
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v.as_str())
    }

    /// Every value of the name, in order.
    pub fn values(&self, name: &str) -> Vec<&str> {
        let key = canonical_header_key(name);
        self.entries
            .iter()
            .filter(|(k, _)| *k == key)
            .map(|(_, v)| v.as_str())
            .collect()
    }

    /// Reports whether the name has a value.
    pub fn contains(&self, name: &str) -> bool {
        self.get(name).is_some()
    }

    /// Removes every value of the name.
    pub fn del(&mut self, name: &str) {
        let key = canonical_header_key(name);
        self.entries.retain(|(k, _)| *k != key);
    }

    /// The `(name, value)` pairs in order, names in canonical form.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.entries.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }

    /// The distinct names, in the order each first appeared.
    pub fn names(&self) -> Vec<&str> {
        let mut out: Vec<&str> = Vec::new();
        for (k, _) in &self.entries {
            if !out.contains(&k.as_str()) {
                out.push(k);
            }
        }
        out
    }

    /// The number of values held (a repeated name counts each time).
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Reports whether there are no headers.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The headers as a map from canonical name to its values, sorted by
    /// name, for callers that want a stable order.
    pub fn to_map(&self) -> BTreeMap<String, Vec<String>> {
        let mut m: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (k, v) in &self.entries {
            m.entry(k.clone()).or_default().push(v.clone());
        }
        m
    }
}

impl<N: AsRef<str>, V: AsRef<str>> FromIterator<(N, V)> for Headers {
    fn from_iter<T: IntoIterator<Item = (N, V)>>(iter: T) -> Headers {
        let mut h = Headers::new();
        for (k, v) in iter {
            h.add(k.as_ref(), v.as_ref());
        }
        h
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_key() {
        for (give, want) in [
            ("content-type", "Content-Type"),
            ("CONTENT-TYPE", "Content-Type"),
            ("etag", "Etag"),
            ("x-api-KEY", "X-Api-Key"),
            ("www-authenticate", "Www-Authenticate"),
            ("has space", "has space"),
            ("", ""),
        ] {
            assert_eq!(canonical_header_key(give), want);
        }
    }

    #[test]
    fn multi_map() {
        let mut h = Headers::new();
        h.add("set-cookie", "a=1");
        h.add("X-One", "1");
        h.add("Set-Cookie", "b=2");
        assert_eq!(h.get("SET-COOKIE"), Some("a=1"));
        assert_eq!(h.values("set-cookie"), ["a=1", "b=2"]);
        assert_eq!(h.names(), ["Set-Cookie", "X-One"]);
        h.set("set-cookie", "c=3");
        assert_eq!(h.values("Set-Cookie"), ["c=3"]);
        assert_eq!(h.iter().next(), Some(("Set-Cookie", "c=3")));
        h.del("x-one");
        assert_eq!(h.len(), 1);
        assert_eq!(h.to_map()["Set-Cookie"], ["c=3"]);
    }
}
