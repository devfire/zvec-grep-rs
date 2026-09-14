//! Read-only workspace introspection: index freshness and workspace
//! identity, mirroring `indexStatus` and `info()`.

use std::path::Path;

use super::service::ZvecGrepService;
use crate::error::EngineResult;
use crate::service::types::{ZvecGrepInfoResult, workspace_index_disabled};
use crate::service::workspace_index::{
    IndexMode, WorkspaceIndex, WorkspaceIndexOptions, is_workspace_indexed,
};
use crate::types::WorkspaceIndexStatus;

impl ZvecGrepService {
    /// Index freshness derived from stored files, mirroring `indexStatus`.
    ///
    /// # Errors
    ///
    /// Returns an error when no built index is found or the index cannot be opened for status.
    pub fn index_status(&self, root: Option<&Path>) -> EngineResult<WorkspaceIndexStatus> {
        let location = self.require_indexed_location(root)?;
        let manifest = self.require_manifest(&location)?;
        let index = WorkspaceIndex::open(
            manifest.info.clone(),
            WorkspaceIndexOptions {
                mode: IndexMode::Read,
                embedding_model: None,
            },
        )?;
        index.status()
    }

    /// Workspace identity, policy, embedding, and status, mirroring `info()`.
    ///
    /// # Errors
    ///
    /// Returns an error when the manifest cannot be read or the workspace index is disabled.
    pub fn workspace_info(&self, root: Option<&Path>) -> EngineResult<ZvecGrepInfoResult> {
        let start = self.root_string(root);
        let Some(location) = crate::service::root::find_nearest_workspace_index(&start) else {
            return Ok(not_indexed_result(&start));
        };
        let manifest = self.require_manifest(&location)?;
        if manifest.info.index_policy == Some(crate::types::WorkspaceIndexPolicy::Disabled) {
            return Err(workspace_index_disabled(&location.root));
        }
        let indexed = is_workspace_indexed(&manifest.info);
        let embedding = manifest.info.embedding.clone().flatten().map(|schema| {
            crate::service::types::EmbeddingInfo {
                provider: schema.provider,
                model: schema.model,
                dimension: schema.dimension,
                metric: schema.metric,
            }
        });
        let (status, suggestion) = if indexed {
            let status = WorkspaceIndex::open(
                manifest.info.clone(),
                WorkspaceIndexOptions {
                    mode: IndexMode::Read,
                    embedding_model: None,
                },
            )
            .and_then(|index| index.status())
            .ok();
            (status, None)
        } else {
            (
                None,
                Some("workspace is not indexed; run zg index first".to_owned()),
            )
        };
        Ok(ZvecGrepInfoResult {
            root: location.root,
            indexed,
            index_policy: manifest.info.index_policy,
            embedding,
            workspace_index: Some(manifest.info.clone()),
            status,
            suggestion,
        })
    }
}

pub(super) fn not_indexed_result(root: &str) -> ZvecGrepInfoResult {
    ZvecGrepInfoResult {
        root: root.to_owned(),
        indexed: false,
        suggestion: Some("workspace is not indexed; run zg index first".to_owned()),
        ..ZvecGrepInfoResult::default()
    }
}
