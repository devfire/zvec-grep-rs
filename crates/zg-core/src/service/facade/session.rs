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
/// Closing is RAII — [`close`](ReadSession::close) consumes the guard and
/// [`Drop`] closes the storage handle either way.
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

    /// Consumes the guard and closes the storage handle.
    pub fn close(mut self) {
        if let Some(index) = self.index.as_mut() {
            index.close();
        }
        self.index = None;
    }
}

impl Drop for ReadSession {
    fn drop(&mut self) {
        if let Some(index) = self.index.as_mut() {
            index.close();
        }
    }
}
