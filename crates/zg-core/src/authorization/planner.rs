//! Authorization planning: decides whether a remote-embedding operation
//! needs user authorization, mirroring `src/authorization/planner.ts`.
//!
//! Divergence: TS takes the MCP `NormalizedSearchInput`; the core planner
//! takes the already-normalized signals (`uses_vector`, `auto_update`,
//! `freshness_wait`) so `zg-core` does not depend on the MCP layer. The
//! MCP crate adapts its normalized input to these flags in phase H.

use super::error::AuthError;
use super::store::RemoteEmbeddingAuthorizationStore;
use super::target::create_remote_embedding_target;
use super::types::{
    PlanReason, RemoteEmbeddingDisclosure, RemoteEmbeddingOperation, RemoteEmbeddingPlan,
    WorkspaceContentDisclosure,
};
use crate::error::EngineResult;
use crate::models::EmbeddingModelInfo;
use crate::service::types::ZvecGrepInfoResult;

/// Index-side planning input.
pub struct PlanIndexInput<'a> {
    pub info: &'a ZvecGrepInfoResult,
    pub model: &'a EmbeddingModelInfo,
    pub rebuild: bool,
    pub needs_update: bool,
}

/// Search-side planning input (pre-normalized MCP signals).
pub struct PlanSearchInput<'a> {
    pub info: &'a ZvecGrepInfoResult,
    pub model: &'a EmbeddingModelInfo,
    pub uses_vector: bool,
    pub auto_update: bool,
    /// `freshness == "wait_for_fresh"`.
    pub freshness_wait: bool,
    pub runtime_needs_reconciliation: bool,
}

/// Plans remote-index authorization, mirroring
/// `planRemoteIndexAuthorization`. `None` when no remote embedding runs.
///
/// # Errors
///
/// Returns [`AuthError::InvalidTarget`] when the model has no remote endpoint or the target and grant path cannot be built.
pub fn plan_remote_index_authorization(
    input: &PlanIndexInput<'_>,
) -> EngineResult<Option<RemoteEmbeddingPlan>> {
    if input.model.provider != "qwen" {
        return Ok(None);
    }
    let needs_embedding = input.rebuild
        || input.needs_update
        || !input.info.indexed
        || !index_status_is_fresh(input.info);
    if !needs_embedding {
        return Ok(None);
    }
    let Some(endpoint) = remote_endpoint(input.model) else {
        return Err(AuthError::InvalidTarget {
            detail: format!(
                "Embedding model {} did not provide a remote endpoint.",
                input.model.reference
            ),
        }
        .into());
    };
    let target = create_remote_embedding_target(
        &workspace_roots(input.info),
        &input.model.provider,
        &input.model.model,
        &endpoint,
    )?;
    let store = RemoteEmbeddingAuthorizationStore::new();
    let grant_path = store
        .grant_path(&target)
        .map_err(crate::error::EngineError::from)?;
    Ok(Some(RemoteEmbeddingPlan {
        operation: RemoteEmbeddingOperation::Index,
        target,
        disclosure: RemoteEmbeddingDisclosure {
            query_text: false,
            workspace_content: if !input.info.indexed || input.rebuild {
                WorkspaceContentDisclosure::Full
            } else {
                WorkspaceContentDisclosure::Changed
            },
        },
        reason: if input.rebuild {
            PlanReason::IndexRebuild
        } else if input.info.indexed {
            PlanReason::IndexUpdate
        } else {
            PlanReason::IndexCreate
        },
        grant_path,
    }))
}

/// Plans remote-search authorization, mirroring
/// `planRemoteSearchAuthorization`. `None` when no remote embedding runs.
///
/// # Errors
///
/// Returns [`AuthError::InvalidTarget`] when the model mismatches the indexed schema, has no remote endpoint, or the target cannot be built.
pub fn plan_remote_search_authorization(
    input: &PlanSearchInput<'_>,
) -> EngineResult<Option<RemoteEmbeddingPlan>> {
    let schema = input
        .info
        .workspace_index
        .as_ref()
        .and_then(|index| index.embedding.clone())
        .flatten();
    let Some(schema) = schema else {
        return Ok(None);
    };
    if !input.info.indexed || schema.provider != "qwen" {
        return Ok(None);
    }
    if input.model.provider != schema.provider || input.model.model != schema.model {
        return Err(AuthError::InvalidTarget {
            detail: format!(
                "Embedding model {} does not match indexed model {}/{}.",
                input.model.reference, schema.provider, schema.model
            ),
        }
        .into());
    }
    let needs_update = input.runtime_needs_reconciliation || !index_status_is_fresh(input.info);
    let updates_index = needs_update && (input.auto_update || input.freshness_wait);
    if !input.uses_vector && !updates_index {
        return Ok(None);
    }
    let Some(endpoint) = remote_endpoint(input.model) else {
        return Err(AuthError::InvalidTarget {
            detail: format!(
                "Embedding model {} did not provide a remote endpoint.",
                input.model.reference
            ),
        }
        .into());
    };
    let target = create_remote_embedding_target(
        &workspace_roots(input.info),
        &input.model.provider,
        &input.model.model,
        &endpoint,
    )?;
    let store = RemoteEmbeddingAuthorizationStore::new();
    let grant_path = store
        .grant_path(&target)
        .map_err(crate::error::EngineError::from)?;
    Ok(Some(RemoteEmbeddingPlan {
        operation: if input.uses_vector {
            if updates_index {
                RemoteEmbeddingOperation::QueryAndIndex
            } else {
                RemoteEmbeddingOperation::Query
            }
        } else {
            RemoteEmbeddingOperation::Index
        },
        target,
        disclosure: RemoteEmbeddingDisclosure {
            query_text: input.uses_vector,
            workspace_content: if updates_index {
                WorkspaceContentDisclosure::Changed
            } else {
                WorkspaceContentDisclosure::None
            },
        },
        reason: PlanReason::Query,
        grant_path,
    }))
}

/// Remote endpoint for planning, mirroring TS (`input.model.endpoint`).
fn remote_endpoint(model: &EmbeddingModelInfo) -> Option<String> {
    model.endpoint.clone()
}

/// True when the stored status shows no pending work, mirroring
/// `indexStatusIsFresh`.
#[must_use]
pub fn index_status_is_fresh(info: &ZvecGrepInfoResult) -> bool {
    let Some(status) = info.status.as_ref() else {
        return false;
    };
    status.files_added == 0
        && status.files_modified == 0
        && status.files_deleted == 0
        && status.files_pending == 0
        && status.files_failed == 0
}

fn workspace_roots(info: &ZvecGrepInfoResult) -> Vec<String> {
    let roots: Vec<String> = info
        .workspace_index
        .as_ref()
        .map(|index| {
            index
                .root_paths
                .iter()
                .map(|root| root.absolute_path.clone())
                .collect()
        })
        .unwrap_or_default();
    if roots.is_empty() {
        vec![info.root.clone()]
    } else {
        roots
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{RootPath, SearchMetric, WorkspaceIndexEmbeddingSchema, WorkspaceIndexInfo};

    fn model() -> EmbeddingModelInfo {
        EmbeddingModelInfo {
            reference: "qwen/text-embedding-v4".to_owned(),
            provider: "qwen".to_owned(),
            model: "text-embedding-v4".to_owned(),
            dimension: 1024,
            metric: SearchMetric::Cosine,
            supports_images: false,
            max_input_tokens: Some(8192),
            input_kinds: vec![crate::models::EmbeddingInputKind::Text],
            endpoint: Some(
                "https://dashscope.aliyuncs.com/compatible-mode/v1/embeddings".to_owned(),
            ),
            default_concurrency: None,
        }
    }

    fn info(root: &str, status: crate::types::WorkspaceIndexStatus) -> ZvecGrepInfoResult {
        ZvecGrepInfoResult {
            root: root.to_owned(),
            indexed: true,
            index_policy: None,
            embedding: None,
            workspace_index: Some(WorkspaceIndexInfo {
                id: "workspace-index".to_owned(),
                name: "workspace".to_owned(),
                path: root.to_owned(),
                root_paths: vec![RootPath {
                    absolute_path: root.to_owned(),
                    recursive: true,
                    ..RootPath::default()
                }],
                index_policy: None,
                embedding: Some(Some(WorkspaceIndexEmbeddingSchema {
                    provider: "qwen".to_owned(),
                    model: "text-embedding-v4".to_owned(),
                    dimension: 1024,
                    metric: SearchMetric::Cosine,
                })),
                index_version: Some(1),
                created_time: crate::types::UnixMillis::from_millis(1),
                updated_time: crate::types::UnixMillis::from_millis(1),
            }),
            status: Some(status),
            suggestion: None,
        }
    }

    fn status(files_modified: usize) -> crate::types::WorkspaceIndexStatus {
        crate::types::WorkspaceIndexStatus {
            files_scanned: 1,
            files_modified,
            files_stored: 1,
            entities_indexed: 1,
            ..crate::types::WorkspaceIndexStatus::default()
        }
    }

    #[test]
    fn search_planner_follows_query_and_index_behavior() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().to_string_lossy().into_owned();
        let model = model();

        let query = plan_remote_search_authorization(&PlanSearchInput {
            info: &info(&root, status(0)),
            model: &model,
            uses_vector: true,
            auto_update: true,
            freshness_wait: false,
            runtime_needs_reconciliation: false,
        })
        .expect("plan")
        .expect("query plan");
        assert_eq!(query.operation, RemoteEmbeddingOperation::Query);
        assert_eq!(
            query.target.endpoint,
            "https://dashscope.aliyuncs.com/compatible-mode/v1/embeddings"
        );
        assert!(query.disclosure.query_text);
        assert_eq!(
            query.disclosure.workspace_content,
            WorkspaceContentDisclosure::None
        );

        let coupled = plan_remote_search_authorization(&PlanSearchInput {
            info: &info(&root, status(1)),
            model: &model,
            uses_vector: false,
            auto_update: true,
            freshness_wait: false,
            runtime_needs_reconciliation: false,
        })
        .expect("plan")
        .expect("coupled plan");
        assert_eq!(coupled.operation, RemoteEmbeddingOperation::Index);
        assert!(!coupled.disclosure.query_text);
        assert_eq!(
            coupled.disclosure.workspace_content,
            WorkspaceContentDisclosure::Changed
        );

        let skipped = plan_remote_search_authorization(&PlanSearchInput {
            info: &info(&root, status(1)),
            model: &model,
            uses_vector: false,
            auto_update: false,
            freshness_wait: false,
            runtime_needs_reconciliation: true,
        })
        .expect("plan");
        assert!(skipped.is_none());
    }

    #[test]
    fn index_planner_skips_fresh_and_reports_updates() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().to_string_lossy().into_owned();
        let model = model();
        let fresh = plan_remote_index_authorization(&PlanIndexInput {
            info: &info(&root, status(0)),
            model: &model,
            rebuild: false,
            needs_update: false,
        })
        .expect("plan");
        assert!(fresh.is_none());
        let update = plan_remote_index_authorization(&PlanIndexInput {
            info: &info(&root, status(1)),
            model: &model,
            rebuild: false,
            needs_update: false,
        })
        .expect("plan")
        .expect("update plan");
        assert_eq!(update.operation, RemoteEmbeddingOperation::Index);
        assert_eq!(update.reason, PlanReason::IndexUpdate);
    }

    #[test]
    fn local_models_need_no_authorization() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().to_string_lossy().into_owned();
        let local = EmbeddingModelInfo {
            reference: "local/potion-retrieval-32m".to_owned(),
            provider: "model2vec".to_owned(),
            model: "potion".to_owned(),
            endpoint: None,
            ..model()
        };
        let planned = plan_remote_index_authorization(&PlanIndexInput {
            info: &info(&root, status(1)),
            model: &local,
            rebuild: false,
            needs_update: false,
        })
        .expect("plan");
        assert!(planned.is_none());
    }
}
