//! `<root>/.lyra/.generated/discovery.json` (§14.1): the last result, the fingerprints of
//! every file it read, and the scan parameters. A cache entry is reused only when all of
//! them match and it is younger than seven days. Modification times are not used.

use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use lyra_protocol::{Digest, Timestamp};
use serde::{Deserialize, Serialize};

use crate::walk::Inputs;
use crate::{DISCOVERY_FORMAT, Discovery, Limits, Truncation};

const CACHE_TTL_MS: i64 = 7 * 24 * 60 * 60 * 1000;
const MAX_CACHE_BYTES: u64 = 8 * 1024 * 1024;

pub fn cache_path(root: &Path) -> PathBuf {
    root.join(".lyra").join(".generated").join("discovery.json")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheStatus {
    Hit,
    Miss,
    /// `--refresh` bypassed the cache.
    Refreshed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MissReason {
    Absent,
    Unreadable,
    Invalid,
    ParamsChanged,
    RootChanged,
    Expired,
    SourcesChanged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheWrite {
    /// The cache was reused as is.
    NotNeeded,
    Written,
    /// `.lyra` or `.lyra/.generated` exists but is not a plain directory; nothing is written.
    SkippedNotDirectory,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CacheReport {
    pub path: String,
    pub status: CacheStatus,
    pub miss_reason: Option<MissReason>,
    pub write: CacheWrite,
    /// When the reported discovery result was produced.
    pub created_at: Timestamp,
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileFingerprint {
    path: String,
    size: u64,
    sha256: Digest,
}

/// Everything the result depends on: read file contents, the set of classified paths, and
/// the omitted scope found while walking and reading.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fingerprints {
    read: Vec<FileFingerprint>,
    present: Vec<String>,
    omitted: Vec<Truncation>,
}

impl Fingerprints {
    pub fn of(inputs: &Inputs) -> Self {
        Self {
            read: inputs
                .files
                .iter()
                .filter_map(|f| {
                    f.fingerprint.as_ref().map(|(size, sha)| FileFingerprint {
                        path: f.rel.clone(),
                        size: *size,
                        sha256: sha.clone(),
                    })
                })
                .collect(),
            present: inputs.files.iter().map(|f| f.rel.clone()).collect(),
            omitted: inputs.truncated.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Params {
    format: u32,
    scanner_version: String,
    limits: Limits,
}

impl Params {
    fn current(limits: &Limits) -> Self {
        Self {
            format: DISCOVERY_FORMAT,
            scanner_version: env!("CARGO_PKG_VERSION").to_owned(),
            limits: *limits,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CacheFile {
    params: Params,
    root: String,
    created_at: Timestamp,
    fingerprints: Fingerprints,
    discovery: Discovery,
}

pub enum Lookup {
    Hit(Box<Discovery>, CacheReport),
    Miss(MissReason),
    Bypassed,
}

fn shown(root: &Path) -> String {
    cache_path(root).to_string_lossy().into_owned()
}

fn read_cache(path: &Path) -> Result<CacheFile, MissReason> {
    let meta = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(MissReason::Absent),
        Err(_) => return Err(MissReason::Unreadable),
    };
    if !meta.is_file() || meta.len() > MAX_CACHE_BYTES {
        return Err(MissReason::Unreadable);
    }
    let mut bytes = Vec::new();
    fs::File::open(path)
        .and_then(|f| f.take(MAX_CACHE_BYTES).read_to_end(&mut bytes))
        .map_err(|_| MissReason::Unreadable)?;
    serde_json::from_slice(&bytes).map_err(|_| MissReason::Invalid)
}

pub fn load(root: &Path, limits: &Limits, fingerprints: &Fingerprints) -> Lookup {
    let cache = match read_cache(&cache_path(root)) {
        Ok(c) => c,
        Err(reason) => return Lookup::Miss(reason),
    };
    if cache.params != Params::current(limits) {
        return Lookup::Miss(MissReason::ParamsChanged);
    }
    if cache.root != root.to_string_lossy() {
        return Lookup::Miss(MissReason::RootChanged);
    }
    let age = Timestamp::now().unix_ms() - cache.created_at.unix_ms();
    if !(0..CACHE_TTL_MS).contains(&age) {
        return Lookup::Miss(MissReason::Expired);
    }
    if &cache.fingerprints != fingerprints {
        return Lookup::Miss(MissReason::SourcesChanged);
    }
    let report = CacheReport {
        path: shown(root),
        status: CacheStatus::Hit,
        miss_reason: None,
        write: CacheWrite::NotNeeded,
        created_at: cache.created_at,
        note: None,
    };
    Lookup::Hit(Box::new(cache.discovery), report)
}

/// Creates `dir` (mode 0700) when absent. Returns whether it was created.
fn ensure_dir(dir: &Path) -> Result<bool, (CacheWrite, String)> {
    match fs::symlink_metadata(dir) {
        Ok(m) if m.is_dir() => Ok(false),
        Ok(_) => Err((
            CacheWrite::SkippedNotDirectory,
            format!("{} is not a directory", dir.display()),
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => fs::DirBuilder::new()
            .mode(0o700)
            .create(dir)
            .map(|()| true)
            .map_err(|e| {
                (
                    CacheWrite::Failed,
                    format!("cannot create {}: {e}", dir.display()),
                )
            }),
        Err(e) => Err((
            CacheWrite::Failed,
            format!("cannot inspect {}: {e}", dir.display()),
        )),
    }
}

fn write_new(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(bytes)?;
    f.sync_all()
}

fn write_cache(root: &Path, file: &CacheFile) -> Result<(), (CacheWrite, String)> {
    let lyra = root.join(".lyra");
    let generated = lyra.join(".generated");
    ensure_dir(&lyra)?;
    if ensure_dir(&generated)? {
        // Keeps the generated directory out of Git without editing the project's ignore files.
        let _ = write_new(&generated.join(".gitignore"), b"*\n");
    }
    let bytes = serde_json::to_vec(file).map_err(|e| (CacheWrite::Failed, e.to_string()))?;
    let tmp = generated.join(format!(".discovery.json.{}.tmp", std::process::id()));
    let _ = fs::remove_file(&tmp);
    let result = write_new(&tmp, &bytes).and_then(|()| fs::rename(&tmp, cache_path(root)));
    result.map_err(|e| {
        let _ = fs::remove_file(&tmp);
        (
            CacheWrite::Failed,
            format!("cannot write the discovery cache: {e}"),
        )
    })
}

pub fn store(
    root: &Path,
    limits: &Limits,
    fingerprints: &Fingerprints,
    discovery: &Discovery,
    miss: Option<MissReason>,
) -> CacheReport {
    let file = CacheFile {
        params: Params::current(limits),
        root: root.to_string_lossy().into_owned(),
        created_at: Timestamp::now(),
        fingerprints: fingerprints.clone(),
        discovery: discovery.clone(),
    };
    let (write, note) = match write_cache(root, &file) {
        Ok(()) => (CacheWrite::Written, None),
        Err((w, note)) => (w, Some(note)),
    };
    CacheReport {
        path: shown(root),
        status: if miss.is_some() {
            CacheStatus::Miss
        } else {
            CacheStatus::Refreshed
        },
        miss_reason: miss,
        write,
        created_at: file.created_at,
        note,
    }
}
