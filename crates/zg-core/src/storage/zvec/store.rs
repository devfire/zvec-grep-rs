//! JSON-backed file metadata store.
//!
//! Replaces the TypeScript `ZvecFileMetaStore` (a second zvec collection
//! holding one document per file) with an atomic JSON map of file id to
//! [`FileRecord`]. Reads are served from memory; every mutation persists
//! the whole map via [`crate::utils::json_io`].

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{EngineError, EngineErrorCode, EngineResult};
use crate::types::FileInfo;
use crate::utils::json_io::{self, DEFAULT_MODES};

/// A [`FileInfo`] plus the public entity ids indexed for it, mirroring the
/// TypeScript `FileRecord` (`FileInfo & { entityIds }`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileRecord {
    #[serde(flatten)]
    pub info: FileInfo,
    pub entity_ids: Vec<String>,
}

impl FileRecord {
    pub fn file_info(&self) -> FileInfo {
        self.info.clone()
    }
}

/// In-memory file records with atomic JSON persistence.
#[derive(Debug)]
pub struct FileMetaStore {
    path: PathBuf,
    read_only: bool,
    records: HashMap<String, FileRecord>,
}

impl FileMetaStore {
    /// Loads records from `path`; a missing file starts empty unless
    /// `read_only`, which mirrors the TypeScript missing-store error.
    pub fn open(path: &Path, read_only: bool) -> EngineResult<Self> {
        if read_only && !path.exists() {
            return Err(EngineError::new(
                EngineErrorCode::from_static("STORAGE.ZVEC_FILE_META_MISSING"),
                "zvec file metadata storage does not exist",
            )
            .with_context(format!("path={}", path.display())));
        }
        let records: HashMap<String, FileRecord> = json_io::read_json_file(path, HashMap::new())?;
        Ok(Self {
            path: path.to_path_buf(),
            read_only,
            records,
        })
    }

    /// Builds a store from already-loaded records without touching disk.
    /// Used by the TS-generation importer ([`super::legacy_import`]),
    /// which sources records from `files.zvec` rather than `files.json`;
    /// the caller persists explicitly via [`persist`](Self::persist) only
    /// for verified imports, so a partial import never cements itself as
    /// `files.json`.
    pub fn from_records(path: &Path, read_only: bool, records: Vec<FileRecord>) -> Self {
        Self {
            path: path.to_path_buf(),
            read_only,
            records: records
                .into_iter()
                .map(|record| (record.info.id.as_str().to_owned(), record))
                .collect(),
        }
    }

    pub fn get(&self, file_id: &str) -> Option<&FileRecord> {
        self.records.get(file_id)
    }

    /// Records sorted by relative path, mirroring the TypeScript listing.
    pub fn list(&self) -> Vec<FileRecord> {
        let mut records: Vec<FileRecord> = self.records.values().cloned().collect();
        records.sort_by(|left, right| left.info.relative_path.cmp(&right.info.relative_path));
        records
    }

    pub fn upsert(&mut self, record: FileRecord) -> EngineResult<()> {
        self.assert_writable("upsertFile")?;
        self.records
            .insert(record.info.id.as_str().to_owned(), record);
        self.persist()
    }

    pub fn remove(&mut self, file_id: &str) -> EngineResult<()> {
        self.assert_writable("deleteFile")?;
        self.records.remove(file_id);
        self.persist()
    }

    /// Persists the whole record map atomically. Called by mutation paths
    /// and, once, by the storage opener after a verified legacy import.
    pub(crate) fn persist(&self) -> EngineResult<()> {
        json_io::write_json_file(&self.path, &self.records, DEFAULT_MODES)
    }

    fn assert_writable(&self, operation: &str) -> EngineResult<()> {
        if self.read_only {
            return Err(EngineError::new(
                EngineErrorCode::from_static("STORAGE.FILE_META_READ_ONLY"),
                "cannot update read-only file metadata storage",
            )
            .with_context(format!(
                "path={} operation={operation}",
                self.path.display()
            )));
        }
        Ok(())
    }
}
