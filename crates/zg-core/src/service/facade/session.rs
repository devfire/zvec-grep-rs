//! Explicit RAII read sessions: `context()` with `auto_update` forced off.
//! The facade holds no timers; TTL eviction belongs to the daemon (phase G).

use std::path::Path;

use super::context::normalize::{NormalizedContextRequest, normalize_context_request};
use super::context::search::run_context_search;
use super::service::ZvecGrepService;
use crate::error::{EngineError, EngineResult, codes};
use crate::service::types::{
    ZvecGrepContextOptions, ZvecGrepContextResult, workspace_index_disabled,
    workspace_index_not_found,
};
use crate::service::workspace_index::{
    IndexMode, WorkspaceIndex, WorkspaceIndexOptions, is_workspace_indexed,
};
use crate::types::WorkspaceIndexInfo;

impl ZvecGrepService {
    /// Opens an explicit RAII read session; the daemon owns TTL eviction.
    ///
    /// # Errors
    ///
    /// Returns an error when no built index is found, the index is disabled or unbuilt, or model
    /// resolution or index open fails.
    pub fn open_read_session(&self, root: Option<&Path>) -> EngineResult<ReadSession> {
        let location = self.require_indexed_location(root)?;
        let manifest = self.require_manifest(&location)?;
        if manifest.info.index_policy == Some(crate::types::WorkspaceIndexPolicy::Disabled) {
            return Err(workspace_index_disabled(&location.root));
        }
        if !is_workspace_indexed(&manifest.info) {
            return Err(workspace_index_not_found(&location.root));
        }
        let model = self.model_for_manifest(Some(&manifest))?;
        let index = WorkspaceIndex::open(
            manifest.info.clone(),
            WorkspaceIndexOptions {
                mode: IndexMode::Read,
                embedding_model: Some(model),
            },
        )?;
        Ok(ReadSession {
            root: location.root,
            info: manifest.info,
            index: Some(index),
        })
    }
}

/// Explicit read-session guard: `context()` with `auto_update` forced off.
///
/// The facade holds no timers; TTL eviction belongs to the daemon (phase G).
/// Single-owner close: the session owns shutdown — [`close`](ReadSession::close)
/// consumes the guard and drains the one [`WorkspaceIndex`] through its consuming
/// primitive, so a second close is a compile-time move error. [`Drop`] only
/// backstops a guard dropped without close. `context()` rejects a closed session
/// with `SERVICE.READ_SESSION_CLOSED` instead of reading closed storage.
pub struct ReadSession {
    root: String,
    info: WorkspaceIndexInfo,
    index: Option<WorkspaceIndex>,
}

impl ReadSession {
    /// Workspace root the session was opened for.
    #[must_use]
    pub fn root(&self) -> &str {
        &self.root
    }

    /// Searches through the open read handle; errors once closed.
    ///
    /// # Errors
    ///
    /// Returns an error when the session is closed, the query is empty, or search fails.
    pub fn context(
        &self,
        options: &ZvecGrepContextOptions<'_>,
    ) -> EngineResult<ZvecGrepContextResult> {
        let Some(index) = self.index.as_ref() else {
            return Err(EngineError::new(
                codes::service_read_session_closed(),
                "workspace read session is already closed",
            ));
        };
        let request: NormalizedContextRequest = normalize_context_request(options)?;
        run_context_search(index, &self.root, &self.info, &request, options)
    }

    /// Consumes the guard and closes the storage handle exactly once.
    ///
    /// Drains the owned [`WorkspaceIndex`] through its consuming close; the
    /// following [`Drop`] then sees an empty guard and stays a no-op.
    pub fn close(mut self) {
        if let Some(index) = self.index.take() {
            index.close();
        }
    }
}

impl Drop for ReadSession {
    /// Backstop for guards dropped without [`close`](ReadSession::close):
    /// shuts the drained handle down once, or stays a no-op when close
    /// already took it.
    fn drop(&mut self) {
        if let Some(index) = self.index.take() {
            index.close();
        }
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    use crate::error::EngineErrorCode;
    use crate::ids::FileId;
    use crate::storage::{StorageOptions, WorkspaceIndexStorage, create_workspace_index_storage};
    use crate::types::{
        CURRENT_INDEX_VERSION, FileFormat, FileInfo, FileKind, RootPath, SearchMetric, UnixMillis,
        WorkspaceIndexEmbeddingSchema,
    };

    fn test_info(storage_path: &std::path::Path) -> WorkspaceIndexInfo {
        WorkspaceIndexInfo {
            id: "test".to_owned(),
            name: "test".to_owned(),
            path: storage_path.to_string_lossy().into_owned(),
            root_paths: vec![RootPath {
                absolute_path: storage_path.to_string_lossy().into_owned(),
                recursive: true,
                ..RootPath::default()
            }],
            index_policy: None,
            embedding: Some(Some(test_schema())),
            index_version: Some(CURRENT_INDEX_VERSION),
            created_time: UnixMillis::now(),
            updated_time: UnixMillis::now(),
        }
    }

    fn test_schema() -> WorkspaceIndexEmbeddingSchema {
        WorkspaceIndexEmbeddingSchema {
            provider: "test".to_owned(),
            model: "dummy".to_owned(),
            dimension: 4,
            metric: SearchMetric::Cosine,
        }
    }

    /// Checkpoints one file record so `files.json` exists: `close`/`drop`
    /// only flush staged writes, so a never-written store has no metadata
    /// file and a read-only open correctly fails with FILE_META_MISSING.
    /// The handle is closed (and its lock released on drop) before return,
    /// so callers can reopen the storage in any mode.
    fn seed_persisted_storage(
        storage_path: &std::path::Path,
        schema: &WorkspaceIndexEmbeddingSchema,
    ) {
        std::fs::create_dir_all(storage_path).expect("create seed storage dir");
        let absolute_path = storage_path.join("seed.txt").to_string_lossy().into_owned();
        std::fs::write(&absolute_path, "0123456789abcdef").expect("write seed file");
        let mut storage = create_workspace_index_storage(StorageOptions::ReadWrite {
            storage_path,
            embedding: schema,
        })
        .expect("open seed storage");
        storage
            .replace_file(
                &FileInfo {
                    id: FileId::from_raw("seed".to_owned()),
                    absolute_path,
                    relative_path: "seed.txt".to_owned(),
                    root_path: storage_path.to_string_lossy().into_owned(),
                    size_bytes: 16,
                    last_modified_time: UnixMillis::from_millis(1_700_000_000_000),
                    content_hash: Some("hash-seed".to_owned()),
                    kind: FileKind::Text,
                    format: FileFormat::parse("text"),
                    index_status: None,
                },
                &[],
                None,
            )
            .expect("seed test storage");
        storage.close();
    }

    fn open_session(dir: &tempfile::TempDir) -> ReadSession {
        let storage_path = dir.path().join("storage");
        let schema = test_schema();
        // Read-only open needs persisted storage: checkpoint one file record
        // first, then pass through a write handle, mirroring the real
        // index-then-read flow.
        seed_persisted_storage(&storage_path, &schema);
        let info = test_info(&storage_path);
        WorkspaceIndex::open(
            info.clone(),
            WorkspaceIndexOptions {
                mode: IndexMode::Write,
                embedding_model: None,
            },
        )
        .expect("build test storage")
        .close();
        let index = WorkspaceIndex::open(
            info.clone(),
            WorkspaceIndexOptions {
                mode: IndexMode::Read,
                embedding_model: None,
            },
        )
        .expect("open test index");
        ReadSession {
            root: storage_path.to_string_lossy().into_owned(),
            info,
            index: Some(index),
        }
    }

    #[test]
    fn close_then_context_errors() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let mut session = open_session(&dir);
        // Post-close state: consuming `close(self)` makes a second call a
        // compile-time move error, so the drained guard stands in for it.
        session.index = None;
        let error = session
            .context(&ZvecGrepContextOptions::default())
            .expect_err("context after close errors");
        assert_eq!(*error.code(), EngineErrorCode::ServiceReadSessionClosed);
    }

    #[test]
    fn close_is_single_close() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        // Consuming close drains the handle; the Drop that follows sees an
        // empty guard, so close-then-drop never shuts storage twice.
        open_session(&dir).close();
    }

    #[test]
    fn drop_without_close_closes_handle() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let storage_path = dir.path().join("storage");
        let info = test_info(&storage_path);
        drop(open_session(&dir));
        let reopened = WorkspaceIndex::open(
            info,
            WorkspaceIndexOptions {
                mode: IndexMode::Read,
                embedding_model: None,
            },
        )
        .expect("reopen after drop");
        reopened.status().expect("status after reopen");
    }
}
