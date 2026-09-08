//! Opened workspace index: version/embedding validation plus indexing,
//! status, and search over one storage handle.
//!
//! Port of `engine/service/workspace-index.ts` (`WorkspaceIndex`,
//! `isWorkspaceIndexed`).

use std::sync::Arc;

use crate::error::{
    error_details, workspace_index_detail, DetailEntry, DetailValue, EngineError, EngineErrorCode,
    EngineResult,
};
use crate::models::EmbeddingModel;
use crate::pipeline::indexing::{
    get_workspace_index_status, index_workspace, index_workspace_paths, IndexContext,
    IndexProgressSink,
};
use crate::pipeline::indexing::scanner::CancelFlag;
use crate::pipeline::search::{search_workspace_index, SearchContext};
use crate::storage::{create_workspace_index_storage, StorageOptions, WorkspaceIndexStorage};
use crate::types::{
    CURRENT_INDEX_VERSION, IndexResult, SearchPlan, SearchPlanResult,
    WorkspaceIndexEmbeddingSchema, WorkspaceIndexInfo, WorkspaceIndexStatus,
};

/// Open mode (mirrors `WorkspaceIndexOptions["mode"]`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexMode {
    Read,
    Write,
}

/// Options for [`WorkspaceIndex::open`].
pub struct WorkspaceIndexOptions {
    pub mode: IndexMode,
    pub embedding_model: Option<Arc<dyn EmbeddingModel>>,
}

/// Per-run indexing options (mirrors `IndexOptions`).
#[derive(Clone, Default)]
pub struct IndexOptions {
    pub embedding_concurrency: Option<usize>,
    pub on_progress: Option<IndexProgressSink>,
    pub changed_paths: Option<Vec<String>>,
    pub cancel: Option<CancelFlag>,
}

/// One opened workspace index.
pub struct WorkspaceIndex {
    info: WorkspaceIndexInfo,
    storage: Box<dyn WorkspaceIndexStorage>,
    embedding: WorkspaceIndexEmbeddingSchema,
    embedding_model: Option<Arc<dyn EmbeddingModel>>,
    closed: bool,
}

impl WorkspaceIndex {
    /// Opens (or creates, in write mode) the index described by `info`.
    pub fn open(info: WorkspaceIndexInfo, options: WorkspaceIndexOptions) -> EngineResult<Self> {
        let embedding = require_workspace_index_embedding(&info, "open")?;
        validate_index_version(&info)?;
        if let Some(model) = &options.embedding_model {
            validate_embedding_schema(&info, &embedding, model.as_ref())?;
        }
        let storage_path = std::path::Path::new(&info.path);
        let storage = match options.mode {
            IndexMode::Write => create_workspace_index_storage(StorageOptions::ReadWrite {
                storage_path,
                embedding: &embedding,
            })?,
            IndexMode::Read => {
                create_workspace_index_storage(StorageOptions::ReadOnly { storage_path })?
            }
        };
        Ok(Self {
            info,
            storage,
            embedding,
            embedding_model: options.embedding_model,
            closed: false,
        })
    }

    /// Index display name.
    pub fn name(&self) -> &str {
        &self.info.name
    }

    /// Index identity the handle was opened with.
    pub fn info(&self) -> &WorkspaceIndexInfo {
        &self.info
    }

    /// Recorded embedding schema the handle was opened with.
    pub fn embedding(&self) -> &WorkspaceIndexEmbeddingSchema {
        &self.embedding
    }

    /// Runs indexing, or changed-path indexing when requested (mirrors
    /// `WorkspaceIndex#index`).
    pub fn index(&mut self, options: &IndexOptions) -> EngineResult<IndexResult> {
        if self.storage.read_only() {
            return Err(EngineError::new(
                EngineErrorCode::from_static("WORKSPACE_INDEX.READ_ONLY"),
                "cannot update a read-only workspace index",
            )
            .with_context(workspace_index_operation_details(&self.info.name, "index")));
        }
        let embedding_model = self.require_embedding_model("index")?;
        let mut ctx = IndexContext {
            workspace_index: self.info.clone(),
            storage: &mut *self.storage,
            embedding_model,
            embedding_concurrency: options.embedding_concurrency,
            on_progress: options.on_progress.clone(),
            cancel: options.cancel.clone(),
        };
        match &options.changed_paths {
            Some(paths) if !paths.is_empty() => index_workspace_paths(&mut ctx, paths),
            _ => index_workspace(&mut ctx),
        }
    }

    /// Derives status from stored files (mirrors `WorkspaceIndex#status`).
    pub fn status(&self) -> EngineResult<WorkspaceIndexStatus> {
        get_workspace_index_status(&self.info, &self.storage.list_files(), None)
    }

    /// Executes a hybrid search plan (mirrors `WorkspaceIndex#searchPlan`).
    pub fn search_plan(&self, plan: &SearchPlan) -> EngineResult<SearchPlanResult> {
        search_workspace_index(
            plan,
            &SearchContext {
                workspace_index: self.info.clone(),
                storage: &*self.storage,
                embedding_model: self.embedding_model.clone(),
            },
        )
    }

    /// Closes the underlying storage handle (idempotent, mirrors `close`).
    pub fn close(&mut self) {
        if self.closed {
            return;
        }
        self.storage.close();
        self.closed = true;
    }

    fn require_embedding_model(&self, operation: &str) -> EngineResult<Arc<dyn EmbeddingModel>> {
        self.embedding_model.clone().ok_or_else(|| {
            EngineError::new(
                EngineErrorCode::from_static("WORKSPACE_INDEX.EMBEDDING_MODEL_REQUIRED"),
                "workspace index operation requires an embedding model",
            )
            .with_context(workspace_index_operation_details(&self.info.name, operation))
        })
    }
}

fn validate_index_version(info: &WorkspaceIndexInfo) -> EngineResult<()> {
    if info.index_version == Some(CURRENT_INDEX_VERSION)
        || info.index_version == Some(crate::types::LEGACY_TS_INDEX_VERSION)
    {
        return Ok(());
    }
    let actual = info.index_version.map(|version| version.to_string()).unwrap_or_else(|| "null".to_owned());
    let detail = error_details(vec![
        DetailEntry::Line(&workspace_index_detail(&info.name)),
        DetailEntry::Pair("expected", DetailValue::Int(CURRENT_INDEX_VERSION)),
        DetailEntry::Pair("actual", DetailValue::Str(&actual)),
        DetailEntry::Pair(
            "hint",
            DetailValue::Str(
                "Recreate the index with the current zvec-grep version; run \"zg index --rebuild\" for a workspace index.",
            ),
        ),
    ])
    .unwrap_or_default();
    Err(EngineError::new(
        EngineErrorCode::from_static("WORKSPACE_INDEX.VERSION_MISMATCH"),
        "workspace index version is not supported",
    )
    .with_context(detail))
}

fn validate_embedding_schema(
    info: &WorkspaceIndexInfo,
    expected: &WorkspaceIndexEmbeddingSchema,
    current: &dyn EmbeddingModel,
) -> EngineResult<()> {
    validate_index_version(info)?;
    let actual = current.info();
    if expected.provider != actual.provider {
        return Err(schema_mismatch(
            &info.name,
            "provider",
            EngineErrorCode::from_static("WORKSPACE_INDEX.EMBEDDING_PROVIDER_MISMATCH"),
            "workspace index embedding provider does not match current model",
            &expected.provider,
            &actual.provider,
        ));
    }
    if expected.model != actual.model {
        return Err(schema_mismatch(
            &info.name,
            "model",
            EngineErrorCode::from_static("WORKSPACE_INDEX.EMBEDDING_MODEL_MISMATCH"),
            "workspace index embedding model does not match current model",
            &expected.model,
            &actual.model,
        ));
    }
    if expected.dimension != actual.dimension {
        return Err(schema_mismatch(
            &info.name,
            "dimension",
            EngineErrorCode::from_static("WORKSPACE_INDEX.EMBEDDING_DIMENSION_MISMATCH"),
            "workspace index embedding dimension does not match current model",
            &expected.dimension.to_string(),
            &actual.dimension.to_string(),
        ));
    }
    if expected.metric != actual.metric {
        return Err(schema_mismatch(
            &info.name,
            "metric",
            EngineErrorCode::from_static("WORKSPACE_INDEX.EMBEDDING_METRIC_MISMATCH"),
            "workspace index embedding metric does not match current model",
            &format!("{:?}", expected.metric),
            &format!("{:?}", actual.metric),
        ));
    }
    Ok(())
}

fn schema_mismatch(
    name: &str,
    _field: &str,
    code: EngineErrorCode,
    message: &str,
    expected: &str,
    actual: &str,
) -> EngineError {
    let detail = error_details(vec![
        DetailEntry::Line(&workspace_index_detail(name)),
        DetailEntry::Pair("expected", DetailValue::Str(expected)),
        DetailEntry::Pair("actual", DetailValue::Str(actual)),
    ])
    .unwrap_or_default();
    EngineError::new(code, message).with_context(detail)
}

fn workspace_index_operation_details(name: &str, operation: &str) -> String {
    error_details(vec![
        DetailEntry::Line(&workspace_index_detail(name)),
        DetailEntry::Pair("operation", DetailValue::Str(operation)),
    ])
    .unwrap_or_default()
}

/// True when the manifest record describes a built index (mirrors
/// `isWorkspaceIndexed`).
pub fn is_workspace_indexed(info: &WorkspaceIndexInfo) -> bool {
    matches!(info.embedding, Some(Some(_))) && info.index_version.is_some()
}

fn require_workspace_index_embedding(
    info: &WorkspaceIndexInfo,
    operation: &str,
) -> EngineResult<WorkspaceIndexEmbeddingSchema> {
    if let Some(Some(embedding)) = &info.embedding {
        if info.index_version.is_some() {
            return Ok(embedding.clone());
        }
    }
    let detail = error_details(vec![
        DetailEntry::Line(&workspace_index_detail(&info.name)),
        DetailEntry::Pair("operation", DetailValue::Str(operation)),
        DetailEntry::Pair("hint", DetailValue::Str("Run zg index to build this index.")),
    ])
    .unwrap_or_default();
    Err(EngineError::new(
        EngineErrorCode::from_static("WORKSPACE_INDEX.MISSING"),
        "workspace index has not been built",
    )
    .with_context(detail))
}

