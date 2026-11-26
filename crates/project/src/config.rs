//! `apic.yaml`: the configuration structs, the table of their keys, and
//! the decoder that fills them from the file.
//!
//! The structs, the key table ([`CONFIG_FIELDS`]) and the decoder are
//! written by one macro from one list of fields, so a key cannot be in one
//! and missing from another.

use std::collections::{BTreeMap, HashSet};

use apic_gocompat::quote;

use crate::yaml::{Doc, Kind, Node, Resolved};

/// The optional per-project configuration file name.
pub const CONFIG_FILE: &str = "apic.yaml";

/// One key of `apic.yaml`: what it is called in the file, the field that
/// holds it, and what it takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConfigField {
    /// The key as written in the file: `maxBodyBytes`.
    pub key: &'static str,
    /// The field of the struct: `max_body_bytes`.
    pub name: &'static str,
    /// What the key takes.
    pub kind: FieldKind,
}

/// The type of a key of `apic.yaml`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldKind {
    String,
    Bool,
    /// A whole number.
    Int,
    /// A list of the inner kind.
    Slice(&'static FieldKind),
    /// The inner kind, or nothing: a key that knows whether it was set.
    Optional(&'static FieldKind),
    /// A mapping from any name to the inner kind.
    Map(&'static FieldKind),
    /// A section with keys of its own, in order.
    Struct(&'static [ConfigField]),
}

/// Every key of `apic.yaml` in the order [`Config`] declares them, with
/// the sections nested: what the schema generator walks.
pub const CONFIG_FIELDS: &[ConfigField] = Config::FIELDS;

/// Collects what goes wrong while a document is decoded. A value of the
/// wrong type is noted and the decoding goes on, so one run reports every
/// such value.
pub(crate) struct Ctx<'a> {
    doc: &'a Doc<'a>,
    errors: Vec<String>,
    /// A failure that ends the decoding: reported alone.
    fatal: Option<String>,
    /// While a `<<` merge is decoded, the keys already set: the mapping's
    /// own keys win over merged ones, and an earlier merge over a later.
    merged: Option<HashSet<String>>,
}

impl Ctx<'_> {
    /// Notes a value that does not fit the type it is decoded into.
    fn type_error(&mut self, node: &Node, into: &str) {
        let what = match &node.kind {
            Kind::Seq(_) => "!!seq".to_string(),
            Kind::Map(_) => "!!map".to_string(),
            Kind::Scalar { raw, .. } => {
                let tag = self.doc.resolved(node).tag();
                if raw.len() > 10 {
                    let head = String::from_utf8_lossy(&raw.as_bytes()[..7]);
                    format!("{tag} `{head}...`")
                } else {
                    format!("{tag} `{raw}`")
                }
            }
        };
        let line = self.doc.line(node);
        self.errors
            .push(format!("line {line}: cannot unmarshal {what} into {into}"));
    }

    fn is_null(&self, node: &Node) -> bool {
        matches!(node.kind, Kind::Scalar { .. }) && self.doc.resolved(node) == Resolved::Null
    }

    /// Reports the keys a mapping holds twice; false when there are any.
    fn unique_keys(&mut self, pairs: &[(Node, Node)]) -> bool {
        fn ident(node: &Node) -> (u8, &str) {
            match &node.kind {
                Kind::Scalar { raw, .. } => (0, raw),
                Kind::Seq(_) => (1, ""),
                Kind::Map(_) => (2, ""),
            }
        }
        let before = self.errors.len();
        for (i, (first, _)) in pairs.iter().enumerate() {
            for (second, _) in &pairs[i + 1..] {
                if ident(first) == ident(second) {
                    let message = format!(
                        "line {}: mapping key {} already defined at line {}",
                        self.doc.line(second),
                        quote(ident(second).1),
                        self.doc.line(first)
                    );
                    self.errors.push(message);
                }
            }
        }
        self.errors.len() == before
    }

    fn is_merge(&self, key: &Node) -> bool {
        matches!(&key.kind, Kind::Scalar { raw, .. } if raw == "<<")
            && self.doc.resolved(key) == Resolved::Merge
    }
}

/// A type a YAML node decodes into.
pub(crate) trait Decode: Default {
    const KIND: FieldKind;

    /// The type's name in messages.
    fn type_name() -> String;

    /// Decodes `node` into `self` and reports whether a value was set.
    /// A null leaves `self` as it is. `name` replaces [`Decode::type_name`]
    /// in messages when given.
    fn decode(&mut self, node: &Node, cx: &mut Ctx<'_>, name: Option<&str>) -> bool;
}

/// The checks every type but a mapping's makes first: a null sets nothing,
/// and a mapping with repeated keys is reported for those alone. Returns
/// whether the node is still to be decoded.
fn prepare(node: &Node, cx: &mut Ctx<'_>) -> bool {
    if cx.is_null(node) {
        return false;
    }
    match &node.kind {
        Kind::Map(pairs) => cx.unique_keys(pairs),
        _ => true,
    }
}

fn named<T: Decode>(name: Option<&str>) -> String {
    name.map_or_else(T::type_name, str::to_string)
}

impl Decode for String {
    const KIND: FieldKind = FieldKind::String;

    fn type_name() -> String {
        "string".to_string()
    }

    fn decode(&mut self, node: &Node, cx: &mut Ctx<'_>, name: Option<&str>) -> bool {
        if !prepare(node, cx) {
            return false;
        }
        match &node.kind {
            // Any scalar is its own text: `timeout: 30` is "30".
            Kind::Scalar { raw, .. } => {
                self.clone_from(raw);
                true
            }
            _ => {
                cx.type_error(node, &named::<Self>(name));
                false
            }
        }
    }
}

impl Decode for bool {
    const KIND: FieldKind = FieldKind::Bool;

    fn type_name() -> String {
        "bool".to_string()
    }

    fn decode(&mut self, node: &Node, cx: &mut Ctx<'_>, name: Option<&str>) -> bool {
        if !prepare(node, cx) {
            return false;
        }
        if let Kind::Scalar { raw, .. } = &node.kind {
            match (cx.doc.resolved(node), raw.as_str()) {
                (Resolved::Bool(b), _) => {
                    *self = b;
                    return true;
                }
                // The older spellings, for a key that takes a boolean.
                (Resolved::Str, "y" | "Y" | "yes" | "Yes" | "YES" | "on" | "On" | "ON") => {
                    *self = true;
                    return true;
                }
                (Resolved::Str, "n" | "N" | "no" | "No" | "NO" | "off" | "Off" | "OFF") => {
                    *self = false;
                    return true;
                }
                _ => {}
            }
        }
        cx.type_error(node, &named::<Self>(name));
        false
    }
}

impl Decode for i64 {
    const KIND: FieldKind = FieldKind::Int;

    fn type_name() -> String {
        "int64".to_string()
    }

    fn decode(&mut self, node: &Node, cx: &mut Ctx<'_>, name: Option<&str>) -> bool {
        if !prepare(node, cx) {
            return false;
        }
        if matches!(node.kind, Kind::Scalar { .. }) {
            // 2^63: the first number past the largest this holds.
            const LIMIT: f64 = 9_223_372_036_854_775_808.0;
            match cx.doc.resolved(node) {
                Resolved::Int(n) => {
                    *self = n;
                    return true;
                }
                // A number with a fraction is cut to its whole part.
                Resolved::Float(f) if f <= LIMIT => {
                    *self = if f == LIMIT { i64::MIN } else { f as i64 };
                    return true;
                }
                _ => {}
            }
        }
        cx.type_error(node, &named::<Self>(name));
        false
    }
}

impl<T: Decode> Decode for Vec<T> {
    const KIND: FieldKind = FieldKind::Slice(&T::KIND);

    fn type_name() -> String {
        format!("[]{}", T::type_name())
    }

    fn decode(&mut self, node: &Node, cx: &mut Ctx<'_>, name: Option<&str>) -> bool {
        if !prepare(node, cx) {
            return false;
        }
        let Kind::Seq(items) = &node.kind else {
            cx.type_error(node, &named::<Self>(name));
            return false;
        };
        self.clear();
        for item in items {
            // An item that sets nothing (a null, or a wrong type) is left out.
            let mut value = T::default();
            if value.decode(item, cx, None) {
                self.push(value);
            }
        }
        true
    }
}

impl<T: Decode> Decode for Option<T> {
    const KIND: FieldKind = FieldKind::Optional(&T::KIND);

    fn type_name() -> String {
        T::type_name()
    }

    fn decode(&mut self, node: &Node, cx: &mut Ctx<'_>, name: Option<&str>) -> bool {
        if cx.is_null(node) {
            return false;
        }
        self.get_or_insert_with(T::default).decode(node, cx, name)
    }
}

impl<T: Decode> Decode for BTreeMap<String, T> {
    const KIND: FieldKind = FieldKind::Map(&T::KIND);

    fn type_name() -> String {
        format!("map[string]{}", T::type_name())
    }

    fn decode(&mut self, node: &Node, cx: &mut Ctx<'_>, name: Option<&str>) -> bool {
        decode_mapping(
            self,
            node,
            cx,
            &named::<Self>(name),
            |map, cx, key, value| {
                let mut entry = T::default();
                // A null value still makes the entry, unless a merge would
                // put it over one that is there.
                if entry.decode(value, cx, None) || (cx.is_null(value) && !map.contains_key(key)) {
                    map.insert(key.to_string(), entry);
                }
            },
        )
    }
}

/// Decodes a mapping node into `target`, calling `set` for each key with
/// its value, and then whatever a `<<` key merges in.
fn decode_mapping<T>(
    target: &mut T,
    node: &Node,
    cx: &mut Ctx<'_>,
    type_name: &str,
    set: fn(&mut T, &mut Ctx<'_>, &str, &Node),
) -> bool {
    if !prepare(node, cx) {
        return false;
    }
    let Kind::Map(pairs) = &node.kind else {
        cx.type_error(node, type_name);
        return false;
    };
    // The keys already set matter to this mapping's own keys only: the
    // values decode with a clean slate.
    let mut merged = cx.merged.take();
    let mut merge: Option<&Node> = None;
    for (key, value) in pairs {
        if cx.is_merge(key) {
            merge = Some(value);
            continue;
        }
        let mut name = String::new();
        if !name.decode(key, cx, None) {
            continue;
        }
        if let Some(seen) = merged.as_mut() {
            if !seen.insert(name.clone()) {
                continue;
            }
        }
        set(target, cx, &name, value);
    }
    cx.merged = merged;
    let Some(merge) = merge else {
        return true;
    };
    let outer = cx.merged.clone();
    if cx.merged.is_none() {
        cx.merged = Some(
            pairs
                .iter()
                .filter_map(|(key, _)| match &key.kind {
                    Kind::Scalar { raw, .. } => Some(raw.clone()),
                    _ => None,
                })
                .collect(),
        );
    }
    let sources: Vec<&Node> = match &merge.kind {
        Kind::Map(_) => vec![merge],
        Kind::Seq(items) if items.iter().all(|i| matches!(i.kind, Kind::Map(_))) => {
            items.iter().collect()
        }
        _ => {
            cx.fatal.get_or_insert_with(|| {
                "map merge requires map or sequence of maps as the value".to_string()
            });
            Vec::new()
        }
    };
    for source in sources {
        decode_mapping(target, source, cx, type_name, set);
    }
    cx.merged = outer;
    true
}

/// Declares a section of `apic.yaml`: the struct, its key table and its
/// decoder. Each field is `name: Type = "key"`, with `as "type"` where
/// messages name the type differently.
macro_rules! config_struct {
    (
        $(#[$meta:meta])*
        pub struct $name:ident as $type_name:literal {
            $(
                $(#[$field_meta:meta])*
                pub $field:ident: $ty:ty = $key:literal $(as $field_type:literal)?,
            )*
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Default, PartialEq, Eq)]
        pub struct $name {
            $(
                $(#[$field_meta])*
                pub $field: $ty,
            )*
        }

        impl $name {
            /// The keys of this section, in order.
            pub const FIELDS: &'static [ConfigField] = &[
                $(ConfigField {
                    key: $key,
                    name: stringify!($field),
                    kind: <$ty as Decode>::KIND,
                },)*
            ];
        }

        impl Decode for $name {
            const KIND: FieldKind = FieldKind::Struct(Self::FIELDS);

            fn type_name() -> String {
                $type_name.to_string()
            }

            fn decode(&mut self, node: &Node, cx: &mut Ctx<'_>, name: Option<&str>) -> bool {
                decode_mapping(self, node, cx, &named::<Self>(name), |target, cx, key, value| {
                    match key {
                        $($key => {
                            let name: &[&str] = &[$($field_type)?];
                            target.$field.decode(value, cx, name.first().copied());
                        })*
                        // A key apic does not know is left alone.
                        _ => {}
                    }
                })
            }
        }
    };
}

config_struct! {
    /// The content of `apic.yaml`.
    pub struct Config as "project.Config" {
        /// The default environment.
        pub env: String = "env",
        /// The directory holding `.http` files, relative to the project
        /// root.
        pub dir: String = "dir",
        /// The default request timeout, e.g. `30s`.
        pub timeout: String = "timeout",
        /// The default retry policy, `<attempts> [interval]`, e.g. `10 2s`.
        pub retry: String = "retry",
        /// Keep a cookie jar per environment in `.apic/cookies.json`.
        pub cookies: bool = "cookies",
        /// Keeps the last N responses of each named request, per
        /// environment, under `.apic/history`; zero (the default) keeps
        /// none.
        pub history: i64 = "history" as "int",
        /// An http, https or socks5 proxy URL every request goes through;
        /// `no_proxy` lists the hosts that bypass it. `--proxy` beats it
        /// and `--no-proxy` switches it off for one command.
        pub proxy: String = "proxy",
        pub no_proxy: Vec<String> = "noProxy",
        /// Caps how much of a response apic will read into memory. Zero
        /// means the built-in default; see the runner's
        /// `DEFAULT_MAX_BODY_BYTES`.
        pub max_body_bytes: i64 = "maxBodyBytes",
        pub auth: AuthConfig = "auth",
        pub test: TestConfig = "test",
        pub tls: TlsConfig = "tls",
    }
}

config_struct! {
    /// The `tls:` section of `apic.yaml`. Paths are relative to the
    /// project root and confined to it.
    pub struct TlsConfig as "project.TLSConfig" {
        /// A PEM bundle added to the system roots.
        pub ca_file: String = "caFile",
        /// The client certificate (PEM).
        pub cert_file: String = "certFile",
        /// Its private key (PEM); defaults to `cert_file`.
        pub key_file: String = "keyFile",
        /// Verify the server certificate; default true.
        pub verify_host: Option<bool> = "verifyHost",
        /// Per-host overrides, by host name or `*.suffix`.
        pub hosts: BTreeMap<String, TlsHost> = "hosts",
    }
}

config_struct! {
    /// Overrides [`TlsConfig`] for one host.
    pub struct TlsHost as "project.TLSHost" {
        pub ca_file: String = "caFile",
        pub cert_file: String = "certFile",
        pub key_file: String = "keyFile",
        pub verify_host: Option<bool> = "verifyHost",
    }
}

config_struct! {
    /// The `test:` section of `apic.yaml`.
    pub struct TestConfig as "project.TestConfig" {
        /// Feature files or directories, relative to the root (default:
        /// `features`).
        pub paths: Vec<String> = "paths",
    }
}

config_struct! {
    /// The `auth:` section of `apic.yaml`.
    pub struct AuthConfig as "project.AuthConfig" {
        /// The auth spec applied to requests without `# @auth`, e.g.
        /// `aws region=eu-west-2`.
        pub default: String = "default",
        /// Permit `# @auth exec ...`.
        pub allow_exec: bool = "allowExec",
    }
}

impl Config {
    /// Reads the content of an `apic.yaml`: the first YAML document of
    /// `data`. A key apic does not know is ignored, a null leaves its key
    /// unset, and a key written twice or holding a value of the wrong type
    /// is an error. The error is the message to show:
    ///
    /// ```text
    /// yaml: unmarshal errors:
    ///   line 2: cannot unmarshal !!str `soon` into int
    /// ```
    pub fn from_yaml(data: &[u8]) -> Result<Config, String> {
        let mut config = Config::default();
        let Some(doc) = Doc::parse(data).map_err(|err| format!("yaml: {err}"))? else {
            return Ok(config);
        };
        let mut cx = Ctx {
            doc: &doc,
            errors: Vec::new(),
            fatal: None,
            merged: None,
        };
        config.decode(&doc.root, &mut cx, None);
        if let Some(fatal) = cx.fatal {
            return Err(format!("yaml: {fatal}"));
        }
        if !cx.errors.is_empty() {
            return Err(format!(
                "yaml: unmarshal errors:\n  {}",
                cx.errors.join("\n  ")
            ));
        }
        Ok(config)
    }
}
