//! YAML declaration files: Compose, Taskfile, and `pnpm-workspace.yaml`.
//! Environment sections are never read out; only names, images, builds, ports, profiles.

use crate::yaml::{self, Node};
use crate::{Certainty, FactKind, Facts, Lines};

const P: Certainty = Certainty::Parsed;

fn line(n: &Node) -> Option<Lines> {
    Some(Lines::one(n.line()))
}

/// Declared text of a long-syntax port mapping, e.g. `target=80 published=8080`.
fn port_text(node: &Node) -> Option<String> {
    match node {
        Node::Scalar(s, _) => Some(s.clone()),
        Node::Map(entries, _) => {
            let parts: Vec<String> = entries
                .iter()
                .filter(|e| {
                    matches!(
                        e.key.as_str(),
                        "target" | "published" | "protocol" | "host_ip" | "mode"
                    )
                })
                .filter_map(|e| e.value.as_str().map(|v| format!("{}={v}", e.key)))
                .collect();
            (!parts.is_empty()).then(|| parts.join(" "))
        }
        _ => None,
    }
}

pub fn compose(rel: &str, text: &str, facts: &mut Facts) -> bool {
    let Some(doc) = yaml::parse(text) else {
        return false;
    };
    facts.add(FactKind::ComposeFile, rel, rel, None, P);
    let Some(services) = doc.get("services") else {
        return true;
    };
    for svc in services.value.entries() {
        if svc.key.is_empty() {
            continue;
        }
        let name = svc.key.as_str();
        facts.add(
            FactKind::ComposeService,
            name,
            rel,
            Some(Lines::one(svc.key_line)),
            P,
        );
        if let Some(image) = svc.value.get("image")
            && let Some(v) = image.value.as_str()
        {
            facts
                .add(FactKind::ComposeImage, v, rel, line(&image.value), P)
                .with(Some(name), None);
        }
        if let Some(build) = svc.value.get("build") {
            let context = match &build.value {
                Node::Scalar(s, _) => Some(s.as_str()),
                Node::Map(..) => Some(
                    build
                        .value
                        .get("context")
                        .and_then(|c| c.value.as_str())
                        .unwrap_or("."),
                ),
                _ => None,
            };
            if let Some(context) = context {
                facts
                    .add(
                        FactKind::ComposeBuild,
                        context,
                        rel,
                        Some(Lines::one(build.key_line)),
                        P,
                    )
                    .with(Some(name), None);
            }
        }
        if let Some(ports) = svc.value.get("ports") {
            for p in ports.value.items() {
                if let Some(t) = port_text(p) {
                    facts
                        .add(FactKind::ComposePort, t, rel, line(p), P)
                        .with(Some(name), None);
                }
            }
        }
        if let Some(profiles) = svc.value.get("profiles") {
            for p in profiles.value.items() {
                if let Some(v) = p.as_str() {
                    facts
                        .add(FactKind::ComposeProfile, v, rel, line(p), P)
                        .with(Some(name), None);
                }
            }
        }
    }
    true
}

pub fn taskfile(rel: &str, text: &str, facts: &mut Facts) -> bool {
    let Some(doc) = yaml::parse(text) else {
        return false;
    };
    if let Some(tasks) = doc.get("tasks") {
        for t in tasks.value.entries() {
            if t.key.is_empty() {
                continue;
            }
            let desc = t
                .value
                .get("desc")
                .or_else(|| t.value.get("summary"))
                .and_then(|d| d.value.as_str());
            facts
                .add(
                    FactKind::TaskfileTask,
                    &t.key,
                    rel,
                    Some(Lines::one(t.key_line)),
                    P,
                )
                .with(None, desc);
        }
    }
    if let Some(includes) = doc.get("includes") {
        for inc in includes.value.entries() {
            let path = match &inc.value {
                Node::Scalar(s, _) => Some(s.as_str()),
                n => n.get("taskfile").and_then(|t| t.value.as_str()),
            };
            facts
                .add(
                    FactKind::TaskfileInclude,
                    &inc.key,
                    rel,
                    Some(Lines::one(inc.key_line)),
                    P,
                )
                .with(None, path);
        }
    }
    true
}

pub fn pnpm_workspace(rel: &str, text: &str, facts: &mut Facts) -> bool {
    let Some(doc) = yaml::parse(text) else {
        return false;
    };
    facts.add(FactKind::MonorepoTool, rel, rel, None, P);
    if let Some(packages) = doc.get("packages") {
        for p in packages.value.items() {
            if let Some(g) = p.as_str() {
                facts.add(FactKind::WorkspaceGlob, g, rel, line(p), P);
            }
        }
    }
    true
}
