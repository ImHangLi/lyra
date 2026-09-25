//! A minimal YAML tree with 1-based line numbers, built from `yaml-rust2` parser events.
//! Aliases are kept as opaque nodes and never expanded.

use yaml_rust2::parser::{Event, Parser};

/// Deepest nesting accepted; deeper documents are rejected as unparseable.
const MAX_DEPTH: usize = 64;

pub enum Node {
    Scalar(String, usize),
    Map(Vec<Entry>, usize),
    Seq(Vec<Node>, usize),
    Alias(usize),
}

pub struct Entry {
    pub key: String,
    pub key_line: usize,
    pub value: Node,
}

impl Node {
    pub fn line(&self) -> usize {
        match self {
            Self::Scalar(_, l) | Self::Map(_, l) | Self::Seq(_, l) | Self::Alias(l) => *l,
        }
    }

    pub fn get(&self, key: &str) -> Option<&Entry> {
        match self {
            Self::Map(entries, _) => entries.iter().find(|e| e.key == key),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Scalar(s, _) => Some(s),
            _ => None,
        }
    }

    pub fn entries(&self) -> &[Entry] {
        match self {
            Self::Map(entries, _) => entries,
            _ => &[],
        }
    }

    pub fn items(&self) -> &[Node] {
        match self {
            Self::Seq(items, _) => items,
            _ => &[],
        }
    }
}

enum Frame {
    Map {
        entries: Vec<Entry>,
        line: usize,
        key: Option<(String, usize)>,
    },
    Seq {
        items: Vec<Node>,
        line: usize,
    },
}

/// Parses the first document of `text`. Returns `None` on a syntax error or excess depth.
pub fn parse(text: &str) -> Option<Node> {
    let mut parser = Parser::new_from_str(text);
    let mut stack: Vec<Frame> = Vec::new();
    loop {
        let (event, mark) = parser.next_token().ok()?;
        let line = mark.line();
        let node = match event {
            // A document without content (only comments) is an empty mapping.
            Event::StreamEnd | Event::DocumentEnd => {
                return stack.is_empty().then(|| Node::Map(Vec::new(), line));
            }
            Event::Scalar(value, ..) => Node::Scalar(value, line),
            Event::Alias(_) => Node::Alias(line),
            Event::MappingStart(..) => {
                if stack.len() >= MAX_DEPTH {
                    return None;
                }
                stack.push(Frame::Map {
                    entries: Vec::new(),
                    line,
                    key: None,
                });
                continue;
            }
            Event::SequenceStart(..) => {
                if stack.len() >= MAX_DEPTH {
                    return None;
                }
                stack.push(Frame::Seq {
                    items: Vec::new(),
                    line,
                });
                continue;
            }
            Event::MappingEnd | Event::SequenceEnd => match stack.pop()? {
                Frame::Map { entries, line, .. } => Node::Map(entries, line),
                Frame::Seq { items, line } => Node::Seq(items, line),
            },
            _ => continue,
        };
        match stack.last_mut() {
            None => return Some(node),
            Some(Frame::Seq { items, .. }) => items.push(node),
            Some(Frame::Map { entries, key, .. }) => match key.take() {
                Some((k, key_line)) => entries.push(Entry {
                    key: k,
                    key_line,
                    value: node,
                }),
                None => {
                    // Complex (non-scalar) keys are kept with an empty name and ignored later.
                    let line = node.line();
                    let k = match node {
                        Node::Scalar(s, _) => s,
                        _ => String::new(),
                    };
                    *key = Some((k, line));
                }
            },
        }
    }
}
