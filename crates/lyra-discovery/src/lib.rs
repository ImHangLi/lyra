//! Bounded, read-only static discovery of project facts (§16.1).
//!
//! The scanner lists directories and reads a small set of declaration files (package
//! manifests, Compose files, task files, env examples). It never executes project code,
//! shells, package managers, or `make`. Every fact names the file it came from. Hitting a
//! scan limit is always reported in [`Discovery::truncated`]; the result never claims to be
//! complete when it is not.

use std::path::Path;

use serde::{Deserialize, Serialize};

mod cache;
mod compose;
mod env;
mod js;
mod python;
mod tasks;
mod text;
mod walk;
mod yaml;

pub use cache::{CacheReport, CacheStatus, CacheWrite, MissReason, cache_path};

/// Version of the fact format and parsers. A change invalidates every cache.
pub const DISCOVERY_FORMAT: u32 = 1;

/// Scan limits (§16.1). Byte limits count raw file bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    /// Directory levels below the root that are listed (the root is level 0).
    pub max_depth: usize,
    /// Directory entries examined across the whole scan.
    pub max_entries: usize,
    /// Largest single file that is read.
    pub max_file_bytes: u64,
    /// Total bytes read across all files.
    pub max_total_text_bytes: u64,
    /// Facts returned; keeps the reply well under the public reply size limit.
    pub max_facts: usize,
}

impl Limits {
    pub const DEFAULT: Self = Self {
        max_depth: 4,
        max_entries: 5000,
        max_file_bytes: 1024 * 1024,
        max_total_text_bytes: 8 * 1024 * 1024,
        max_facts: 1000,
    };
}

impl Default for Limits {
    fn default() -> Self {
        Self::DEFAULT
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Certainty {
    /// Read from the declared content of a file with a real parser.
    Parsed,
    /// Inferred from a file or directory name only; the content was not interpreted.
    Hint,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactKind {
    /// Package or project name (`package.json` `name`, `[project].name`, `[tool.poetry].name`).
    PackageName,
    /// `package.json` script; `value` is the script name, `detail` its declared command text.
    PackageScript,
    /// `package.json` `packageManager` field.
    PackageManager,
    /// A lockfile exists; `owner` is the package manager that writes it.
    Lockfile,
    /// A workspace member glob (`workspaces`, `pnpm-workspace.yaml`, `[tool.uv.workspace]`).
    WorkspaceGlob,
    /// A monorepo tool configuration file exists (`turbo.json`, `nx.json`, ...).
    MonorepoTool,
    /// A declared runtime requirement (`engines`, `requires-python`); `owner` is the runtime.
    RuntimeRequirement,
    /// A pinned runtime version file (`.nvmrc`, `.python-version`, `.tool-versions`).
    RuntimeVersion,
    /// `[project.scripts]` or `[tool.poetry.scripts]` entry point; `detail` is the target.
    PythonScript,
    /// A `[tool.<name>]` table in `pyproject.toml`.
    PythonTool,
    /// `[build-system].build-backend`.
    BuildBackend,
    /// A `requirements*.txt` or `requirements*.in` file exists.
    RequirementsFile,
    /// A Python project file that is never executed (`setup.py`, `setup.cfg`, `Pipfile`).
    PythonProjectFile,
    /// A Compose file exists and was parsed.
    ComposeFile,
    /// A Compose service; `value` is the service name.
    ComposeService,
    /// A service `image`; `owner` is the service.
    ComposeImage,
    /// A service `build` context; `owner` is the service.
    ComposeBuild,
    /// A service port as declared text; `owner` is the service.
    ComposePort,
    /// A service profile; `owner` is the service.
    ComposeProfile,
    /// A Makefile target name; `detail` is a trailing `##` comment when present.
    MakeTarget,
    /// A Makefile `include` directive as declared text; never followed or evaluated.
    MakeInclude,
    /// A justfile recipe name; `detail` is the comment line above it when present.
    JustRecipe,
    /// A Taskfile task name; `detail` is its `desc` when present.
    TaskfileTask,
    /// A Taskfile `includes` entry; `detail` is the declared path.
    TaskfileInclude,
    /// A README, CONTRIBUTING, or other development document location.
    Doc,
    /// A documentation directory location.
    DocDir,
    /// A variable name declared in an env example file. Values are never read out.
    EnvExampleVar,
    /// A real env file exists. It is never read.
    EnvFile,
    /// Another project file that exists (`Cargo.toml`, `go.mod`, `Dockerfile`).
    ProjectFile,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lines {
    pub start: usize,
    pub end: usize,
}

impl Lines {
    pub fn one(line: usize) -> Self {
        Self {
            start: line,
            end: line,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    /// Path relative to the selected root, `/`-separated.
    pub path: String,
    /// 1-based inclusive line range, only when the position is reliable.
    pub lines: Option<Lines>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fact {
    pub kind: FactKind,
    pub value: String,
    /// The entity the fact belongs to, e.g. the Compose service of a port.
    pub owner: Option<String>,
    /// Declared text that explains the value, e.g. a script command. Never an env value.
    pub detail: Option<String>,
    pub source: Source,
    pub certainty: Certainty,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AmbiguityKind {
    /// Lockfiles of different package managers in one directory.
    MultipleLockfiles,
    /// `packageManager` disagrees with the lockfile in the same directory.
    PackageManagerMismatch,
    /// More than one base Compose file in one directory.
    MultipleComposeFiles,
    /// The root declares commands in more than one place; the scanner picks none.
    MultipleCommandSources,
    /// The root sits inside a directory that declares a monorepo.
    ParentMonorepo,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ambiguity {
    pub kind: AmbiguityKind,
    pub message: String,
    /// Paths relative to the root (or absolute for [`AmbiguityKind::ParentMonorepo`]).
    pub sources: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TruncationReason {
    MaxDepth,
    MaxEntries,
    MaxFileBytes,
    MaxTotalTextBytes,
    MaxFacts,
    SymlinkOutsideRoot,
    UnsupportedPathEncoding,
    Unreadable,
    ParseFailed,
}

/// Omitted paths listed per truncation event.
pub const MAX_OMITTED_LISTED: usize = 20;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Truncation {
    pub reason: TruncationReason,
    /// The limit value that was hit, when the reason is a numeric limit.
    pub limit: Option<u64>,
    /// Omitted scope: directories not listed or files not read (first entries only).
    pub omitted: Vec<String>,
    /// Total number of omitted directories, files, or facts.
    pub omitted_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScanStats {
    pub limits: Limits,
    pub entries_seen: u64,
    pub dirs_listed: u64,
    pub files_read: u64,
    pub bytes_read: u64,
    /// Dependency, build, VCS, and `.lyra` directories skipped by rule.
    pub dirs_skipped: u64,
}

/// The cacheable scan result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Discovery {
    pub facts: Vec<Fact>,
    pub ambiguities: Vec<Ambiguity>,
    pub truncated: Vec<Truncation>,
    pub scan: ScanStats,
}

pub struct Outcome {
    pub discovery: Discovery,
    pub cache: CacheReport,
}

/// Scans `root` (a canonical directory) within [`Limits::DEFAULT`]. Reuses the cache at
/// [`cache_path`] when the fingerprints of every read file and the scan parameters match and
/// the cache is younger than seven days; `refresh` always rescans.
pub fn discover(root: &Path, refresh: bool) -> Outcome {
    let limits = Limits::DEFAULT;
    let plan = walk::walk(root, &limits);
    let inputs = walk::read_inputs(plan, &limits);
    let fingerprints = cache::Fingerprints::of(&inputs);
    let lookup = if refresh {
        cache::Lookup::Bypassed
    } else {
        cache::load(root, &limits, &fingerprints)
    };
    match lookup {
        cache::Lookup::Hit(discovery, report) => Outcome {
            discovery: *discovery,
            cache: report,
        },
        cache::Lookup::Miss(reason) => {
            let discovery = build(inputs, &limits);
            let report = cache::store(root, &limits, &fingerprints, &discovery, Some(reason));
            Outcome {
                discovery,
                cache: report,
            }
        }
        cache::Lookup::Bypassed => {
            let discovery = build(inputs, &limits);
            let report = cache::store(root, &limits, &fingerprints, &discovery, None);
            Outcome {
                discovery,
                cache: report,
            }
        }
    }
}

/// Reports when `root` lies inside a directory that declares a monorepo, so the setup skill
/// can ask whether the parent should be the workspace. Checks ancestors up to (not including)
/// HOME or `/`; reads at most one `package.json` per ancestor, bounded by the file limit.
pub fn parent_monorepo(root: &Path) -> Option<Ambiguity> {
    let home = std::env::var_os("HOME").and_then(|h| std::fs::canonicalize(h).ok());
    for dir in root.ancestors().skip(1) {
        if dir == Path::new("/") || home.as_deref() == Some(dir) {
            break;
        }
        if let Some(marker) = js::monorepo_marker(dir, Limits::DEFAULT.max_file_bytes) {
            let path = dir.join(marker);
            return Some(Ambiguity {
                kind: AmbiguityKind::ParentMonorepo,
                message: format!(
                    "the selected root is inside a monorepo declared at {}; subpackages belong to that root unless selected with --project",
                    dir.display()
                ),
                sources: vec![path.to_string_lossy().into_owned()],
            });
        }
    }
    None
}

/// Collects facts while enforcing [`Limits::max_facts`] and dropping exact repeats
/// (same kind, value, owner, and file), e.g. a variable declared twice in one env example.
pub(crate) struct Facts {
    items: Vec<Fact>,
    /// The fact added last; `with` may still set its owner and detail.
    pending: Option<Fact>,
    seen: std::collections::HashSet<(FactKind, String, Option<String>, String)>,
    max: usize,
    dropped: u64,
    dropped_from: std::collections::BTreeSet<String>,
}

impl Facts {
    fn new(max: usize) -> Self {
        Self {
            items: Vec::new(),
            pending: None,
            seen: std::collections::HashSet::new(),
            max,
            dropped: 0,
            dropped_from: std::collections::BTreeSet::new(),
        }
    }

    fn flush(&mut self) {
        let Some(mut fact) = self.pending.take() else {
            return;
        };
        text::clip(&mut fact.value);
        if let Some(d) = fact.detail.as_mut() {
            text::clip(d);
        }
        if let Some(o) = fact.owner.as_mut() {
            text::clip(o);
        }
        let key = (
            fact.kind,
            fact.value.clone(),
            fact.owner.clone(),
            fact.source.path.clone(),
        );
        if self.seen.contains(&key) {
            return;
        }
        if self.items.len() >= self.max {
            self.dropped += 1;
            self.dropped_from.insert(fact.source.path);
            return;
        }
        self.seen.insert(key);
        self.items.push(fact);
    }

    pub(crate) fn add(
        &mut self,
        kind: FactKind,
        value: impl Into<String>,
        path: &str,
        lines: Option<Lines>,
        certainty: Certainty,
    ) -> &mut Self {
        self.flush();
        self.pending = Some(Fact {
            kind,
            value: value.into(),
            owner: None,
            detail: None,
            source: Source {
                path: path.to_owned(),
                lines,
            },
            certainty,
        });
        self
    }

    /// Sets `owner` and `detail` on the fact added last.
    pub(crate) fn with(&mut self, owner: Option<&str>, detail: Option<&str>) {
        if let Some(f) = self.pending.as_mut() {
            f.owner = owner.map(str::to_owned);
            f.detail = detail.map(str::to_owned);
        }
    }

    fn has(&self, kind: FactKind, dir: &str) -> Option<&str> {
        self.items
            .iter()
            .find(|f| f.kind == kind && text::parent(&f.source.path) == dir)
            .map(|f| f.source.path.as_str())
    }
}

fn build(inputs: walk::Inputs, limits: &Limits) -> Discovery {
    let walk::Inputs {
        files,
        mut truncated,
        stats,
    } = inputs;
    let mut facts = Facts::new(limits.max_facts);
    let mut parse_failed = Vec::new();
    // Command and manifest files first, so bulky env examples or docs cannot push them out
    // of the fact limit; shallower files first within each rank.
    let mut order: Vec<&walk::InputFile> = files.iter().collect();
    order.sort_by_key(|f| (f.class.rank(), f.rel.matches('/').count()));
    for file in order {
        let ok = match &file.content {
            Some(content) => parse_file(file, content, &mut facts),
            None => {
                presence_fact(file, &mut facts);
                true
            }
        };
        if !ok {
            parse_failed.push(file.rel.clone());
        }
    }
    facts.flush();
    if !parse_failed.is_empty() {
        truncated.push(Truncation {
            reason: TruncationReason::ParseFailed,
            limit: None,
            omitted_count: parse_failed.len() as u64,
            omitted: parse_failed.into_iter().take(MAX_OMITTED_LISTED).collect(),
        });
    }
    let ambiguities = ambiguities(&files, &facts);
    if facts.dropped > 0 {
        truncated.push(Truncation {
            reason: TruncationReason::MaxFacts,
            limit: Some(limits.max_facts as u64),
            // Files whose facts were partly or fully dropped.
            omitted: facts
                .dropped_from
                .iter()
                .take(MAX_OMITTED_LISTED)
                .cloned()
                .collect(),
            omitted_count: facts.dropped,
        });
    }
    let mut items = facts.items;
    items.sort_by(|a, b| {
        (&a.source.path, a.source.lines.map(|l| l.start), a.kind).cmp(&(
            &b.source.path,
            b.source.lines.map(|l| l.start),
            b.kind,
        ))
    });
    Discovery {
        facts: items,
        ambiguities,
        truncated,
        scan: stats,
    }
}

/// Parses one read file. Returns `false` when the file could not be parsed.
fn parse_file(file: &walk::InputFile, content: &str, facts: &mut Facts) -> bool {
    use walk::Class;
    let rel = file.rel.as_str();
    match file.class {
        Class::PackageJson => js::package_json(rel, content, facts),
        Class::PnpmWorkspace => compose::pnpm_workspace(rel, content, facts),
        Class::Pyproject => python::pyproject(rel, content, facts),
        Class::Compose => compose::compose(rel, content, facts),
        Class::Taskfile => compose::taskfile(rel, content, facts),
        Class::Makefile => {
            tasks::makefile(rel, content, facts);
            true
        }
        Class::Justfile => {
            tasks::justfile(rel, content, facts);
            true
        }
        Class::EnvExample => {
            env::env_example(rel, content, facts);
            true
        }
        Class::RuntimeVersion => {
            env::runtime_version(rel, content, facts);
            true
        }
        _ => {
            presence_fact(file, facts);
            true
        }
    }
}

fn presence_fact(file: &walk::InputFile, facts: &mut Facts) {
    use walk::Class;
    let rel = file.rel.as_str();
    let (kind, owner) = match file.class {
        Class::Lockfile(manager, _) => (FactKind::Lockfile, Some(manager)),
        Class::MonorepoTool => (FactKind::MonorepoTool, None),
        Class::Requirements => (FactKind::RequirementsFile, None),
        Class::PythonProjectFile => (FactKind::PythonProjectFile, None),
        Class::EnvFile => (FactKind::EnvFile, None),
        Class::Doc => (FactKind::Doc, None),
        Class::DocDir => (FactKind::DocDir, None),
        Class::ProjectFile => (FactKind::ProjectFile, None),
        // Files that should have been read but were omitted by a limit carry no fact.
        _ => return,
    };
    facts
        .add(kind, rel, rel, None, Certainty::Hint)
        .with(owner, None);
}

fn ambiguities(files: &[walk::InputFile], facts: &Facts) -> Vec<Ambiguity> {
    use std::collections::BTreeMap;
    use walk::Class;
    let mut out = Vec::new();

    // Lockfiles per (directory, ecosystem).
    let mut locks: BTreeMap<(String, &str), Vec<(&str, &str)>> = BTreeMap::new();
    for f in files {
        if let Class::Lockfile(manager, ecosystem) = f.class {
            locks
                .entry((text::parent(&f.rel).to_owned(), ecosystem))
                .or_default()
                .push((manager, f.rel.as_str()));
        }
    }
    for ((dir, ecosystem), list) in &locks {
        let mut managers: Vec<&str> = list.iter().map(|(m, _)| *m).collect();
        managers.sort_unstable();
        managers.dedup();
        if managers.len() > 1 {
            out.push(Ambiguity {
                kind: AmbiguityKind::MultipleLockfiles,
                message: format!(
                    "{} has {ecosystem} lockfiles for {}; the package manager in use is unclear",
                    text::shown_dir(dir),
                    managers.join(", ")
                ),
                sources: list.iter().map(|(_, p)| (*p).to_owned()).collect(),
            });
        }
        if *ecosystem == "javascript"
            && let Some(declared) = facts
                .items
                .iter()
                .find(|f| f.kind == FactKind::PackageManager && text::parent(&f.source.path) == dir)
        {
            let name = declared.value.split('@').next().unwrap_or_default();
            if !managers.contains(&name) {
                let mut sources = vec![declared.source.path.clone()];
                sources.extend(list.iter().map(|(_, p)| (*p).to_owned()));
                out.push(Ambiguity {
                    kind: AmbiguityKind::PackageManagerMismatch,
                    message: format!(
                        "packageManager declares `{}` but the lockfile belongs to {}",
                        declared.value,
                        managers.join(", ")
                    ),
                    sources,
                });
            }
        }
    }

    // Several base Compose files in one directory.
    let mut compose: BTreeMap<String, Vec<&str>> = BTreeMap::new();
    for f in files {
        if f.class == Class::Compose && walk::is_base_compose(text::file_name(&f.rel)) {
            compose
                .entry(text::parent(&f.rel).to_owned())
                .or_default()
                .push(f.rel.as_str());
        }
    }
    for (dir, list) in compose {
        if list.len() > 1 {
            out.push(Ambiguity {
                kind: AmbiguityKind::MultipleComposeFiles,
                message: format!(
                    "{} has {} base Compose files; which one is used is unclear",
                    text::shown_dir(&dir),
                    list.len()
                ),
                sources: list.into_iter().map(str::to_owned).collect(),
            });
        }
    }

    // Commands declared in several places at the root.
    let sources: Vec<String> = [
        FactKind::PackageScript,
        FactKind::MakeTarget,
        FactKind::JustRecipe,
        FactKind::TaskfileTask,
        FactKind::ComposeService,
    ]
    .iter()
    .filter_map(|k| facts.has(*k, ""))
    .map(str::to_owned)
    .collect();
    if sources.len() > 1 {
        out.push(Ambiguity {
            kind: AmbiguityKind::MultipleCommandSources,
            message: format!(
                "the root declares commands in {} files; the scanner does not choose a start flow",
                sources.len()
            ),
            sources,
        });
    }
    out
}
