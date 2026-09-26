//! Env example files (variable names only) and runtime version pin files.

use crate::{Certainty, FactKind, Facts, Lines};

fn is_var_name(s: &str) -> bool {
    let mut chars = s.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
}

/// Reports each declared variable name. The text after `=` is never copied anywhere.
pub fn env_example(rel: &str, text: &str, facts: &mut Facts) {
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line).trim_start();
        let name = line.split_once('=').map_or(line, |(n, _)| n).trim();
        if is_var_name(name) {
            facts.add(
                FactKind::EnvExampleVar,
                name,
                rel,
                Some(Lines::one(i + 1)),
                Certainty::Parsed,
            );
        }
    }
}

/// `.nvmrc`, `.node-version`, `.python-version`: first non-comment line. `.tool-versions`:
/// one fact per `tool version` line.
pub fn runtime_version(rel: &str, text: &str, facts: &mut Facts) {
    let name = crate::text::file_name(rel);
    let owner = match name {
        ".nvmrc" | ".node-version" => Some("node"),
        ".python-version" => Some("python"),
        _ => None,
    };
    for (i, line) in text.lines().enumerate() {
        let line = line.split('#').next().unwrap_or_default().trim();
        if line.is_empty() {
            continue;
        }
        let lines = Some(Lines::one(i + 1));
        match owner {
            Some(owner) => {
                facts
                    .add(
                        FactKind::RuntimeVersion,
                        line,
                        rel,
                        lines,
                        Certainty::Parsed,
                    )
                    .with(Some(owner), None);
                return;
            }
            None => {
                let mut parts = line.split_whitespace();
                if let (Some(tool), Some(version)) = (parts.next(), parts.next()) {
                    facts
                        .add(
                            FactKind::RuntimeVersion,
                            version,
                            rel,
                            lines,
                            Certainty::Parsed,
                        )
                        .with(Some(tool), None);
                }
            }
        }
    }
}
