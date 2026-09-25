//! `lyra setup`: read-only static discovery for the setup skill (§10.2, §16.1).
//! Selects the workspace (§4.1), then reports bounded facts. Never executes project code.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::ExitCode;

use lyra_discovery::{Ambiguity, CacheReport, Fact, ScanStats, Truncation};
use lyra_protocol::reply::{PublicReply, ReplyContext, ReplyMeta, WorkspaceRef};
use lyra_protocol::workspace::{self, SelectError, SelectionReason};
use lyra_protocol::{AbsolutePath, ErrorCode, ErrorInfo};
use serde::Serialize;

use crate::output::{self, Mode};

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum SetupState {
    /// `<root>/.lyra/workspace.json` exists.
    Configured,
    NotSetup,
}

#[derive(Serialize)]
struct SetupReport {
    selected_root: Option<AbsolutePath>,
    reason: Option<SelectionReason>,
    /// The directory selection started from (explicit `--project` or the current directory).
    searched: Option<AbsolutePath>,
    /// Direct child directories that look like projects; set only when no root was selected.
    candidates: Vec<AbsolutePath>,
    setup_state: Option<SetupState>,
    facts: Vec<Fact>,
    ambiguities: Vec<Ambiguity>,
    truncated: Vec<Truncation>,
    scan: Option<ScanStats>,
    cache: Option<CacheReport>,
}

pub fn run(mode: Mode, project: Option<&Path>, refresh: bool) -> ExitCode {
    let cwd = match std::env::current_dir() {
        Ok(c) => c,
        Err(e) => {
            return output::fail(
                mode,
                ReplyContext::default(),
                ErrorInfo::new(
                    ErrorCode::NOT_FOUND,
                    format!("cannot read the current directory: {e}"),
                ),
            );
        }
    };
    let selected = match workspace::select(project, &cwd) {
        Ok(s) => s,
        Err(SelectError::NeedsProject {
            searched,
            candidates,
        }) => {
            let report = SetupReport {
                selected_root: None,
                reason: None,
                searched: Some(searched),
                candidates,
                setup_state: None,
                facts: Vec::new(),
                ambiguities: Vec::new(),
                truncated: Vec::new(),
                scan: None,
                cache: None,
            };
            let reply = PublicReply::success(ReplyContext::default(), report, ReplyMeta::default());
            return output::emit(mode, &reply, render);
        }
        Err(e) => return output::fail(mode, ReplyContext::default(), e.to_error_info()),
    };

    let root = selected.root.as_path();
    let outcome = lyra_discovery::discover(root, refresh);
    let mut discovery = outcome.discovery;
    if selected.reason == SelectionReason::ProjectManifest
        && let Some(a) = lyra_discovery::parent_monorepo(root)
    {
        discovery.ambiguities.push(a);
    }
    let meta = ReplyMeta {
        truncated: !discovery.truncated.is_empty(),
        ..ReplyMeta::default()
    };
    let ctx = ReplyContext {
        workspace: Some(WorkspaceRef {
            id: selected.id.clone(),
            root: selected.root.clone(),
        }),
        ..ReplyContext::default()
    };
    let report = SetupReport {
        setup_state: Some(if selected.is_setup() {
            SetupState::Configured
        } else {
            SetupState::NotSetup
        }),
        searched: Some(project.map_or_else(
            || AbsolutePath::from_path(&cwd).unwrap_or_else(|_| selected.root.clone()),
            |_| selected.root.clone(),
        )),
        selected_root: Some(selected.root),
        reason: Some(selected.reason),
        candidates: Vec::new(),
        facts: discovery.facts,
        ambiguities: discovery.ambiguities,
        truncated: discovery.truncated,
        scan: Some(discovery.scan),
        cache: Some(outcome.cache),
    };
    let reply = PublicReply::success(ctx, report, meta);
    output::emit(mode, &reply, render)
}

fn json_name<T: Serialize>(v: &T) -> String {
    serde_json::to_value(v)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// Facts listed in text mode; JSON always carries all of them.
const TEXT_FACTS: usize = 40;

fn render(r: &SetupReport) -> String {
    let Some(root) = &r.selected_root else {
        let searched = r.searched.as_ref().map_or("", |s| s.as_str());
        let mut s = format!("no workspace selected at {searched}");
        match r.candidates.len() {
            0 => s.push_str("\n  no candidate projects found; pass --project PATH"),
            n => {
                s.push_str(&format!("\n  {n} candidate(s); pass --project PATH:"));
                for c in &r.candidates {
                    s.push_str(&format!("\n    {c}"));
                }
            }
        }
        return s;
    };
    let state = match r.setup_state {
        Some(SetupState::Configured) => "configured",
        _ => "not set up",
    };
    let reason = r.reason.as_ref().map(json_name).unwrap_or_default();
    let mut s = format!("workspace {root} ({reason}, {state})");

    let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
    for f in &r.facts {
        *kinds.entry(json_name(&f.kind)).or_default() += 1;
    }
    let summary: Vec<String> = kinds.iter().map(|(k, n)| format!("{k} {n}")).collect();
    s.push_str(&format!(
        "\nfacts: {} ({})",
        r.facts.len(),
        summary.join(", ")
    ));
    for f in r.facts.iter().take(TEXT_FACTS) {
        let at = match f.source.lines {
            Some(l) => format!("{}:{}", f.source.path, l.start),
            None => f.source.path.clone(),
        };
        let owner = f
            .owner
            .as_deref()
            .map(|o| format!(" [{o}]"))
            .unwrap_or_default();
        let detail = f
            .detail
            .as_deref()
            .map(|d| format!(" = {d}"))
            .unwrap_or_default();
        s.push_str(&format!(
            "\n  {at}  {}{owner} {}{detail}",
            json_name(&f.kind),
            f.value
        ));
    }
    if r.facts.len() > TEXT_FACTS {
        s.push_str(&format!(
            "\n  ... {} more; use --json for all facts",
            r.facts.len() - TEXT_FACTS
        ));
    }
    if !r.ambiguities.is_empty() {
        s.push_str("\nambiguities:");
        for a in &r.ambiguities {
            s.push_str(&format!(
                "\n  {}: {} ({})",
                json_name(&a.kind),
                a.message,
                a.sources.join(", ")
            ));
        }
    }
    if r.truncated.is_empty() {
        s.push_str("\ntruncated: none");
    } else {
        s.push_str("\ntruncated (discovery is incomplete):");
        for t in &r.truncated {
            let limit = t.limit.map(|l| format!(" limit {l}")).unwrap_or_default();
            s.push_str(&format!(
                "\n  {}{limit}: {} omitted ({})",
                json_name(&t.reason),
                t.omitted_count,
                t.omitted.join(", ")
            ));
        }
    }
    if let Some(c) = &r.cache {
        let status = json_name(&c.status);
        let write = json_name(&c.write);
        s.push_str(&format!("\ncache: {status}, {write} ({})", c.path));
        if let Some(n) = &c.note {
            s.push_str(&format!("\n  {n}"));
        }
    }
    s
}
