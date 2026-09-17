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
    CodeSymbolType, Content, EntityMetadata, Range, RootPath, SearchMatchedBy, SearchMetric,
    UnixMillis, WorkspaceIndexInfo, WorkspaceIndexPolicy,
};

/// Abort probe: return `true` to cancel a long-running operation.
///
/// Owned and `Send + Sync` (M4) so options structs can cross the async
/// boundary via `spawn_blocking`; borrowed `&dyn Fn` is leaf-only.
pub type AbortCheck = Arc<dyn Fn() -> bool + Send + Sync>;

/// Options accepted by [`crate::service::facade::ZvecGrepService::ensure_index`].
///
/// Defaults: index the bound root with the stored (or default) discovery
/// settings — no rebuild, no path reset, full index run. Every filter and
/// concurrency cap is `None`/empty, meaning "keep the workspace default".
///
/// Invariants:
/// - `root_paths` non-empty wins over `root`, `reset_paths`, and stored
///   paths; entries are validated (`SCANNER.*` codes on overlap/stat
///   failures). Otherwise `reset_paths == false` (default) keeps the
///   manifest's stored root paths, while `true` replaces them.
/// - `max_file_size_bytes`: `Some(0)` is rejected with
///   `LEXICAL.SEARCH_FAILED` (see
///   [`crate::file_size_policy::validate_max_file_size_bytes`]); oversized
///   values clamp to 512 MiB
///   ([`crate::file_size_policy::HARD_MAX_FILE_SIZE_BYTES`]).
/// - `max_depth`: `None` (default) searches without a depth limit.
/// - `changed_paths` empty (default) runs a full index; non-empty scopes an
///   incremental update to those paths.
/// - `embedding_concurrency`: `None` (default) auto-sizes the embed pool.
#[derive(Default)]
pub struct ZvecGrepIndexOptions<'a> {
    /// Workspace root override; `None` (default) uses the bound root.
    pub root: Option<&'a std::path::Path>,
    /// Explicit root set; non-empty wins over `root` and stored paths.
    pub root_paths: Vec<RootPathSpec<'a>>,
    /// Force a full reindex even when the index is fresh.
    pub rebuild: bool,
    /// Replace stored root paths instead of keeping them.
    pub reset_paths: bool,
    /// Directory-prefix whitelist.
    pub include_paths: Vec<String>,
    /// Directory-prefix blacklist.
    pub exclude_paths: Vec<String>,
    /// Case-sensitive glob filters (`!` negates, last match wins).
    pub globs: Vec<String>,
    /// Case-insensitive glob filters (`--iglob` semantics).
    pub insensitive_globs: Vec<String>,
    /// Ripgrep file-type names to include (`--type`).
    pub file_types: Vec<String>,
    /// Ripgrep file-type names to exclude (`--type-not`).
    pub excluded_file_types: Vec<String>,
    /// Search hidden files; `None` (default) keeps the workspace default.
    pub hidden: Option<bool>,
    /// Ignore `.gitignore`/`.ignore` rules; `None` keeps the default.
    pub no_ignore: Option<bool>,
    /// Extra ignore files (`--ignore-file`).
    pub ignore_files: Vec<String>,
    /// Maximum directory depth below each searched path; `None` is unlimited.
    pub max_depth: Option<u32>,
    /// Per-file size cap override in bytes; `Some(0)` is rejected, oversized
    /// values clamp to 512 MiB.
    pub max_file_size_bytes: Option<u64>,
    /// Follow symlinks when `Some(true)`.
    pub follow: Option<bool>,
    /// Descend into nested git repositories when `Some(true)`.
    pub include_nested_git: Option<bool>,
    /// Embed parallelism; `None` (default) auto-sizes.
    pub embedding_concurrency: Option<usize>,
    /// Progress sink; `None` (default) reports nothing.
    pub on_progress: Option<IndexProgressSink>,
    /// Empty (default) runs a full index; non-empty scopes an incremental update.
    pub changed_paths: Vec<std::path::PathBuf>,
    /// Abort probe; `None` (default) runs to completion.
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

/// Options accepted by [`crate::service::facade::ZvecGrepService::context`].
///
/// Defaults: no query (the caller must supply one), `limit: None` (per-group
/// default of 10 for up to 3 groups, else the 30-item total budget split
/// across groups), `fuse: false`, `trace: false`, and `auto_update: true`
/// (a stale index is refreshed before searching).
///
/// Query rules: at least one non-blank primary (`query`/`queries`) or extra
/// route (`routes`/`fts`/`vector`) is required, else `CONTEXT.EMPTY_QUERY`;
/// a blank extra route query is rejected with `SERVICE.EMPTY_ROUTE_QUERY`.
/// Every query input is trimmed and blank primaries are dropped before the
/// check, so whitespace-only queries count as absent.
///
/// Time filters: `modified_after` later than `modified_before` is rejected
/// with `SEARCH_PLAN.INVALID_MODIFIED_TIME_RANGE`; negative epoch millis is
/// rejected with `SEARCH_PLAN.INVALID_MODIFIED_TIME_FILTER`.
///
/// `auto_update` defaults to `true`. Read sessions
/// ([`ReadSession::context`](crate::service::facade::ReadSession::context))
/// never refresh: the flag is ignored there (forced off) and the search runs
/// on the open handle.
pub struct ZvecGrepContextOptions<'a> {
    /// Workspace root override; `None` (default) uses the bound root.
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
    /// Per-group item cap; `None` (default) uses the 10-item default (or the
    /// split 30-item total budget past 3 groups). `Some(n)` wins verbatim.
    pub limit: Option<usize>,
    /// Attach per-item trace payloads.
    pub trace: bool,
    /// Entity id to track across ranking (diagnostics only, not a filter).
    pub track_entity_id: Option<EntityId>,
    /// Prefer symbol-defined entities when ranking.
    pub prefer_symbol: bool,
    /// Restrict symbol preference to these symbol types (empty = all).
    pub symbol_types: Vec<CodeSymbolType>,
    /// Directory-prefix whitelist.
    pub include_paths: Vec<String>,
    /// Directory-prefix blacklist.
    pub exclude_paths: Vec<String>,
    /// Case-sensitive glob filters (`!` negates, last match wins).
    pub globs: Vec<String>,
    /// Case-insensitive glob filters.
    pub insensitive_globs: Vec<String>,
    /// Ripgrep file-type names to include.
    pub file_types: Vec<String>,
    /// Ripgrep file-type names to exclude.
    pub excluded_file_types: Vec<String>,
    /// Only items modified at/after this timestamp; must not be later than
    /// `modified_before` (`SEARCH_PLAN.INVALID_MODIFIED_TIME_RANGE`).
    pub modified_after: Option<UnixMillis>,
    /// Only items modified at/before this timestamp.
    pub modified_before: Option<UnixMillis>,
    /// Exhaustive lexical path.
    pub rg: Option<RgOptions>,
    /// Refresh a stale index before searching (default true). Ignored
    /// (forced off) by read sessions, which search the open handle.
    pub auto_update: bool,
    /// Abort probe; `None` (default) runs to completion.
    pub signal: Option<AbortCheck>,
}

impl<'a> Default for ZvecGrepContextOptions<'a> {
    fn default() -> Self {
        Self {
            root: None,
            query: None,
            queries: Vec::new(),
            routes: Vec::new(),
            fts: Vec::new(),
            vector: Vec::new(),
            fuse: false,
            limit: None,
            trace: false,
            track_entity_id: None,
            prefer_symbol: false,
            symbol_types: Vec::new(),
            include_paths: Vec::new(),
            exclude_paths: Vec::new(),
            globs: Vec::new(),
            insensitive_globs: Vec::new(),
            file_types: Vec::new(),
            excluded_file_types: Vec::new(),
            modified_after: None,
            modified_before: None,
            rg: None,
            auto_update: true,
            signal: None,
        }
    }
}

impl ZvecGrepContextOptions<'_> {
    /// True unless the caller explicitly disabled auto-refresh.
    ///
    /// Read sessions ignore this flag: [`ReadSession::context`](crate::service::facade::ReadSession::context)
    /// never refreshes and searches the open handle instead.
    #[must_use]
    pub fn wants_auto_update(&self) -> bool {
        self.auto_update
    }
}

/// Options for the exhaustive lexical (`rg`) path.
///
/// All flags default off; `None` caps mean "no cap". The flags feed the
/// in-process lexical search (`fixedStrings` → literals, `ignoreCase` /
/// `smartCase` → case folding, `maxCount` → per-file match cap,
/// `contextLines` → lines before and after each match).
#[derive(Debug, Clone, Default)]
pub struct RgOptions {
    /// Exhaustive-path pattern override; `None` (default) adds no pattern.
    pub pattern: Option<String>,
    /// Case-insensitive matching (`--ignore-case`).
    pub case_insensitive: bool,
    /// Case-insensitive unless the pattern has an uppercase letter.
    pub smart_case: bool,
    /// Treat the pattern as a literal (`--fixed-strings`).
    pub fixed_strings: bool,
    /// Maximum matches per file; `None` (default) is unlimited.
    pub max_count: Option<usize>,
    /// Context lines before and after each match; `None` (default) is none.
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
#[serde(rename_all = "snake_case")]
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coverage_group: Option<String>,
}

/// Item provenance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextItemKind {
    IndexedEntity,
    /// Serializes as `lexical_match` per the TS wire contract.
    #[serde(rename = "lexical_match")]
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

/// Reference to the query group that matched an item (mirrors TS
/// `ZvecGrepContextQueryGroupMatch`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryGroupRef {
    pub id: String,
    pub query: String,
    pub role: GroupRole,
    pub rank: usize,
    pub matched_by: SearchMatchedBy,
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

/// Per-query-group search result (mirrors TS `groupResults`: per-group items,
/// not hits).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupResult {
    pub id: String,
    pub query: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<GroupRole>,
    pub items: Vec<ContextItem>,
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
#[must_use]
pub fn empty_query_error() -> crate::error::EngineError {
    crate::error::EngineError::new(
        crate::error::EngineErrorCode::ContextEmptyQuery,
        "query is required",
    )
}

/// Error used when a requested root has no workspace index.
#[must_use]
pub fn workspace_index_not_found(root: &str) -> crate::error::EngineError {
    crate::error::EngineError::new(
        crate::error::EngineErrorCode::ContextWorkspaceIndexNotFound,
        "workspace index not found",
    )
    .with_context(format!("root={root}"))
}

/// Error used when the workspace index exists but is disabled.
#[must_use]
pub fn workspace_index_disabled(root: &str) -> crate::error::EngineError {
    crate::error::EngineError::new(
        crate::error::EngineErrorCode::ContextWorkspaceIndexDisabled,
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

    #[test]
    fn context_options_default_enables_auto_update() {
        assert!(ZvecGrepContextOptions::default().wants_auto_update());
    }
}
