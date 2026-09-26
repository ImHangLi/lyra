//! Static target names from Makefiles and justfiles. Nothing is expanded or evaluated:
//! `$(shell ...)`, variables, and `include`d files stay as declared text.

use std::collections::BTreeSet;

use crate::{Certainty, FactKind, Facts, Lines};

const P: Certainty = Certainty::Parsed;

/// Logical lines with backslash continuations joined, each with its first and last line.
fn logical_lines(text: &str) -> Vec<(String, usize, usize)> {
    let mut out = Vec::new();
    let mut buf = String::new();
    let mut start = 0;
    for (i, raw) in text.lines().enumerate() {
        let n = i + 1;
        if buf.is_empty() {
            start = n;
        }
        match raw.strip_suffix('\\') {
            Some(head) => {
                buf.push_str(head);
                buf.push(' ');
            }
            None => {
                buf.push_str(raw);
                out.push((std::mem::take(&mut buf), start, n));
            }
        }
    }
    if !buf.is_empty() {
        out.push((buf, start, text.lines().count()));
    }
    out
}

/// Byte index of the rule colon in `s`, skipping quoted text; `None` for `:=`/`::=`.
/// With `eq_ends`, an `=` before the colon marks a variable assignment (Make syntax).
fn rule_colon(s: &str, eq_ends: bool) -> Option<usize> {
    let mut quote = None;
    for (i, c) in s.char_indices() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(c),
            (None, '=') if eq_ends => return None,
            (None, ':') => {
                let rest = &s[i..];
                if rest.starts_with(":=") || rest.starts_with("::=") {
                    return None;
                }
                return Some(i);
            }
            _ => {}
        }
    }
    None
}

const MAKE_DIRECTIVES: &[&str] = &[
    "ifeq", "ifneq", "ifdef", "ifndef", "else", "endif", "export", "unexport", "override", "vpath",
    "undefine", "private",
];

pub fn makefile(rel: &str, text: &str, facts: &mut Facts) {
    let mut seen = BTreeSet::new();
    let mut in_define = false;
    for (line, start, end) in logical_lines(text) {
        let first = line.split_whitespace().next().unwrap_or_default();
        if in_define {
            if first == "endef" {
                in_define = false;
            }
            continue;
        }
        if line.starts_with('\t') || first.starts_with('#') || first.is_empty() {
            continue;
        }
        if first == "define" {
            in_define = true;
            continue;
        }
        if matches!(first, "include" | "-include" | "sinclude") {
            let declared = line.trim_start()[first.len()..].trim();
            if !declared.is_empty() {
                facts.add(
                    FactKind::MakeInclude,
                    declared,
                    rel,
                    Some(Lines { start, end }),
                    P,
                );
            }
            continue;
        }
        if MAKE_DIRECTIVES.contains(&first) {
            continue;
        }
        let (body, doc) = match line.split_once("##") {
            Some((b, d)) => (b, Some(d.trim())),
            None => (line.as_str(), None),
        };
        let body = body.split_once('#').map_or(body, |(b, _)| b);
        let Some(colon) = rule_colon(body, true) else {
            continue;
        };
        let head = &body[..colon];
        // Target names built from variables or functions are only known after expansion,
        // which the scanner never performs.
        if head.contains('$') {
            continue;
        }
        for target in head.split_whitespace() {
            if target.contains('%') || target.starts_with('.') || !seen.insert(target.to_owned()) {
                continue;
            }
            facts
                .add(
                    FactKind::MakeTarget,
                    target,
                    rel,
                    Some(Lines::one(start)),
                    P,
                )
                .with(None, doc.filter(|d| !d.is_empty()));
        }
    }
}

const JUST_KEYWORDS: &[&str] = &["set", "export", "alias", "import", "mod", "unexport"];

fn just_name(token: &str) -> Option<&str> {
    let name = token.trim_start_matches('@');
    let mut chars = name.chars();
    let first = chars.next()?;
    let ok = (first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    ok.then_some(name)
}

pub fn justfile(rel: &str, text: &str, facts: &mut Facts) {
    let mut comment: Option<String> = None;
    for (i, line) in text.lines().enumerate() {
        let n = i + 1;
        if line.starts_with(char::is_whitespace) || line.is_empty() {
            if line.trim().is_empty() {
                comment = None;
            }
            continue;
        }
        if let Some(c) = line.strip_prefix('#') {
            comment = Some(c.trim().to_owned());
            continue;
        }
        if line.starts_with('[') {
            continue;
        }
        let doc = comment.take();
        let first = line.split_whitespace().next().unwrap_or_default();
        if JUST_KEYWORDS.contains(&first) {
            continue;
        }
        let Some(colon) = rule_colon(line, false) else {
            continue;
        };
        let head = &line[..colon];
        let Some(name) = head.split_whitespace().next().and_then(just_name) else {
            continue;
        };
        facts
            .add(FactKind::JustRecipe, name, rel, Some(Lines::one(n)), P)
            .with(None, doc.as_deref().filter(|d| !d.is_empty()));
    }
}
