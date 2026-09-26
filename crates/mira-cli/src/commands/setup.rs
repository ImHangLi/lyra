//! `mira setup`: read-only static discovery for the setup skill (§10.2, §16.1).
//! Selects the workspace (§4.1), then reports bounded facts. Never executes project code.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::ExitCode;

use mira_discovery::{Ambiguity, CacheReport, Fact, ScanStats, Truncation};
use mira_protocol::reply::{PublicReply, ReplyContext, ReplyMeta, WorkspaceRef};
use mira_protocol::workspace::{self, SelectError, SelectionReason};
use mira_protocol::{AbsolutePath, ErrorCode, ErrorInfo};
use serde::Serialize;

use super::skills::{AgentKind, install_into};
use crate::output::{self, Mode};

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum SetupState {
    /// `<root>/.mira/workspace.json` exists.
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
    let outcome = mira_discovery::discover(root, refresh);
    let mut discovery = outcome.discovery;
    if selected.reason == SelectionReason::ProjectManifest
        && let Some(a) = mira_discovery::parent_monorepo(root)
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
        let mut s = format!("no project selected at {searched}");
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
    let mut s = format!("project {root} ({reason}, {state})");

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

/// The instruction a person gives their agent after `mira setup`. No apostrophes, so it can
/// be quoted with single quotes in a shell.
const AGENT_PROMPT: &str = "Set up Mira for this repository. Follow the mira skill: run \
`mira setup --json`, read the docs and scripts, create plugins for the real dev commands, \
validate and apply them, run `mira doctor`, verify one tool, and tell me what is ready and \
what is not.";

fn on_path(program: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|p| {
        std::env::split_paths(&p).any(|d| {
            let f = d.join(program);
            f.is_file()
                && std::fs::metadata(&f).is_ok_and(|m| {
                    std::os::unix::fs::PermissionsExt::mode(&m.permissions()) & 0o111 != 0
                })
        })
    })
}

fn home_short(path: &str) -> String {
    match std::env::var("HOME") {
        Ok(h) if !h.is_empty() && path.starts_with(&h) => format!("~{}", &path[h.len()..]),
        _ => path.to_owned(),
    }
}

/// Copies `text` to the macOS clipboard; returns whether it worked.
fn copy_to_clipboard(text: &str) -> bool {
    use std::io::Write;
    let Ok(mut child) = std::process::Command::new("/usr/bin/pbcopy")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    else {
        return false;
    };
    let wrote = child
        .stdin
        .take()
        .is_some_and(|mut s| s.write_all(text.as_bytes()).is_ok());
    child.wait().is_ok_and(|s| s.success()) && wrote
}

/// A one-line summary of what discovery found, for people.
fn found_summary(facts: &[Fact]) -> String {
    use mira_discovery::FactKind as K;
    let count = |kinds: &[K]| facts.iter().filter(|f| kinds.contains(&f.kind)).count();
    let mut parts = Vec::new();
    let commands = count(&[
        K::PackageScript,
        K::PythonScript,
        K::MakeTarget,
        K::JustRecipe,
        K::TaskfileTask,
    ]);
    if commands > 0 {
        parts.push(format!("{commands} command(s)"));
    }
    let services = count(&[K::ComposeService]);
    if services > 0 {
        parts.push(format!("{services} Compose service(s)"));
    }
    let docs = count(&[K::Doc]);
    if docs > 0 {
        parts.push(format!("{docs} doc(s)"));
    }
    let env = count(&[K::EnvExampleVar]);
    if env > 0 {
        parts.push(format!("{env} env var(s) in examples"));
    }
    if parts.is_empty() {
        "no scripts, Compose files, or docs found".into()
    } else {
        format!("found {}", parts.join(", "))
    }
}

/// `mira setup` for people: install the agent skills, then print the one command that lets
/// the agent create the plugins. Never runs project code and never starts an agent.
pub fn guided(project: Option<&Path>, agent: Option<AgentKind>) -> ExitCode {
    use std::io::IsTerminal;
    let cwd = match std::env::current_dir() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("mira setup: cannot read the current directory: {e}");
            return ExitCode::from(3);
        }
    };
    let selected = match workspace::select(project, &cwd) {
        Ok(s) => s,
        Err(SelectError::NeedsProject {
            searched,
            candidates,
        }) => {
            let mut s = format!(
                "mira setup: {} is not a project.",
                home_short(searched.as_str())
            );
            if candidates.is_empty() {
                s.push_str("\n  cd into your repository, or pass --project PATH.");
            } else {
                s.push_str("\n  Projects here (pass --project PATH):");
                for c in &candidates {
                    s.push_str(&format!("\n    {}", home_short(c.as_str())));
                }
            }
            eprintln!("{s}");
            return ExitCode::from(3);
        }
        Err(e) => return output::fail(Mode::Text, ReplyContext::default(), e.to_error_info()),
    };
    let root = selected.root.as_path();
    let (has_claude, has_codex) = (on_path("claude"), on_path("codex"));
    let kind = agent.unwrap_or(if has_claude {
        AgentKind::Claude
    } else {
        AgentKind::Generic
    });
    let skills = match install_into(root, kind) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("mira setup: cannot install the agent skills: {e}");
            return ExitCode::from(8);
        }
    };
    let changed = skills.written.len() + skills.updated.len();
    let skills_line = match (kind, changed) {
        (_, 0) => "agent skills are up to date".to_owned(),
        (AgentKind::Claude, _) => "wrote .agents/skills and .claude/skills (add them to \
             .gitignore unless your team shares them)"
            .to_owned(),
        _ => "wrote .agents/skills (add it to .gitignore unless your team shares it)".to_owned(),
    };
    let mut out = format!(
        "Mira setup in {}\n  ✓ {skills_line}",
        home_short(selected.root.as_str())
    );
    for c in &skills.conflicts {
        out.push_str(&format!(
            "\n  ! kept your edited {}; the new version is next to it",
            home_short(&c.path)
        ));
    }
    if selected.is_setup() {
        out.push_str(
            "\n\nThis project already has Mira plugins. Run `mira`. To add a plugin, edit \
             .mira/plugins/ and run `mira reload`, or ask your agent.",
        );
        println!("{out}");
        return ExitCode::SUCCESS;
    }
    let facts = mira_discovery::discover(root, false).discovery.facts;
    out.push_str(&format!("\n  ✓ {}", found_summary(&facts)));
    out.push_str("\n\nGive your agent this prompt. It saves the repo's commands as plugins:\n");
    let quoted = format!("'{AGENT_PROMPT}'");
    let mut runners = Vec::new();
    if has_claude || kind == AgentKind::Claude {
        runners.push(format!("  claude {quoted}"));
    }
    if has_codex || kind == AgentKind::Codex {
        runners.push(format!("  codex {quoted}"));
    }
    if runners.is_empty() {
        out.push_str(&format!("\n  Ask your agent:\n\n  {AGENT_PROMPT}\n"));
    } else {
        out.push_str(&format!("\n{}\n", runners.join("\n")));
    }
    let copied = std::io::stdout().is_terminal() && copy_to_clipboard(AGENT_PROMPT);
    if copied {
        out.push_str("\nCopied the prompt to your clipboard.");
    }
    out.push_str("\nWhen the agent is done, run `mira`.");
    println!("{out}");
    ExitCode::SUCCESS
}
