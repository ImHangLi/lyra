//! `mira skills export DIR`: write the bundled skills of this Mira version to `DIR/mira/`
//! and `DIR/mira-extend/` without overwriting files the user changed (§17.2). Each skill
//! keeps a `.mira-install.json` record with the source version and the content hash of
//! every file Mira wrote. Mira never picks DIR itself and refuses a DIR inside the current
//! Git work tree: skills belong to the user, not to a repository.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use mira_protocol::ids::Digest;
use mira_protocol::reply::{PublicReply, ReplyContext, ReplyMeta};
use mira_protocol::workspace::git_worktree_root;
use mira_protocol::{ErrorCode, ErrorInfo};
use serde::{Deserialize, Serialize};

use super::ctx::Ctx;
use crate::output::invalid_argument;

macro_rules! bundled {
    ($($skill:literal => [$($file:literal),* $(,)?]),* $(,)?) => {
        const BUNDLED: &[(&str, &str, &str)] = &[
            $($(($skill, $file, include_str!(concat!("../../../../skills/", $skill, "/", $file))),)*)*
        ];
    };
}

bundled! {
    "mira" => ["SKILL.md", "references/cli.md", "references/setup.md", "references/updates.md"],
    "mira-extend" => [
        "SKILL.md",
        "references/manifest.md",
        "references/protocol.md",
        "templates/command/plugin.json",
        "templates/command/with_input.py",
        "templates/structured/plugin.json",
        "templates/structured/main.py",
    ],
}

const RECORD: &str = ".mira-install.json";
const SKILLS: [&str; 2] = ["mira", "mira-extend"];

#[derive(Serialize, Deserialize, Default)]
struct InstallRecord {
    source: String,
    files: BTreeMap<String, String>,
}

#[derive(Serialize, Default)]
pub struct Report {
    pub targets: Vec<String>,
    pub written: Vec<String>,
    pub updated: Vec<String>,
    pub unchanged: Vec<String>,
    /// Files the user changed; the new version is written next to them as `*.mira-new`.
    pub conflicts: Vec<Conflict>,
}

#[derive(Serialize)]
pub struct Conflict {
    pub path: String,
    pub new_version: String,
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
        source: format!("mira {}", mira_protocol::VERSION),
        files: BTreeMap::new(),
    };
    for (_, rel, content) in BUNDLED.iter().filter(|(s, _, _)| *s == skill) {
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
                    // Unmodified since Mira wrote it: safe to update.
                    std::fs::write(&target, content).map_err(|e| format!("{shown}: {e}"))?;
                    report.updated.push(shown);
                    record.files.insert((*rel).to_owned(), new_hash);
                } else {
                    let copy = PathBuf::from(format!("{shown}.mira-new"));
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

/// Resolves `dir` against `cwd` and through symlinks of its nearest existing ancestor.
fn resolve(cwd: &Path, dir: &Path) -> PathBuf {
    let abs = cwd.join(dir);
    let mut rest = Vec::new();
    let mut base = abs.as_path();
    loop {
        if let Ok(real) = std::fs::canonicalize(base) {
            return rest.iter().rev().fold(real, |p, c| p.join(c));
        }
        match (base.parent(), base.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_owned());
                base = parent;
            }
            _ => return abs,
        }
    }
}

/// The record of one exported skill: the Mira version that wrote it, if readable.
pub fn recorded_version(skill_dir: &Path) -> Option<String> {
    let bytes = std::fs::read(skill_dir.join(RECORD)).ok()?;
    let record: InstallRecord = serde_json::from_slice(&bytes).ok()?;
    record.source.strip_prefix("mira ").map(str::to_owned)
}

pub fn export(ctx: &Ctx, dir: &Path) -> ExitCode {
    let cwd = match std::env::current_dir() {
        Ok(c) => c,
        Err(e) => {
            return ctx.fail(
                ReplyContext::default(),
                ErrorInfo::new(
                    ErrorCode::NOT_FOUND,
                    format!("cannot read the current directory: {e}"),
                ),
            );
        }
    };
    let target = resolve(&cwd, dir);
    if let Some(repo) = git_worktree_root(&cwd).map(|r| resolve(&cwd, &r))
        && target.starts_with(&repo)
    {
        return ctx.fail(
            ReplyContext::default(),
            invalid_argument(format!(
                "{} is inside the Git work tree {}; export the skills to your own skills \
                 folder (for example ~/.claude/skills or ~/.agents/skills), not into a project",
                target.display(),
                repo.display()
            )),
        );
    }
    let mut report = Report::default();
    for skill in SKILLS {
        let skill_dir = target.join(skill);
        report
            .targets
            .push(skill_dir.to_string_lossy().into_owned());
        if let Err(e) = install_skill(&skill_dir, skill, &mut report) {
            return ctx.fail(
                ReplyContext::default(),
                ErrorInfo::new(
                    ErrorCode::STORAGE_UNAVAILABLE,
                    format!("cannot export skills: {e}"),
                ),
            );
        }
    }
    let reply = PublicReply::success(ReplyContext::default(), report, ReplyMeta::default());
    ctx.emit(&reply, |r| {
        let mut s = format!(
            "mira {} skills: {} written, {} updated, {} unchanged, {} kept\n  {}",
            mira_protocol::VERSION,
            r.written.len(),
            r.updated.len(),
            r.unchanged.len(),
            r.conflicts.len(),
            r.targets.join("\n  ")
        );
        for f in r.written.iter().chain(&r.updated) {
            s.push_str(&format!("\n  wrote {f}"));
        }
        for c in &r.conflicts {
            s.push_str(&format!(
                "\n  kept your edit: {} (new version: {})",
                c.path, c.new_version
            ));
        }
        s
    })
}
