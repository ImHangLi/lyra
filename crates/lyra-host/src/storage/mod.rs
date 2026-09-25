//! Workspace ledger (§14.2, §15): one SQLite connection owned by one dedicated thread.
//!
//! The actor talks to it only through [`Storage`], a cloneable handle that sends typed jobs
//! over a bounded channel and awaits their results. No other code opens the database.

use lyra_protocol::error::{ErrorCode, ErrorInfo};
use lyra_protocol::ids::{ActionRef, Digest, RequestKey, RunId, ViewRef, ViewRevision};
use lyra_protocol::manifest::ViewKind;
use lyra_protocol::run::Outcome;
use lyra_protocol::time::Timestamp;
use lyra_protocol::view::SourceKind;

/// Current schema version written by this binary.
pub const SCHEMA_VERSION: u32 = 1;
/// View revisions are reserved in blocks of this size (§15.3).
pub const VIEW_REVISION_BLOCK: u64 = 1024;

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum StorageError {
    /// A required write or read could not complete (disk full, permissions, IO).
    #[error("storage unavailable: {0}")]
    Unavailable(String),
    /// The database was written by a newer binary.
    #[error("state database schema {found} is newer than supported {supported}")]
    VersionUnsupported { found: u32, supported: u32 },
    /// The database file exists but cannot be used; it is never replaced by an empty one.
    #[error("state database is damaged: {0}")]
    Corrupt(String),
    /// The workspace root recorded in the database differs (hash-prefix collision).
    #[error("state database belongs to another workspace root: {0}")]
    WorkspaceCollision(String),
    #[error("view revision counter exhausted")]
    CounterExhausted,
}

impl StorageError {
    pub fn to_error_info(&self) -> ErrorInfo {
        let code = match self {
            Self::Unavailable(_) | Self::Corrupt(_) => ErrorCode::STORAGE_UNAVAILABLE,
            Self::VersionUnsupported { .. } => ErrorCode::STORAGE_VERSION_UNSUPPORTED,
            Self::WorkspaceCollision(_) => ErrorCode::WORKSPACE_ID_COLLISION,
            Self::CounterExhausted => ErrorCode::COUNTER_EXHAUSTED,
        };
        ErrorInfo::new(code, self.to_string())
    }
}

/// Idempotency scope for a request key (§11.5): per action, per view, or per operation kind.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum KeyScope {
    Action(ActionRef),
    Exec,
    Publish(ViewRef),
    Apply,
}

impl KeyScope {
    pub fn as_key(&self) -> String {
        match self {
            Self::Action(a) => format!("action:{a}"),
            Self::Exec => "exec".into(),
            Self::Publish(v) => format!("publish:{v}"),
            Self::Apply => "apply".into(),
        }
    }
}

/// A request key plus the HMAC fingerprint of `{effective_input, definition_hash}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyClaim {
    pub scope: KeyScope,
    pub key: RequestKey,
    pub fingerprint: Digest,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Claim {
    /// The key was new and is now recorded for `run_id` (or publish revision).
    New,
    /// The key exists with the same fingerprint: return the original result.
    Same { reference: String },
    /// The key exists with a different fingerprint: REQUEST_KEY_CONFLICT.
    Conflict,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct RunFilter {
    pub action_ref: Option<ActionRef>,
    pub outcome: Option<Outcome>,
    /// Keyset pagination: only runs strictly older than this (started_at, run_id).
    pub before: Option<(Timestamp, RunId)>,
    pub limit: usize,
}

/// A stored view body. `data_json` is canonical ViewData JSON; decode it again on read.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredView {
    pub view_ref: ViewRef,
    pub revision: ViewRevision,
    pub kind: ViewKind,
    pub recorded_at: Timestamp,
    pub source_run_id: Option<RunId>,
    pub source_kind: SourceKind,
    pub definition_hash: Digest,
    pub data_json: String,
}

/// What `open` found.
#[derive(Debug, Clone, PartialEq)]
pub struct OpenReport {
    pub created: bool,
    pub sqlite_version: String,
    /// Runs that were active when the previous host stopped unexpectedly; now `interrupted`.
    pub interrupted: Vec<RunId>,
}

mod db;
mod schema;
mod thread;

pub use thread::Storage;

// Operations provided by [`Storage`]. Every method is async and fails with [`StorageError`].
//
// - `open(paths) -> Result<(Storage, OpenReport)>` (sync): creates dirs 0700 / files 0600,
//   runs migrations, applies PRAGMAs, checks root and version, loads `fingerprint.key`, and
//   marks previously active runs `interrupted` with a note.
// - `fingerprint(&Value) -> Digest` (sync, no IO): HMAC-SHA-256 over the JCS form.
// - `claim_key(KeyClaim, reference)` → [`Claim`]; commits before returning.
// - `insert_run(RunRecord)`: the reservation; commits before the caller spawns anything.
// - `save_run(RunRecord)`: upsert of lifecycle/result changes; commits.
// - `get_run(RunId) -> Option<RunRecord>`, `list_runs(RunFilter) -> Vec<RunRecord>` (newest first).
// - `catalog() -> (CatalogRevision, Option<Digest>)`; `accept_catalog(Digest) -> CatalogRevision`
//   bumps only when the set hash differs.
// - `reserve_view_block() -> (first, end_exclusive)`: persisted before use.
// - `save_view(StoredView)`, `load_view(ViewRef) -> Option<StoredView>`.
// - `status() -> StorageStatusData`.
