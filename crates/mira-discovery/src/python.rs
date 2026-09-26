//! `pyproject.toml` facts, read with a span-preserving TOML parser.

use toml::Spanned;
use toml::de::{DeTable, DeValue};

use crate::text::LineIndex;
use crate::{Certainty, FactKind, Facts, Lines};

const P: Certainty = Certainty::Parsed;

type Item<'a, 'i> = (
    &'a Spanned<std::borrow::Cow<'i, str>>,
    &'a Spanned<DeValue<'i>>,
);

fn get<'a, 'i>(table: &'a DeTable<'i>, key: &str) -> Option<Item<'a, 'i>> {
    table.iter().find(|(k, _)| k.get_ref().as_ref() == key)
}

fn table<'a, 'i>(table: &'a DeTable<'i>, key: &str) -> Option<&'a DeTable<'i>> {
    get(table, key).and_then(|(_, v)| v.get_ref().as_table())
}

fn string<'a>(table: &'a DeTable<'_>, key: &str) -> Option<(&'a str, std::ops::Range<usize>)> {
    get(table, key).and_then(|(k, v)| {
        let s = v.get_ref().as_str()?;
        Some((s, k.span().start..v.span().end))
    })
}

struct Ctx<'f> {
    rel: &'f str,
    lines: LineIndex,
}

impl Ctx<'_> {
    fn at(&self, span: std::ops::Range<usize>) -> Option<Lines> {
        (span.end > span.start).then(|| self.lines.lines(span.start, span.end))
    }
}

fn scripts(ctx: &Ctx<'_>, t: &DeTable<'_>, facts: &mut Facts) {
    for (k, v) in t.iter() {
        let target = v.get_ref().as_str();
        facts
            .add(
                FactKind::PythonScript,
                k.get_ref().as_ref(),
                ctx.rel,
                ctx.at(k.span().start..v.span().end),
                P,
            )
            .with(None, target);
    }
}

pub fn pyproject(rel: &str, text: &str, facts: &mut Facts) -> bool {
    let Ok(doc) = DeTable::parse(text) else {
        return false;
    };
    let doc = doc.get_ref();
    let ctx = Ctx {
        rel,
        lines: LineIndex::new(text),
    };
    if let Some(project) = table(doc, "project") {
        if let Some((name, span)) = string(project, "name") {
            facts.add(FactKind::PackageName, name, rel, ctx.at(span), P);
        }
        if let Some((req, span)) = string(project, "requires-python") {
            facts
                .add(FactKind::RuntimeRequirement, req, rel, ctx.at(span), P)
                .with(Some("python"), None);
        }
        if let Some(s) = table(project, "scripts") {
            scripts(&ctx, s, facts);
        }
    }
    if let Some(build) = table(doc, "build-system")
        && let Some((backend, span)) = string(build, "build-backend")
    {
        facts.add(FactKind::BuildBackend, backend, rel, ctx.at(span), P);
    }
    if let Some(tool) = table(doc, "tool") {
        for (k, _) in tool.iter() {
            facts.add(
                FactKind::PythonTool,
                k.get_ref().as_ref(),
                rel,
                ctx.at(k.span()),
                P,
            );
        }
        if let Some(poetry) = table(tool, "poetry") {
            if let Some((name, span)) = string(poetry, "name") {
                facts.add(FactKind::PackageName, name, rel, ctx.at(span), P);
            }
            if let Some(s) = table(poetry, "scripts") {
                scripts(&ctx, s, facts);
            }
        }
        if let Some(members) = table(tool, "uv")
            .and_then(|uv| table(uv, "workspace"))
            .and_then(|ws| get(ws, "members"))
            .and_then(|(_, v)| v.get_ref().as_array())
        {
            for m in members.iter() {
                if let Some(g) = m.get_ref().as_str() {
                    facts.add(FactKind::WorkspaceGlob, g, rel, ctx.at(m.span()), P);
                }
            }
        }
    }
    true
}
