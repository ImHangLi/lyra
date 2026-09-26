//! `lyra skills install`: copy the bundled skills into the workspace without overwriting
//! files the user changed (§17.2). Each installed skill keeps a `.lyra-install.json` record
//! with the source version and the content hash of every file Lyra wrote.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use lyra_protocol::ids::Digest;
use lyra_protocol::reply::{PublicReply, ReplyContext, ReplyMeta, WorkspaceRef};
use lyra_protocol::{ErrorCode, ErrorInfo};
use serde::{Deserialize, Serialize};

use super::ctx::Ctx;

macro_rules! bundled {
    ($($skill:literal => [$($file:literal),* $(,)?]),* $(,)?) => {
        const BUNDLED: &[(&str, &str, &str)] = &[
            $($(($skill, $file, include_str!(concat!("../../../../skills/", $skill, "/", $file))),)*)*
        ];
    };
}

bundled! {
    "lyra" => ["SKILL.md", "references/cli.md", "references/setup.md", "references/updates.md"],
    "lyra-extend" => [
        "SKILL.md",
        "references/manifest.md",
        "references/protocol.md",
        "templates/command/plugin.json",
        "templates/command/with_input.py",
        "templates/structured/plugin.json",
        "templates/structured/main.py",
    ],
}

const RECORD: &str = ".lyra-install.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum AgentKind {
    Claude,
    Codex,
    Generic,
}

#[derive(Serialize, Deserialize, Default)]
struct InstallRecord {
    source: String,
    files: BTreeMap<String, String>,
}

#[derive(Serialize, Default)]
struct Report {
    targets: Vec<String>,
    written: Vec<String>,
    updated: Vec<String>,
    unchanged: Vec<String>,
    /// Files the user changed; the new version is written next to them as `*.lyra-new`.
    conflicts: Vec<Conflict>,
}

#[derive(Serialize)]
struct Conflict {
    path: String,
    new_version: String,
}

fn hash(bytes: &[u8]) -> String {
    Digest::of_bytes(bytes).to_string()
}

fn install_skill(dir: &Path, skill: &str, report: &mut Report) -> Result<(), String> {
    let record_path = dir.join(RECORD);
    let old: InstallRecord = std::fs::read(&record_path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    let mut record = InstallRecord {
        source: format!("lyra {}", lyra_protocol::VERSION),
        files: BTreeMap::new(),
    };
    for (s, rel, content) in BUNDLED.iter().filter(|(s, _, _)| *s == skill) {
        let _ = s;
        let target = dir.join(rel);
        let shown = target.to_string_lossy().into_owned();
        let new_hash = hash(content.as_bytes());
        match std::fs::read(&target) {
            Err(_) => {
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent)
                        .map_err(|e| format!("{}: {e}", parent.display()))?;
                }
                std::fs::write(&target, content).map_err(|e| format!("{shown}: {e}"))?;
                report.written.push(shown);
                record.files.insert((*rel).to_owned(), new_hash);
            }
            Ok(current) => {
                let current_hash = hash(&current);
                if current_hash == new_hash {
                    report.unchanged.push(shown);
                    record.files.insert((*rel).to_owned(), new_hash);
                } else if old.files.get(*rel) == Some(&current_hash) {
                    // Unmodified since Lyra wrote it: safe to update.
                    std::fs::write(&target, content).map_err(|e| format!("{shown}: {e}"))?;
                    report.updated.push(shown);
                    record.files.insert((*rel).to_owned(), new_hash);
                } else {
                    let copy = PathBuf::from(format!("{shown}.lyra-new"));
                    std::fs::write(&copy, content)
                        .map_err(|e| format!("{}: {e}", copy.display()))?;
                    report.conflicts.push(Conflict {
                        path: shown,
                        new_version: copy.to_string_lossy().into_owned(),
                    });
                    // Keep the old record entry so a later install still sees the user's edit.
                    if let Some(h) = old.files.get(*rel) {
                        record.files.insert((*rel).to_owned(), h.clone());
                    }
                }
            }
        }
    }
    let bytes = serde_json::to_vec_pretty(&record).map_err(|e| e.to_string())?;
    std::fs::write(&record_path, bytes).map_err(|e| format!("{}: {e}", record_path.display()))
}

pub fn install(ctx: &Ctx, agent: AgentKind) -> ExitCode {
    let selected = match ctx.select() {
        Ok(s) => s,
        Err(e) => return ctx.fail(ReplyContext::default(), e),
    };
    let root = selected.root.as_path().to_path_buf();
    let rctx = ReplyContext {
        workspace: Some(WorkspaceRef {
            id: selected.id.clone(),
            root: selected.root.clone(),
        }),
        ..ReplyContext::default()
    };
    let mut bases = vec![root.join(".agents/skills")];
    if agent == AgentKind::Claude {
        bases.push(root.join(".claude/skills"));
    }
    let mut report = Report::default();
    for base in &bases {
        for skill in ["lyra", "lyra-extend"] {
            let dir = base.join(skill);
            report.targets.push(dir.to_string_lossy().into_owned());
            if let Err(e) = install_skill(&dir, skill, &mut report) {
                return ctx.fail(
                    rctx,
                    ErrorInfo::new(
                        ErrorCode::STORAGE_UNAVAILABLE,
                        format!("cannot install skills: {e}"),
                    ),
                );
            }
        }
    }
    let reply = PublicReply::success(rctx, report, ReplyMeta::default());
    ctx.emit(&reply, |r| {
        let mut s = format!(
            "skills: {} written, {} updated, {} unchanged, {} conflict(s)\n  {}",
            r.written.len(),
            r.updated.len(),
            r.unchanged.len(),
            r.conflicts.len(),
            r.targets.join("\n  ")
        );
        for c in &r.conflicts {
            s.push_str(&format!(
                "\n  kept your edit: {} (new version: {})",
                c.path, c.new_version
            ));
        }
        s
    })
}
