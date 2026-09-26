//! `package.json` facts. Scripts are reported as declared text and never run.

use std::path::Path;

use serde_json::Value;

use crate::text::LineIndex;
use crate::{Certainty, FactKind, Facts, Lines};

/// Position of an object key at nesting depth 1 (top level) or 2.
struct KeyPos {
    depth: usize,
    parent: Option<String>,
    key: String,
    offset: usize,
}

/// Finds object keys at depth 1 and 2 with their byte offsets, so facts can cite lines.
/// Runs only on text that `serde_json` already parsed.
fn key_positions(text: &str) -> Vec<KeyPos> {
    let b = text.as_bytes();
    // Per open container: (is_object, expecting_key, current_key).
    let mut stack: Vec<(bool, bool, Option<String>)> = Vec::new();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'{' => stack.push((true, true, None)),
            b'[' => stack.push((false, false, None)),
            b'}' | b']' => {
                stack.pop();
            }
            b',' => {
                if let Some(top) = stack.last_mut() {
                    top.1 = top.0;
                }
            }
            b':' => {
                if let Some(top) = stack.last_mut() {
                    top.1 = false;
                }
            }
            b'"' => {
                let start = i;
                let mut j = i + 1;
                while j < b.len() {
                    match b[j] {
                        b'\\' => j += 2,
                        b'"' => break,
                        _ => j += 1,
                    }
                }
                let end = (j + 1).min(b.len());
                let depth = stack.len();
                if let Some(top) = stack.last()
                    && top.0
                    && top.1
                    && let Some(raw) = text.get(start..end)
                    && let Ok(key) = serde_json::from_str::<String>(raw)
                {
                    if depth <= 2 {
                        let parent = if depth == 2 {
                            stack.first().and_then(|s| s.2.clone())
                        } else {
                            None
                        };
                        out.push(KeyPos {
                            depth,
                            parent,
                            key: key.clone(),
                            offset: start,
                        });
                    }
                    if let Some(top) = stack.last_mut() {
                        top.2 = Some(key);
                    }
                }
                i = end;
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    out
}

struct Locator {
    keys: Vec<KeyPos>,
    lines: LineIndex,
}

impl Locator {
    fn top(&self, key: &str) -> Option<Lines> {
        self.find(1, None, key)
    }
    fn child(&self, parent: &str, key: &str) -> Option<Lines> {
        self.find(2, Some(parent), key)
    }
    fn find(&self, depth: usize, parent: Option<&str>, key: &str) -> Option<Lines> {
        self.keys
            .iter()
            .find(|k| k.depth == depth && k.parent.as_deref() == parent && k.key == key)
            .map(|k| Lines::one(self.lines.line(k.offset)))
    }
}

/// Parses one `package.json`. Returns `false` when it is not valid JSON.
pub fn package_json(rel: &str, text: &str, facts: &mut Facts) -> bool {
    let Ok(Value::Object(root)) = serde_json::from_str::<Value>(text) else {
        return false;
    };
    let loc = Locator {
        keys: key_positions(text),
        lines: LineIndex::new(text),
    };
    let parsed = Certainty::Parsed;
    if let Some(Value::String(name)) = root.get("name") {
        facts.add(FactKind::PackageName, name, rel, loc.top("name"), parsed);
    }
    if let Some(Value::String(pm)) = root.get("packageManager") {
        facts.add(
            FactKind::PackageManager,
            pm,
            rel,
            loc.top("packageManager"),
            parsed,
        );
    }
    if let Some(Value::Object(scripts)) = root.get("scripts") {
        for (name, cmd) in scripts {
            if let Value::String(cmd) = cmd {
                facts
                    .add(
                        FactKind::PackageScript,
                        name,
                        rel,
                        loc.child("scripts", name),
                        parsed,
                    )
                    .with(None, Some(cmd));
            }
        }
    }
    let globs = match root.get("workspaces") {
        Some(Value::Array(a)) => Some(a),
        Some(Value::Object(o)) => match o.get("packages") {
            Some(Value::Array(a)) => Some(a),
            _ => None,
        },
        _ => None,
    };
    for g in globs.into_iter().flatten() {
        if let Value::String(g) = g {
            facts.add(
                FactKind::WorkspaceGlob,
                g,
                rel,
                loc.top("workspaces"),
                parsed,
            );
        }
    }
    if let Some(Value::Object(engines)) = root.get("engines") {
        for (engine, range) in engines {
            if let Value::String(range) = range {
                facts
                    .add(
                        FactKind::RuntimeRequirement,
                        range,
                        rel,
                        loc.child("engines", engine),
                        parsed,
                    )
                    .with(Some(engine), None);
            }
        }
    }
    true
}

/// The file in `dir` that declares a monorepo, if any.
pub fn monorepo_marker(dir: &Path, max_bytes: u64) -> Option<&'static str> {
    for name in [
        "pnpm-workspace.yaml",
        "lerna.json",
        "turbo.json",
        "nx.json",
        "rush.json",
    ] {
        if dir.join(name).is_file() {
            return Some(name);
        }
    }
    let pkg = dir.join("package.json");
    let meta = std::fs::metadata(&pkg).ok()?;
    if !meta.is_file() || meta.len() > max_bytes {
        return None;
    }
    let text = std::fs::read_to_string(&pkg).ok()?;
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(o)) if o.contains_key("workspaces") => Some("package.json"),
        _ => None,
    }
}
