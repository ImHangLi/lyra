//! Storage thread and handle. Interface only; the SQLite implementation replaces these bodies.

use lyra_protocol::ids::{CatalogRevision, Digest, RunId, ViewRef};
use lyra_protocol::ipc::StorageStatusData;
use lyra_protocol::paths::WorkspacePaths;
use lyra_protocol::run::RunRecord;
use serde_json::Value;

use super::{Claim, KeyClaim, OpenReport, RunFilter, StorageError, StoredView};

#[derive(Clone)]
pub struct Storage {
    _private: (),
}

fn todo_err<T>() -> Result<T, StorageError> {
    Err(StorageError::Unavailable("storage is not implemented yet".into()))
}

impl Storage {
    pub fn open(_paths: &WorkspacePaths) -> Result<(Storage, OpenReport), StorageError> {
        todo_err()
    }
    pub fn fingerprint(&self, value: &Value) -> Digest {
        lyra_protocol::hash::canonical_digest(value).unwrap_or_else(|_| Digest::of_bytes(b""))
    }
    pub async fn claim_key(&self, _claim: KeyClaim, _reference: String) -> Result<Claim, StorageError> {
        todo_err()
    }
    pub async fn insert_run(&self, _run: RunRecord) -> Result<(), StorageError> {
        todo_err()
    }
    pub async fn save_run(&self, _run: RunRecord) -> Result<(), StorageError> {
        todo_err()
    }
    pub async fn get_run(&self, _id: RunId) -> Result<Option<RunRecord>, StorageError> {
        todo_err()
    }
    pub async fn list_runs(&self, _filter: RunFilter) -> Result<Vec<RunRecord>, StorageError> {
        todo_err()
    }
    pub async fn catalog(&self) -> Result<(CatalogRevision, Option<Digest>), StorageError> {
        todo_err()
    }
    pub async fn accept_catalog(&self, _set_hash: Digest) -> Result<CatalogRevision, StorageError> {
        todo_err()
    }
    pub async fn reserve_view_block(&self) -> Result<(u64, u64), StorageError> {
        todo_err()
    }
    pub async fn save_view(&self, _view: StoredView) -> Result<(), StorageError> {
        todo_err()
    }
    pub async fn load_view(&self, _view: ViewRef) -> Result<Option<StoredView>, StorageError> {
        todo_err()
    }
    pub async fn status(&self) -> Result<StorageStatusData, StorageError> {
        todo_err()
    }
}
