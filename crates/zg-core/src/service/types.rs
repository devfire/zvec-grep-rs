//! Public service types: the `context()` search contract, index options, and
//! info results shared by CLI, daemon, and MCP layers.
//!
//! JSON field naming follows the TS wire format: camelCase except where the
//! daemon status DTO explicitly uses snake_case.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::ids::EntityId;
use crate::pipeline::indexing::IndexProgressSink;
use crate::types::{
    CodeSymbolType, Content, EntityMetadata, Range, RootPath, SearchMetric,
    UnixMillis, WorkspaceIndexInfo, WorkspaceIndexPolicy,
};

/// Abort probe: return `true` to cancel a long-running operation.
///
/// Owned and `Send + Sync` (M4) so options structs can cross the async
/// boundary via `spawn_blocking`; borrowed `&dyn Fn` is leaf-only.
pub type AbortCheck = Arc<dyn Fn() -> bool + Send + Sync>;

/// How the embedding model handle is owned by the service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmbeddingModelOwnership {
    Owned,
    Borrowed,
}

/// Options accepted by [`crate::service::ZvecGrepService::index`].
#[derive(Default)]
pub struct ZvecGrepIndexOptions<'a> {
    pub root: Option<&'a std::path::Path>,
    pub root_paths: Vec<RootPathSpec<'a>>,
    pub rebuild: bool,
    pub reset_paths: bool,
    pub include_paths: Vec<String>,
    pub exclude_paths: Vec<String>,
    pub globs: Vec<String>,
    pub insensitive_globs: Vec<String>,
    pub file_types: Vec<String>,
    pub excluded_file_types: Vec<String>,
    pub hidden: Option<bool>,
    pub no_ignore: Option<bool>,
    pub ignore_files: Vec<String>,
    pub max_depth: Option<u32>,
    pub max_file_size_bytes: Option<u64>,
    pub follow: Option<bool>,
    pub embedding_concurrency: Option<usize>,
    pub on_progress: Option<IndexProgressSink>,
    pub changed_paths: Vec<std::path::PathBuf>,
    pub signal: Option<AbortCheck>,
}

/// One root path entry: either a plain directory string or a full spec.
#[derive(Debug, Clone)]
pub enum RootPathSpec<'a> {
    Path(&'a str),
    Full(Box<RootPath>),
}

/// Result of `info()` / `disableIndex()`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ZvecGrepInfoResult {
    pub root: String,
    pub indexed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index_policy: Option<WorkspaceIndexPolicy>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub embedding: Option<EmbeddingInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_index: Option<WorkspaceIndexInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<crate::types::WorkspaceIndexStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<String>,
}

/// Embedding identity reported by `info()`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmbeddingInfo {
    pub provider: String,
    pub model: String,
    pub dimension: usize,
    pub metric: SearchMetric,
}

/// Options accepted by [`crate::service::ZvecGrepService::context`].
#[derive(Default)]
pub struct ZvecGrepContextOptions<'a> {
    pub root: Option<&'a std::path::Path>,
    /// Primary natural-language query.
    pub query: Option<String>,
    /// Additional primary queries.
    pub queries: Vec<String>,
    /// Fully-specified routes (mode + query).
    pub routes: Vec<crate::types::SearchPlanRoute>,
    /// Shorthand: fts terms (implies fts routes).
    pub fts: Vec<String>,
    /// Shorthand: vector queries (implies vector routes).
    pub vector: Vec<String>,
    /// Fuse all groups into one.
    pub fuse: bool,
    pub limit: Option<usize>,
    pub trace: bool,
    pub track_entity_id: Option<EntityId>,
    pub prefer_symbol: bool,
    pub symbol_types: Vec<CodeSymbolType>,
    pub include_paths: Vec<String>,
    pub exclude_paths: Vec<String>,
    pub globs: Vec<String>,
    pub insensitive_globs: Vec<String>,
    pub file_types: Vec<String>,
    pub excluded_file_types: Vec<String>,
    pub modified_after: Option<UnixMillis>,
    pub modified_before: Option<UnixMillis>,
    /// Exhaustive lexical path.
    pub rg: Option<RgOptions>,
    /// Refresh a stale index before searching (default true).
    pub auto_update: bool,
    pub signal: Option<AbortCheck>,
}

impl ZvecGrepContextOptions<'_> {
    /// True unless the caller explicitly disabled auto-refresh.
    pub fn wants_auto_update(&self) -> bool {
        self.auto_update
    }
}

/// Options for the exhaustive lexical (`rg`) path.
#[derive(Debug, Clone, Default)]
pub struct RgOptions {
    pub pattern: Option<String>,
    pub case_insensitive: bool,
    pub smart_case: bool,
    pub fixed_strings: bool,
    pub max_count: Option<usize>,
    pub context_lines: Option<usize>,
}

/// Where a context result came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ContextSource {
    Index,
    Rg,
}

/// Coverage guarantee of a context result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContextCoverage {
    RankedSample,
    RgExhaustive,
    RgTruncated,
}

/// One search result item.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextItem {
    pub kind: ContextItemKind,
    pub rank: usize,
    pub file: ContextFile,
    pub range: Range,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub excerpt_range: Option<Range>,
    pub content: Content,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_role: Option<ContentRole>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outline: Option<String>,
    pub status: ContentStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<EntityMetadata>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entity_id: Option<EntityId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub query_groups: Vec<QueryGroupRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container: Option<StructuralContainer>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selection_reason: Option<SelectionReason>,
}

/// Item provenance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextItemKind {
    IndexedEntity,
    RgMatch,
}

/// File identity block of a context item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextFile {
    pub absolute_path: String,
    pub relative_path: String,
    pub root_path: String,
}

/// Role of the item content relative to its entity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ContentRole {
    Source,
    Outline,
}

/// Freshness assessment of item content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentStatus {
    Fresh,
    PossiblyStale,
}

/// Reference to the query group that matched an item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryGroupRef {
    pub id: String,
    pub rank: usize,
}

/// Enclosing structural entity attached to lexical matches.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StructuralContainer {
    pub entity_id: EntityId,
    pub range: Range,
    pub metadata: Option<EntityMetadata>,
}

/// Why an item appears in the prioritized list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectionReason {
    Coverage,
    GlobalFill,
}

/// Diagnostics block of a context result.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextDiagnostics {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub empty_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rg: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub structure: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timings: Option<serde_json::Value>,
}

/// Result of `context()`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ZvecGrepContextResult {
    pub query: String,
    pub root: String,
    pub source: ContextSource,
    pub coverage: ContextCoverage,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_index: Option<WorkspaceIndexInfo>,
    pub items: Vec<ContextItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group_results: Option<Vec<GroupResult>>,
    pub diagnostics: ContextDiagnostics,
}

/// Per-query-group search result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupResult {
    pub id: String,
    pub query: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<GroupRole>,
    pub hits: Vec<crate::types::SearchHit>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timings: Option<serde_json::Value>,
}

/// Role of a query group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GroupRole {
    Primary,
    Supplemental,
}

/// Convenience constructor matching the TS `EMPTY_QUERY` error path.
pub fn empty_query_error() -> crate::error::EngineError {
    crate::error::EngineError::new(
        crate::error::EngineErrorCode::from_static("CONTEXT.EMPTY_QUERY"),
        "query is required",
    )
}

/// Error used when a requested root has no workspace index.
pub fn workspace_index_not_found(root: &str) -> crate::error::EngineError {
    crate::error::EngineError::new(
        crate::error::EngineErrorCode::from_static("CONTEXT.WORKSPACE_INDEX_NOT_FOUND"),
        "workspace index not found",
    )
    .with_context(format!("root={root}"))
}

/// Error used when the workspace index exists but is disabled.
pub fn workspace_index_disabled(root: &str) -> crate::error::EngineError {
    crate::error::EngineError::new(
        crate::error::EngineErrorCode::from_static("CONTEXT.WORKSPACE_INDEX_DISABLED"),
        "workspace index is disabled",
    )
    .with_context(format!("root={root}"))
}

/// Options structs must stay `Send` (M4): they cross the async boundary via
/// `spawn_blocking`. A borrowed callback field would break this; the test
/// below pins it.
#[cfg(test)]
mod tests {
    use super::*;

    fn assert_send<T: Send>() {}

    #[test]
    fn options_are_send() {
        assert_send::<ZvecGrepIndexOptions<'static>>();
        assert_send::<ZvecGrepContextOptions<'static>>();
    }
}
