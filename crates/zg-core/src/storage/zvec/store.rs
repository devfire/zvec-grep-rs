//! JSON-backed file metadata store.
//!
//! Replaces the TypeScript `ZvecFileMetaStore` (a second zvec collection
//! holding one document per file) with an atomic JSON map of file id to
//! [`FileRecord`]. Reads are served from memory; mutations stage in memory
//! and persist via [`flush`](FileMetaStore::flush), which writes the whole
//! map through [`crate::utils::json_io`] only when dirty.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

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
    #[must_use]
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
    dirty: bool,
}

impl FileMetaStore {
    /// Loads records from `path`; a missing file starts empty unless
    /// `read_only`, which mirrors the TypeScript missing-store error.
    /// A corrupt file is quarantined to `<path>.corrupt.<unix-seconds>`
    /// (skipped in `read_only` mode) and reported with
    /// `STORAGE.FILE_META_CORRUPT` carrying the path and quarantine
    /// location, so one bad byte never bricks the index.
    ///
    /// # Errors
    ///
    /// Returns `STORAGE.ZVEC_FILE_META_MISSING` when a read-only store file is absent,
    /// `STORAGE.FILE_META_CORRUPT` when persisted records fail to parse, or a JSON
    /// I/O error when persisted records cannot be read.
    pub fn open(path: &Path, read_only: bool) -> EngineResult<Self> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if read_only {
                    return Err(EngineError::new(
                        EngineErrorCode::StorageZvecFileMetaMissing,
                        "zvec file metadata storage does not exist",
                    )
                    .with_context(format!("path={}", path.display())));
                }
                return Ok(Self {
                    path: path.to_path_buf(),
                    read_only,
                    records: HashMap::new(),
                    dirty: false,
                });
            }
            Err(error) => {
                return Err(EngineError::new(
                    EngineErrorCode::JsonReadFailed,
                    format!("failed to read {}", path.display()),
                )
                .with_context(format!("error={error}"))
                .with_source(error));
            }
        };
        match serde_json::from_str::<HashMap<String, FileRecord>>(&text) {
            Ok(records) => Ok(Self {
                path: path.to_path_buf(),
                read_only,
                records,
                dirty: false,
            }),
            Err(error) => Err(quarantine_corrupt_file_meta(path, read_only, error)),
        }
    }

    #[must_use]
    pub fn get(&self, file_id: &str) -> Option<&FileRecord> {
        self.records.get(file_id)
    }

    /// Borrowed record map for hot paths that must not clone the index.
    #[must_use]
    pub fn records(&self) -> &HashMap<String, FileRecord> {
        &self.records
    }

    /// Borrowed records sorted by relative path, mirroring [`list`](Self::list) without cloning.
    #[must_use]
    pub fn sorted_records(&self) -> Vec<&FileRecord> {
        let mut records: Vec<&FileRecord> = self.records.values().collect();
        records.sort_by(|left, right| left.info.relative_path.cmp(&right.info.relative_path));
        records
    }

    /// Records sorted by relative path, mirroring the TypeScript listing.
    #[must_use]
    pub fn list(&self) -> Vec<FileRecord> {
        let mut records: Vec<FileRecord> = self.records.values().cloned().collect();
        records.sort_by(|left, right| left.info.relative_path.cmp(&right.info.relative_path));
        records
    }

    /// Stages `record` in memory, marking the store dirty. Call [`flush`](Self::flush)
    /// at a metadata checkpoint to persist.
    ///
    /// # Errors
    ///
    /// Returns `STORAGE.FILE_META_READ_ONLY` when the store is read-only.
    pub fn upsert(&mut self, record: FileRecord) -> EngineResult<()> {
        self.assert_writable("upsertFile")?;
        self.records
            .insert(record.info.id.as_str().to_owned(), record);
        self.dirty = true;
        Ok(())
    }

    /// Stages removal of `file_id` in memory (no-op when absent), marking the
    /// store dirty. Call [`flush`](Self::flush) at a metadata checkpoint to persist.
    ///
    /// # Errors
    ///
    /// Returns `STORAGE.FILE_META_READ_ONLY` when the store is read-only.
    pub fn remove(&mut self, file_id: &str) -> EngineResult<()> {
        self.assert_writable("deleteFile")?;
        self.records.remove(file_id);
        self.dirty = true;
        Ok(())
    }

    /// Whether staged mutations await persistence.
    #[must_use]
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Persists staged mutations when dirty, then clears the flag. No-op when
    /// clean. Failures keep the flag so a later checkpoint retries.
    ///
    /// # Errors
    ///
    /// Returns a JSON I/O error when persistence fails.
    pub fn flush(&mut self) -> EngineResult<()> {
        if !self.dirty {
            return Ok(());
        }
        self.persist()?;
        self.dirty = false;
        Ok(())
    }

    fn persist(&self) -> EngineResult<()> {
        json_io::write_json_file(&self.path, &self.records, DEFAULT_MODES)
    }

    fn assert_writable(&self, operation: &str) -> EngineResult<()> {
        if self.read_only {
            return Err(EngineError::new(
                EngineErrorCode::StorageFileMetaReadOnly,
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

/// Reports a corrupt file-metadata document, quarantining the original to
/// `<path>.corrupt.<unix-seconds>` so the next open starts clean instead of
/// re-reading the same bad byte. Quarantine is skipped in `read_only` mode
/// (a read must not mutate the store); the error context always carries the
/// source path plus the quarantine location or the reason it was skipped.
fn quarantine_corrupt_file_meta(
    path: &Path,
    read_only: bool,
    error: serde_json::Error,
) -> EngineError {
    if read_only {
        return EngineError::new(
            EngineErrorCode::StorageFileMetaCorrupt,
            "zvec file metadata is corrupt",
        )
        .with_context(format!(
            "path={} quarantine=skipped(read-only) error={error}",
            path.display(),
        ))
        .with_source(error);
    }
    let quarantine = corrupt_quarantine_path(path);
    match fs::rename(path, &quarantine) {
        Ok(()) => EngineError::new(
            EngineErrorCode::StorageFileMetaCorrupt,
            "zvec file metadata is corrupt; quarantined",
        )
        .with_context(format!(
            "path={} quarantine={} error={error}",
            path.display(),
            quarantine.display(),
        ))
        .with_source(error),
        Err(rename_error) => EngineError::new(
            EngineErrorCode::StorageFileMetaCorrupt,
            "zvec file metadata is corrupt; quarantine failed",
        )
        .with_context(format!(
            "path={} quarantine={} error={error} quarantineError={rename_error}",
            path.display(),
            quarantine.display(),
        ))
        .with_source(error),
    }
}

/// Sibling of `path` with `.corrupt.<unix-seconds>` appended, e.g.
/// `files.json` becomes `files.json.corrupt.1758048000`. Falls back to
/// epoch zero when the clock is unavailable.
fn corrupt_quarantine_path(path: &Path) -> PathBuf {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default();
    let mut quarantine = path.as_os_str().to_os_string();
    quarantine.push(format!(".corrupt.{timestamp}"));
    PathBuf::from(quarantine)
}
