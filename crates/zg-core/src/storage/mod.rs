//! Workspace index storage abstraction over zvec collections.

pub mod layout;
pub mod zvec;

use std::path::Path;

use crate::error::EngineResult;
use crate::ids::{EntityId, FileId};
use crate::types::{CodeSymbolType, EntityFragment, FileInfo, WorkspaceIndexEmbeddingSchema};

/// Options for opening workspace index storage.
#[derive(Debug, Clone)]
pub enum StorageOptions<'a> {
    ReadOnly {
        storage_path: &'a Path,
    },
    ReadWrite {
        storage_path: &'a Path,
        embedding: &'a WorkspaceIndexEmbeddingSchema,
    },
}

impl StorageOptions<'_> {
    #[must_use]
    pub fn storage_path(&self) -> &Path {
        match self {
            Self::ReadOnly { storage_path } | Self::ReadWrite { storage_path, .. } => storage_path,
        }
    }

    #[must_use]
    pub fn read_only(&self) -> bool {
        matches!(self, Self::ReadOnly { .. })
    }
}

/// An entity joined with its owning file, as returned by storage reads.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredEntity {
    pub entity: crate::types::Entity,
    pub file: FileInfo,
}

/// A fragment plus its embedding vector, ready to upsert.
#[derive(Debug, Clone, PartialEq)]
pub struct IndexedFragment {
    pub fragment: EntityFragment,
    pub vector: Vec<f32>,
}

/// Per-file diagnostics recorded with an indexed file.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FileIndexDiagnostics {
    pub truncated_fragment_count: Option<usize>,
}

/// Pagination for entity listing.
#[derive(Debug, Clone, Copy, Default)]
pub struct ListEntitiesOptions {
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}

/// Filters applied during FTS/vector recall.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StorageSearchFilter {
    pub file_ids: Vec<FileId>,
    pub group_ids: Vec<String>,
    pub symbol_names: Vec<String>,
    pub symbol_types: Vec<CodeSymbolType>,
}

/// Which recall path produced a hit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageSearchPath {
    Fts,
    Vector,
}

/// A raw storage-level search hit before fusion.
#[derive(Debug, Clone, PartialEq)]
pub struct StorageSearchHit {
    pub fragment: EntityFragment,
    pub file: FileInfo,
    pub path: StorageSearchPath,
    pub score: f64,
}

/// Sealed: `ZvecWorkspaceIndexStorage` is the only implementor, so adding
/// methods is not a breaking change. (Plain `mod private`: the implementor
/// is a child module of this file.)
mod private {
    pub trait Sealed {}
}

/// Storage surface consumed by the indexing and search pipelines.
pub trait WorkspaceIndexStorage: private::Sealed + Send {
    fn read_only(&self) -> bool;

    fn get_file_by_path(&self, absolute_path: &str) -> Option<FileInfo>;
    fn list_files_by_path_prefix(&self, absolute_path: &str) -> Vec<FileInfo>;
    fn list_files_by_path_prefixes(&self, absolute_paths: &[String]) -> Vec<FileInfo>;
    fn list_files(&self) -> Vec<FileInfo>;

    /// Borrowed file metadata in relative-path order, for hot paths that must
    /// not clone the whole index (mirrors [`list_files`] without cloning).
    fn list_file_refs(&self) -> Vec<&FileInfo>;

    fn list_entities_by_file(
        &self,
        file_id: &FileId,
        options: ListEntitiesOptions,
    ) -> Vec<StoredEntity>;
    fn get_entity(&self, entity_id: &EntityId) -> Option<StoredEntity>;

    /// # Errors
    ///
    /// Returns an error when storage is closed or the FTS recall query fails.
    fn search_fts(
        &self,
        query: &str,
        limit: usize,
        filter: Option<&StorageSearchFilter>,
    ) -> EngineResult<Vec<StorageSearchHit>>;
    /// # Errors
    ///
    /// Returns an error when storage is closed or the vector recall query fails.
    fn search_vector(
        &self,
        vector: &[f32],
        limit: usize,
        filter: Option<&StorageSearchFilter>,
    ) -> EngineResult<Vec<StorageSearchHit>>;

    /// # Errors
    ///
    /// Returns an error when storage is read-only or closed, fragments fail validation or
    /// encoding, or the write fails.
    fn replace_file(
        &mut self,
        file: &FileInfo,
        entries: &[IndexedFragment],
        diagnostics: Option<&FileIndexDiagnostics>,
    ) -> EngineResult<()>;
    /// Batched [`replace_file`](Self::replace_file): pending metadata for all
    /// files, document deletes, document upserts, then indexed metadata —
    /// two metadata snapshots per batch. Every entry is validated and encoded
    /// before any destructive work; an empty batch is a no-op. A
    /// metadata-checkpoint failure aborts the batch as the operation error;
    /// document failures attribute to the owning file.
    ///
    /// Each tuple is `(file, entries, diagnostics)`, mirroring [`replace_file`](Self::replace_file).
    ///
    /// # Errors
    ///
    /// Returns an error when storage is read-only or closed, fragments fail validation or
    /// encoding, or the write fails.
    fn replace_files_batch(
        &mut self,
        batch: &[(FileInfo, Vec<IndexedFragment>, Option<FileIndexDiagnostics>)],
    ) -> EngineResult<()>;

    /// # Errors
    ///
    /// Returns an error when storage is read-only or closed, or the write fails.
    fn mark_file_failed(&mut self, file: &FileInfo, error: &str) -> EngineResult<()>;

    /// # Errors
    ///
    /// Returns an error when storage is read-only or closed, or the delete fails.
    fn delete_file(&mut self, file_id: &FileId) -> EngineResult<()>;

    /// Batched [`delete_file`](Self::delete_file): document deletes for all
    /// ids, then one metadata snapshot. An empty batch is a no-op. A
    /// metadata-checkpoint failure aborts the batch as the operation error;
    /// document failures attribute to the owning file.
    ///
    /// # Errors
    ///
    /// Returns an error when storage is read-only or closed, or the delete fails.
    fn delete_files_batch(&mut self, file_ids: &[FileId]) -> EngineResult<()>;

    /// Persists staged file-metadata mutations at a pipeline checkpoint.
    /// Checkpoints: prepare failure, zero-fragment replacement, per-file
    /// fallback failure, failed unit, stale deletion, finalization, shutdown.
    /// No-op when clean; error paths never flush, so cancellation never
    /// publishes queued work.
    ///
    /// # Errors
    ///
    /// Returns an error when the metadata write fails.
    fn flush(&mut self) -> EngineResult<()>;

    /// # Errors
    ///
    /// Returns an error when storage is read-only or closed, or the optimize fails.
    fn finalize_writes(&mut self) -> EngineResult<()>;
    fn close(&mut self);
}

/// Opens workspace index storage per `options`.
///
/// # Errors
///
/// Returns an error when the storage directory, lock, metadata store, or zvec collection cannot
/// be prepared or opened.
pub fn create_workspace_index_storage(
    options: StorageOptions<'_>,
) -> EngineResult<Box<dyn WorkspaceIndexStorage>> {
    zvec::ZvecWorkspaceIndexStorage::open(options)
}
