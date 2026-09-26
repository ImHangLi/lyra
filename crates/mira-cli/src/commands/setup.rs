//! `mira setup` (§10.2, §16.1): selects the workspace (§4.1), installs the agent skills, and
//! tells the agent what to do next. The agent reads the repository itself; Mira never runs
//! project code here.

use std::path::Path;
use std::process::ExitCode;

use mira_protocol::reply::{PublicReply, ReplyContext, ReplyMeta, WorkspaceRef};
use mira_protocol::workspace::{self, SelectError, SelectionReason};
use mira_protocol::{AbsolutePath, ErrorCode, ErrorInfo};
use serde::Serialize;

use super::skills::{self, AgentKind, install_into};
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
    /// The installed agent skills; absent when no root was selected.
    skills: Option<skills::Report>,
}

/// Claude Code when it is installed, else the generic `.agents/skills` layout.
fn default_agent(agent: Option<AgentKind>) -> AgentKind {
    agent.unwrap_or(if on_path("claude") {
        AgentKind::Claude
    } else {
        AgentKind::Generic
    })
}

/// `mira setup --json`: install the skills and report the selected root and its state.
pub fn run(mode: Mode, project: Option<&Path>, agent: Option<AgentKind>) -> ExitCode {
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
                skills: None,
            };
            let reply = PublicReply::success(ReplyContext::default(), report, ReplyMeta::default());
            return output::emit(mode, &reply, render);
        }
        Err(e) => return output::fail(mode, ReplyContext::default(), e.to_error_info()),
    };
    let ctx = ReplyContext {
        workspace: Some(WorkspaceRef {
            id: selected.id.clone(),
            root: selected.root.clone(),
        }),
        ..ReplyContext::default()
    };
    let installed = match install_into(selected.root.as_path(), default_agent(agent)) {
        Ok(r) => r,
        Err(e) => {
            return output::fail(
                mode,
                ctx,
                ErrorInfo::new(
                    ErrorCode::STORAGE_UNAVAILABLE,
                    format!("cannot install the agent skills: {e}"),
                ),
            );
        }
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
        skills: Some(installed),
    };
    let reply = PublicReply::success(ctx, report, ReplyMeta::default());
    output::emit(mode, &reply, render)
}

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
    let mut s = format!("project {root} ({state})");
    if let Some(k) = &r.skills {
        s.push_str(&format!("\nskills:\n  {}", k.targets.join("\n  ")));
    }
    s
}

/// The instruction a person gives their agent after `mira setup`. No apostrophes, so it can
/// be quoted with single quotes in a shell.
const AGENT_PROMPT: &str = "Set up Mira for this repository. Follow the mira skill: read the \
README, docs, and scripts, create plugins for the real dev commands, \
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
    let kind = default_agent(agent);
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
