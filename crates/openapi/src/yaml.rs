//! Reads the first YAML document of a file into a tree of nodes that keeps
//! what the OpenAPI walker needs and a value-level parser throws away: the
//! order of a mapping's keys, each scalar as it was written, whether it
//! was quoted, its tag, the line it starts on, and aliases as nodes of
//! their own (an anchor may contain an alias to itself).
//!
//! The parsing is libyaml's, through `unsafe-libyaml`, which the YAML
//! reader of `apic.yaml` already links. Its API is C's, so the calls sit in
//! [`events`], the one function of this crate that is allowed `unsafe`;
//! everything else works on the events it returns.

use std::collections::HashMap;

use apic_project::{resolve_yaml_scalar, YamlScalar};

/// The index of a node in its [`Tree`].
pub(crate) type NodeId = usize;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Scalar,
    Sequence,
    Mapping,
    Alias,
}

/// One node of the document.
#[derive(Clone, Debug)]
pub(crate) struct Node {
    pub(crate) kind: Kind,
    /// A scalar's text, without its quotes; empty for any other kind.
    pub(crate) value: String,
    /// The short tag: `!!str`, `!!int`, `!!map`... A plain scalar has the
    /// tag it resolves to, a quoted one `!!str`, a tagged one its tag.
    pub(crate) tag: String,
    /// A scalar written plain: not quoted, not a block scalar, not tagged.
    pub(crate) plain: bool,
    /// The 1-based line the node starts on; 0 for a node that was not read
    /// from the file.
    pub(crate) line: usize,
    /// A sequence's items, or a mapping's keys and values alternating.
    pub(crate) content: Vec<NodeId>,
    /// The node an alias stands for.
    pub(crate) alias: Option<NodeId>,
}

impl Node {
    pub(crate) fn mapping(tag: &str, content: Vec<NodeId>) -> Node {
        Node {
            kind: Kind::Mapping,
            value: String::new(),
            tag: tag.to_string(),
            plain: false,
            line: 0,
            content,
            alias: None,
        }
    }
}

/// The first document of a file: `root` is `None` when the file holds no
/// document (it is empty, or comments only).
pub(crate) struct Tree {
    pub(crate) nodes: Vec<Node>,
    pub(crate) root: Option<NodeId>,
}

#[derive(Debug)]
enum Style {
    Plain,
    Other,
}

#[derive(Debug)]
enum Event {
    DocumentStart {
        /// A `%YAML` directive names a version other than 1.1, which has
        /// always been refused, 1.2 included.
        incompatible: bool,
    },
    DocumentEnd,
    StreamEnd,
    Alias {
        anchor: String,
    },
    Scalar {
        anchor: Option<String>,
        tag: Option<String>,
        value: String,
        style: Style,
    },
    SequenceStart {
        anchor: Option<String>,
        tag: Option<String>,
    },
    SequenceEnd,
    MappingStart {
        anchor: Option<String>,
        tag: Option<String>,
    },
    MappingEnd,
}

/// Parses `data` up to the end of its first document and returns the
/// events with the 0-based line each starts on. The error is the message to
/// show after `yaml: `.
#[allow(unsafe_code)]
fn events(data: &[u8]) -> Result<Vec<(Event, usize)>, String> {
    use std::ffi::CStr;
    use std::mem::MaybeUninit;
    use unsafe_libyaml as unsafe_yaml;

    /// Frees the parser's buffers however the function is left.
    struct Parser(Box<MaybeUninit<unsafe_yaml::yaml_parser_t>>);

    impl Drop for Parser {
        fn drop(&mut self) {
            // SAFETY: the parser was initialised before this guard was
            // built and is deleted nowhere else.
            unsafe { unsafe_yaml::yaml_parser_delete(self.0.as_mut_ptr()) }
        }
    }

    /// A NUL-terminated string the parser owns, copied out.
    ///
    /// SAFETY: `ptr` must be null or point to a NUL-terminated string.
    unsafe fn text(ptr: *const u8) -> Option<String> {
        if ptr.is_null() {
            return None;
        }
        let text = unsafe { CStr::from_ptr(ptr.cast()) };
        Some(text.to_string_lossy().into_owned())
    }

    // The parser is boxed: it holds pointers into itself and must not move.
    let mut parser = Box::new(MaybeUninit::<unsafe_yaml::yaml_parser_t>::uninit());
    // SAFETY: `parser` points to writable memory of the right size, which
    // `yaml_parser_initialize` fills in.
    if unsafe { unsafe_yaml::yaml_parser_initialize(parser.as_mut_ptr()) }.fail {
        return Err("out of memory".to_string());
    }
    let mut parser = Parser(parser);
    let raw = parser.0.as_mut_ptr();
    // SAFETY: `data` outlives the parser, which is dropped in this call.
    unsafe { unsafe_yaml::yaml_parser_set_input_string(raw, data.as_ptr(), data.len() as u64) };

    let mut out = Vec::new();
    loop {
        let mut event = MaybeUninit::<unsafe_yaml::yaml_event_t>::uninit();
        // SAFETY: both pointers are valid; on success the event is filled
        // in, on failure the parser's error fields are.
        if unsafe { unsafe_yaml::yaml_parser_parse(raw, event.as_mut_ptr()) }.fail {
            // SAFETY: the parser is initialised, and `problem` is null or a
            // static NUL-terminated string.
            let (kind, problem, problem_line, context_line) = unsafe {
                let failed = &*raw;
                (
                    failed.error,
                    text(failed.problem.cast()),
                    failed.problem_mark.line as usize,
                    failed.context_mark.line as usize,
                )
            };
            // The line such messages have always carried: where the
            // construct being read began when the parser knows it, else
            // where the problem is, counted from zero for a problem of
            // structure and from one for a problem of characters; and no
            // line at all on the first.
            let mut line = if context_line != 0 {
                context_line
            } else {
                problem_line
            };
            if line != 0 && kind == unsafe_yaml::YAML_SCANNER_ERROR {
                line += 1;
            }
            let problem = match problem {
                // The spelling this message has always had.
                Some(p) if !p.is_empty() => p.replace("hexadecimal number", "hexdecimal number"),
                _ => "unknown problem parsing YAML content".to_string(),
            };
            return Err(if line != 0 {
                format!("line {line}: {problem}")
            } else {
                problem
            });
        }
        // SAFETY: the parse succeeded, so the event is initialised, and the
        // union field read is the one its type says is set. The strings are
        // null or NUL-terminated, and a scalar's value is `length` bytes.
        let (converted, line) = unsafe {
            let event = event.assume_init_mut();
            let line = event.start_mark.line as usize;
            let converted = match event.type_ {
                unsafe_yaml::YAML_STREAM_START_EVENT => None,
                unsafe_yaml::YAML_STREAM_END_EVENT => Some(Event::StreamEnd),
                unsafe_yaml::YAML_DOCUMENT_START_EVENT => {
                    let version = event.data.document_start.version_directive;
                    Some(Event::DocumentStart {
                        incompatible: !version.is_null()
                            && ((*version).major != 1 || (*version).minor != 1),
                    })
                }
                unsafe_yaml::YAML_DOCUMENT_END_EVENT => Some(Event::DocumentEnd),
                unsafe_yaml::YAML_ALIAS_EVENT => Some(Event::Alias {
                    anchor: text(event.data.alias.anchor).unwrap_or_default(),
                }),
                unsafe_yaml::YAML_SCALAR_EVENT => {
                    let scalar = &event.data.scalar;
                    let value = std::slice::from_raw_parts(scalar.value, scalar.length as usize);
                    Some(Event::Scalar {
                        anchor: text(scalar.anchor),
                        tag: text(scalar.tag),
                        value: String::from_utf8_lossy(value).into_owned(),
                        style: if scalar.style == unsafe_yaml::YAML_PLAIN_SCALAR_STYLE {
                            Style::Plain
                        } else {
                            Style::Other
                        },
                    })
                }
                unsafe_yaml::YAML_SEQUENCE_START_EVENT => Some(Event::SequenceStart {
                    anchor: text(event.data.sequence_start.anchor),
                    tag: text(event.data.sequence_start.tag),
                }),
                unsafe_yaml::YAML_SEQUENCE_END_EVENT => Some(Event::SequenceEnd),
                unsafe_yaml::YAML_MAPPING_START_EVENT => Some(Event::MappingStart {
                    anchor: text(event.data.mapping_start.anchor),
                    tag: text(event.data.mapping_start.tag),
                }),
                unsafe_yaml::YAML_MAPPING_END_EVENT => Some(Event::MappingEnd),
                _ => None,
            };
            unsafe_yaml::yaml_event_delete(event);
            (converted, line)
        };
        let Some(converted) = converted else { continue };
        let last = matches!(converted, Event::StreamEnd | Event::DocumentEnd);
        out.push((converted, line));
        if last {
            return Ok(out);
        }
    }
}

/// `!!str` for `tag:yaml.org,2002:str`; any other tag as it is.
fn short_tag(tag: &str) -> String {
    match tag.strip_prefix("tag:yaml.org,2002:") {
        Some(rest) => format!("!!{rest}"),
        None => tag.to_string(),
    }
}

struct Builder {
    events: std::vec::IntoIter<(Event, usize)>,
    nodes: Vec<Node>,
    anchors: HashMap<String, NodeId>,
}

impl Builder {
    /// Adds a node, under its anchor when it has one, and returns its id.
    /// An explicit tag other than the non-specific `!` wins over the
    /// default.
    fn push(
        &mut self,
        kind: Kind,
        default_tag: &str,
        tag: Option<String>,
        value: String,
        line: usize,
        anchor: Option<String>,
    ) -> NodeId {
        let (tag, tagged) = match tag.as_deref() {
            Some(t) if !t.is_empty() && t != "!" => (short_tag(t), true),
            _ => (default_tag.to_string(), false),
        };
        let id = self.nodes.len();
        self.nodes.push(Node {
            kind,
            value,
            tag,
            plain: !tagged,
            line: line + 1,
            content: Vec::new(),
            alias: None,
        });
        if let Some(anchor) = anchor {
            self.anchors.insert(anchor, id);
        }
        id
    }

    /// Builds the node the next event starts; `Ok(None)` when the event
    /// closes the collection being read.
    fn node(&mut self) -> Result<Option<NodeId>, String> {
        let Some((event, line)) = self.events.next() else {
            return Ok(None);
        };
        match event {
            Event::Alias { anchor } => {
                let Some(&target) = self.anchors.get(&anchor) else {
                    return Err(format!("unknown anchor '{anchor}' referenced"));
                };
                let id = self.push(Kind::Alias, "", None, String::new(), line, None);
                self.nodes[id].alias = Some(target);
                self.nodes[id].plain = false;
                Ok(Some(id))
            }
            Event::Scalar {
                anchor,
                tag,
                value,
                style,
            } => {
                let plain = matches!(style, Style::Plain);
                let default_tag = if !plain {
                    "!!str"
                } else if value == "<<" {
                    "!!merge"
                } else {
                    resolve_yaml_scalar(&value).tag()
                };
                let id = self.push(Kind::Scalar, default_tag, tag, value, line, anchor);
                self.nodes[id].plain &= plain;
                Ok(Some(id))
            }
            Event::SequenceStart { anchor, tag } => {
                let id = self.push(Kind::Sequence, "!!seq", tag, String::new(), line, anchor);
                self.nodes[id].plain = false;
                while let Some(child) = self.node()? {
                    self.nodes[id].content.push(child);
                }
                Ok(Some(id))
            }
            Event::MappingStart { anchor, tag } => {
                let id = self.push(Kind::Mapping, "!!map", tag, String::new(), line, anchor);
                self.nodes[id].plain = false;
                while let Some(child) = self.node()? {
                    self.nodes[id].content.push(child);
                }
                Ok(Some(id))
            }
            Event::DocumentStart { .. }
            | Event::DocumentEnd
            | Event::StreamEnd
            | Event::SequenceEnd
            | Event::MappingEnd => Ok(None),
        }
    }
}

/// Reads the first document of `data`. The error is the message to show,
/// `yaml: ` included.
pub(crate) fn parse(data: &[u8]) -> Result<Tree, String> {
    let events = events(data).map_err(|e| format!("yaml: {e}"))?;
    let mut builder = Builder {
        events: events.into_iter(),
        nodes: Vec::new(),
        anchors: HashMap::new(),
    };
    let root = match builder.events.next() {
        Some((Event::DocumentStart { incompatible }, line)) => {
            if incompatible {
                let problem = "found incompatible YAML document";
                return Err(if line == 0 {
                    format!("yaml: {problem}")
                } else {
                    format!("yaml: line {line}: {problem}")
                });
            }
            builder.node().map_err(|e| format!("yaml: {e}"))?
        }
        _ => None,
    };
    Ok(Tree {
        nodes: builder.nodes,
        root,
    })
}

/// What a plain scalar resolves to; see [`resolve_yaml_scalar`].
pub(crate) fn resolve(value: &str) -> YamlScalar {
    resolve_yaml_scalar(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_order_text_tags_lines_and_aliases() {
        let text =
            "b: 1\na: \"2\"\nc: &x [0x1F, 2025-01-01, ~, !!str 3]\nd: *x\ne: &loop {self: *loop}\n";
        let tree = parse(text.as_bytes()).unwrap();
        let root = &tree.nodes[tree.root.unwrap()];
        assert_eq!(root.kind, Kind::Mapping);
        assert_eq!(root.line, 1);
        let keys: Vec<&str> = root
            .content
            .iter()
            .step_by(2)
            .map(|&k| tree.nodes[k].value.as_str())
            .collect();
        assert_eq!(keys, ["b", "a", "c", "d", "e"]);
        let value = |i: usize| &tree.nodes[root.content[2 * i + 1]];
        assert_eq!((value(0).tag.as_str(), value(0).plain), ("!!int", true));
        assert_eq!((value(1).tag.as_str(), value(1).plain), ("!!str", false));
        assert_eq!(value(1).line, 2);
        let tags: Vec<(&str, bool)> = value(2)
            .content
            .iter()
            .map(|&i| (tree.nodes[i].tag.as_str(), tree.nodes[i].plain))
            .collect();
        assert_eq!(
            tags,
            [
                ("!!int", true),
                ("!!timestamp", true),
                ("!!null", true),
                ("!!str", false)
            ]
        );
        assert_eq!(value(3).kind, Kind::Alias);
        assert_eq!(value(3).alias, Some(root.content[5]));
        // A recursive anchor is an alias to an enclosing node.
        let inner = &tree.nodes[value(4).content[1]];
        assert_eq!(inner.alias, Some(root.content[9]));
    }

    #[test]
    fn words_errors_and_empty_files() {
        assert!(parse(b"").unwrap().root.is_none());
        assert!(parse(b"# nothing\n").unwrap().root.is_none());
        assert_eq!(
            parse(b"a: *nope\n").err().unwrap(),
            "yaml: unknown anchor 'nope' referenced"
        );
        assert_eq!(
            parse(b"a: 1\n  b: 2\n").err().unwrap(),
            "yaml: line 2: mapping values are not allowed in this context"
        );
        assert_eq!(
            parse(b"a:\n  - b\n c: d\n").err().unwrap(),
            "yaml: line 2: did not find expected key"
        );
        assert_eq!(
            parse(b"%YAML 1.2\n---\na: 1\n").err().unwrap(),
            "yaml: found incompatible YAML document"
        );
        assert_eq!(
            parse(b"# c\n%YAML 1.2\n---\na: 1\n").err().unwrap(),
            "yaml: line 1: found incompatible YAML document"
        );
        assert!(parse(b"%YAML 1.1\n---\na: 1\n").is_ok());
        // Only the first document is read.
        let tree = parse(b"a: 1\n---\n[\n").unwrap();
        assert_eq!(tree.nodes[tree.root.unwrap()].kind, Kind::Mapping);
    }
}
