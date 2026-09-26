//! Bounded breadth-first directory walk and bounded file reads.

use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use mira_protocol::Digest;

use crate::text::join;
use crate::{Limits, MAX_OMITTED_LISTED, ScanStats, Truncation, TruncationReason};

/// What a discovered path is. Only classes with [`Class::reads`] are opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    PackageJson,
    /// Package manager and ecosystem that own the lockfile. Never read.
    Lockfile(&'static str, &'static str),
    PnpmWorkspace,
    MonorepoTool,
    Pyproject,
    Requirements,
    PythonProjectFile,
    Compose,
    Makefile,
    Justfile,
    Taskfile,
    EnvExample,
    /// A real env file. Never read.
    EnvFile,
    RuntimeVersion,
    Doc,
    DocDir,
    ProjectFile,
}

impl Class {
    /// Parse priority under the fact limit: lower ranks are kept first.
    pub fn rank(self) -> u8 {
        match self {
            Self::PackageJson
            | Self::Pyproject
            | Self::Compose
            | Self::Makefile
            | Self::Justfile
            | Self::Taskfile
            | Self::PnpmWorkspace
            | Self::Lockfile(..) => 0,
            Self::MonorepoTool
            | Self::Requirements
            | Self::PythonProjectFile
            | Self::RuntimeVersion
            | Self::ProjectFile => 1,
            Self::Doc | Self::DocDir | Self::EnvFile => 2,
            Self::EnvExample => 3,
        }
    }

    pub fn reads(self) -> bool {
        matches!(
            self,
            Self::PackageJson
                | Self::PnpmWorkspace
                | Self::Pyproject
                | Self::Compose
                | Self::Makefile
                | Self::Justfile
                | Self::Taskfile
                | Self::EnvExample
                | Self::RuntimeVersion
        )
    }
}

/// Directories never entered: VCS data, dependencies, build output, caches, `.mira`.
const SKIP_DIRS: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    ".mira",
    "node_modules",
    "bower_components",
    ".venv",
    "venv",
    "vendor",
    "dist",
    "build",
    "target",
    ".next",
    ".nuxt",
    ".svelte-kit",
    ".turbo",
    ".parcel-cache",
    ".yarn",
    ".gradle",
    ".cache",
    ".direnv",
    "__pycache__",
    ".tox",
    ".nox",
    ".mypy_cache",
    ".pytest_cache",
    ".ruff_cache",
];

pub fn is_base_compose(name: &str) -> bool {
    matches!(
        name,
        "compose.yaml" | "compose.yml" | "docker-compose.yaml" | "docker-compose.yml"
    )
}

fn is_compose(name: &str) -> bool {
    let yaml = name.ends_with(".yaml") || name.ends_with(".yml");
    yaml && (is_base_compose(name)
        || name.starts_with("compose.")
        || name.starts_with("docker-compose."))
}

fn is_env_like(lower: &str) -> bool {
    lower.starts_with(".env") || lower.ends_with(".env") || lower.contains(".env.")
}

fn is_env_example(lower: &str) -> bool {
    (is_env_like(lower) || lower.starts_with("env."))
        && ["example", "sample", "template", "dist", "defaults"]
            .iter()
            .any(|w| lower.contains(w))
}

pub fn classify_file(name: &str) -> Option<Class> {
    let lower = name.to_ascii_lowercase();
    let class = match name {
        "package.json" => Class::PackageJson,
        "package-lock.json" | "npm-shrinkwrap.json" => Class::Lockfile("npm", "javascript"),
        "yarn.lock" => Class::Lockfile("yarn", "javascript"),
        "pnpm-lock.yaml" => Class::Lockfile("pnpm", "javascript"),
        "bun.lockb" | "bun.lock" => Class::Lockfile("bun", "javascript"),
        "poetry.lock" => Class::Lockfile("poetry", "python"),
        "uv.lock" => Class::Lockfile("uv", "python"),
        "Pipfile.lock" => Class::Lockfile("pipenv", "python"),
        "pdm.lock" => Class::Lockfile("pdm", "python"),
        "pnpm-workspace.yaml" => Class::PnpmWorkspace,
        "turbo.json" | "nx.json" | "lerna.json" | "rush.json" => Class::MonorepoTool,
        "pyproject.toml" => Class::Pyproject,
        "setup.py" | "setup.cfg" | "Pipfile" => Class::PythonProjectFile,
        "Makefile" | "makefile" | "GNUmakefile" => Class::Makefile,
        "justfile" | "Justfile" | ".justfile" => Class::Justfile,
        "Taskfile.yml" | "Taskfile.yaml" | "taskfile.yml" | "taskfile.yaml"
        | "Taskfile.dist.yml" | "Taskfile.dist.yaml" => Class::Taskfile,
        ".nvmrc" | ".node-version" | ".python-version" | ".tool-versions" => Class::RuntimeVersion,
        "Cargo.toml" | "go.mod" | "Dockerfile" | "Procfile" => Class::ProjectFile,
        "AGENTS.md" | "CLAUDE.md" => Class::Doc,
        _ if is_compose(name) => Class::Compose,
        _ if lower.starts_with("requirements")
            && (lower.ends_with(".txt") || lower.ends_with(".in")) =>
        {
            Class::Requirements
        }
        _ if is_env_example(&lower) => Class::EnvExample,
        _ if is_env_like(&lower) && lower.starts_with(".env") => Class::EnvFile,
        _ if [
            "readme",
            "contributing",
            "development",
            "developing",
            "hacking",
        ]
        .iter()
        .any(|p| lower.starts_with(p)) =>
        {
            Class::Doc
        }
        _ => return None,
    };
    Some(class)
}

fn classify_dir(name: &str) -> Option<Class> {
    matches!(name, "docs" | "doc" | "documentation").then_some(Class::DocDir)
}

/// Collects omitted scope per truncation reason.
#[derive(Default)]
pub struct TruncationLog {
    events: BTreeMap<TruncationReason, (Option<u64>, Vec<String>, u64)>,
}

impl TruncationLog {
    pub fn record(&mut self, reason: TruncationReason, limit: Option<u64>, path: String) {
        self.record_many(reason, limit, std::iter::once(path));
    }

    pub fn record_many(
        &mut self,
        reason: TruncationReason,
        limit: Option<u64>,
        paths: impl IntoIterator<Item = String>,
    ) {
        let e = self
            .events
            .entry(reason)
            .or_insert_with(|| (limit, Vec::new(), 0));
        for p in paths {
            e.2 += 1;
            if e.1.len() < MAX_OMITTED_LISTED {
                e.1.push(p);
            }
        }
    }

    pub fn into_vec(self) -> Vec<Truncation> {
        self.events
            .into_iter()
            .map(|(reason, (limit, omitted, omitted_count))| Truncation {
                reason,
                limit,
                omitted,
                omitted_count,
            })
            .collect()
    }
}

pub struct PlanEntry {
    pub rel: String,
    pub abs: PathBuf,
    pub class: Class,
}

pub struct Plan {
    pub entries: Vec<PlanEntry>,
    pub log: TruncationLog,
    pub stats: ScanStats,
}

/// Mira's own files: `.mira/` and installed Mira skills (`.agents/skills/mira*`,
/// `.claude/skills/mira*`). They are not project facts, so the scan skips them entirely,
/// including symlinked skill installs that would otherwise be reported as truncation.
fn mira_owned(parent_rel: &str, name: &str) -> bool {
    if name == ".mira" {
        return true;
    }
    let skills_dir = [".agents/skills", ".claude/skills"]
        .iter()
        .any(|d| parent_rel == *d || parent_rel.ends_with(&format!("/{d}")));
    skills_dir && name.starts_with("mira")
}

fn shown(rel: &str) -> String {
    if rel.is_empty() {
        ".".to_owned()
    } else {
        format!("{rel}/")
    }
}

/// Lists `root` breadth-first within `limits`. Never follows directory symlinks and never
/// leaves `root`.
pub fn walk(root: &Path, limits: &Limits) -> Plan {
    let mut stats = ScanStats {
        limits: *limits,
        entries_seen: 0,
        dirs_listed: 0,
        files_read: 0,
        bytes_read: 0,
        dirs_skipped: 0,
    };
    let mut log = TruncationLog::default();
    let mut entries = Vec::new();
    let mut queue: VecDeque<(PathBuf, String, usize)> = VecDeque::new();
    queue.push_back((root.to_path_buf(), String::new(), 0));
    let max_entries = limits.max_entries as u64;

    while let Some((dir, rel, depth)) = queue.pop_front() {
        if stats.entries_seen >= max_entries {
            let rest =
                std::iter::once(shown(&rel)).chain(queue.drain(..).map(|(_, r, _)| shown(&r)));
            log.record_many(TruncationReason::MaxEntries, Some(max_entries), rest);
            break;
        }
        let Ok(read) = fs::read_dir(&dir) else {
            log.record(TruncationReason::Unreadable, None, shown(&rel));
            continue;
        };
        stats.dirs_listed += 1;
        let mut listed = Vec::new();
        let mut partial = false;
        for item in read {
            if stats.entries_seen >= max_entries {
                partial = true;
                break;
            }
            stats.entries_seen += 1;
            if let Ok(item) = item {
                listed.push(item);
            }
        }
        listed.sort_by_key(fs::DirEntry::file_name);
        for item in listed {
            let Ok(name) = item.file_name().into_string() else {
                let lossy = item.file_name().to_string_lossy().into_owned();
                log.record(
                    TruncationReason::UnsupportedPathEncoding,
                    None,
                    join(&rel, &lossy),
                );
                continue;
            };
            if mira_owned(&rel, &name) {
                stats.dirs_skipped += 1;
                continue;
            }
            let child_rel = join(&rel, &name);
            let path = item.path();
            let Ok(ft) = item.file_type() else {
                log.record(TruncationReason::Unreadable, None, child_rel);
                continue;
            };
            if ft.is_symlink() {
                match fs::canonicalize(&path) {
                    Ok(target) if target.starts_with(root) => {
                        // In-root file symlinks are read; directory symlinks are not followed
                        // because their real directory is already inside the scan.
                        if target.is_file()
                            && let Some(class) = classify_file(&name)
                        {
                            entries.push(PlanEntry {
                                rel: child_rel,
                                abs: target,
                                class,
                            });
                        }
                    }
                    Ok(_) => log.record(TruncationReason::SymlinkOutsideRoot, None, child_rel),
                    Err(_) => {}
                }
                continue;
            }
            if ft.is_dir() {
                if SKIP_DIRS.contains(&name.as_str()) || path.join("pyvenv.cfg").is_file() {
                    stats.dirs_skipped += 1;
                    continue;
                }
                if depth + 1 > limits.max_depth {
                    log.record(
                        TruncationReason::MaxDepth,
                        Some(limits.max_depth as u64),
                        shown(&child_rel),
                    );
                    continue;
                }
                if let Some(class) = classify_dir(&name) {
                    entries.push(PlanEntry {
                        rel: child_rel.clone(),
                        abs: path.clone(),
                        class,
                    });
                }
                queue.push_back((path, child_rel, depth + 1));
            } else if ft.is_file()
                && let Some(class) = classify_file(&name)
            {
                entries.push(PlanEntry {
                    rel: child_rel,
                    abs: path,
                    class,
                });
            }
        }
        if partial {
            let rest = std::iter::once(format!("{} (partly listed)", shown(&rel)))
                .chain(queue.drain(..).map(|(_, r, _)| shown(&r)));
            log.record_many(TruncationReason::MaxEntries, Some(max_entries), rest);
            break;
        }
    }
    Plan {
        entries,
        log,
        stats,
    }
}

pub struct InputFile {
    pub rel: String,
    pub class: Class,
    /// UTF-8 content for read classes that were read within the limits.
    pub content: Option<String>,
    /// Size and content hash of every file whose bytes were read.
    pub fingerprint: Option<(u64, Digest)>,
}

pub struct Inputs {
    pub files: Vec<InputFile>,
    pub truncated: Vec<Truncation>,
    pub stats: ScanStats,
}

/// Reads the declaration files of a plan within the single-file and total-byte limits.
pub fn read_inputs(plan: Plan, limits: &Limits) -> Inputs {
    let Plan {
        entries,
        mut log,
        mut stats,
    } = plan;
    let mut files = Vec::with_capacity(entries.len());
    for e in entries {
        let mut file = InputFile {
            rel: e.rel,
            class: e.class,
            content: None,
            fingerprint: None,
        };
        if e.class.reads() {
            match read_bounded(&e.abs, limits, stats.bytes_read) {
                Ok(bytes) => {
                    stats.files_read += 1;
                    stats.bytes_read += bytes.len() as u64;
                    file.fingerprint = Some((bytes.len() as u64, Digest::of_bytes(&bytes)));
                    match String::from_utf8(bytes) {
                        Ok(s) => file.content = Some(s),
                        Err(_) => log.record(TruncationReason::Unreadable, None, file.rel.clone()),
                    }
                }
                Err(reason) => {
                    let limit = match reason {
                        TruncationReason::MaxFileBytes => Some(limits.max_file_bytes),
                        TruncationReason::MaxTotalTextBytes => Some(limits.max_total_text_bytes),
                        _ => None,
                    };
                    log.record(reason, limit, file.rel.clone());
                }
            }
        }
        files.push(file);
    }
    Inputs {
        files,
        truncated: log.into_vec(),
        stats,
    }
}

fn read_bounded(path: &Path, limits: &Limits, used: u64) -> Result<Vec<u8>, TruncationReason> {
    let meta = fs::metadata(path).map_err(|_| TruncationReason::Unreadable)?;
    if !meta.is_file() {
        return Err(TruncationReason::Unreadable);
    }
    if meta.len() > limits.max_file_bytes {
        return Err(TruncationReason::MaxFileBytes);
    }
    if used + meta.len() > limits.max_total_text_bytes {
        return Err(TruncationReason::MaxTotalTextBytes);
    }
    let f = fs::File::open(path).map_err(|_| TruncationReason::Unreadable)?;
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    f.take(limits.max_file_bytes + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| TruncationReason::Unreadable)?;
    if bytes.len() as u64 > limits.max_file_bytes {
        return Err(TruncationReason::MaxFileBytes);
    }
    if used + bytes.len() as u64 > limits.max_total_text_bytes {
        return Err(TruncationReason::MaxTotalTextBytes);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mira_skill_installs_are_not_scanned() {
        let root = std::env::temp_dir().join(format!("mira-walk-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let deep = root.join(".claude/skills/mira/a/b/c/d/e/f/g/h");
        fs::create_dir_all(&deep).expect("create skill dir");
        fs::create_dir_all(root.join(".agents/skills")).expect("create agents dir");
        std::os::unix::fs::symlink("/tmp", root.join(".agents/skills/mira-setup"))
            .expect("symlink");
        fs::write(root.join("package.json"), "{}").expect("write package.json");
        let limits = Limits {
            max_depth: 3,
            ..Limits::DEFAULT
        };
        let plan = walk(&root, &limits);
        let omitted: Vec<String> = plan
            .log
            .into_vec()
            .into_iter()
            .flat_map(|t| t.omitted)
            .collect();
        assert!(omitted.is_empty(), "unexpected truncation: {omitted:?}");
        assert!(plan.entries.iter().all(|e| !e.rel.contains("skills/mira")));
        assert!(!mira_owned(".claude/skills", "other"));
        assert!(mira_owned("", ".mira"));
        let _ = fs::remove_dir_all(root);
    }
}
