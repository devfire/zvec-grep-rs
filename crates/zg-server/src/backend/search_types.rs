//! Hybrid-search request and response types.
//!
//! Owned, borrow-free shapes that cross the actor boundary: the normalized
//! MCP search input ([`SearchQuery`]), its freshness contract
//! ([`SearchFreshness`] / [`ResultFreshness`]), and the daemon-computed
//! response ([`DaemonSearchResult`] with its [`SearchIndexing`] snapshot).

use zg_core::service::types::ZvecGrepContextResult;
use zg_core::types::{CodeSymbolType, SearchPlanRouteMode, UnixMillis};

/// Requested freshness: search the committed index now, or settle pending
/// index work first. Mirrors TS `"eventual" | "wait_for_fresh"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SearchFreshness {
    /// Search immediately; a background refresh may follow.
    #[default]
    Eventual,
    /// Wait for the active index to become fresh before searching.
    WaitForFresh,
}

/// Result freshness, mirroring TS `"fresh" | "possibly_stale"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResultFreshness {
    /// Index covers all known changes.
    Fresh,
    /// Known changes, watcher backlog, or an active job may postdate the index.
    #[default]
    PossiblyStale,
}

/// One supplemental retrieval route (mode + query), mirroring the TS
/// `{ mode: "fts" | "vector", query }` route shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchRoute {
    /// Retrieval mode for this group.
    pub mode: SearchRouteMode,
    /// Route query text.
    pub query: String,
}

/// Retrieval mode for one [`SearchRoute`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchRouteMode {
    /// Lexical route.
    Fts,
    /// Semantic/vector route.
    Vector,
}

impl SearchRouteMode {
    pub(crate) const fn plan_mode(self) -> SearchPlanRouteMode {
        match self {
            Self::Fts => SearchPlanRouteMode::Fts,
            Self::Vector => SearchPlanRouteMode::Vector,
        }
    }
}

/// Owned search request (no borrows: crosses the actor boundary). Carries
/// the full normalized MCP search input; index-scoped knobs
/// (`hidden`, `no_ignore`, `ignore_files`, `max_depth`,
/// `max_file_size_bytes`, `follow`, `embedding_concurrency`) are accepted
/// at the MCP boundary but refreshes reuse the index-time file scope
/// (see `docs/ts-divergence.md`).
#[derive(Debug, Clone, Default)]
pub struct SearchQuery {
    /// Primary natural-language query.
    pub query: Option<String>,
    /// Additional primary query groups.
    pub queries: Vec<String>,
    /// Fully-specified supplemental routes.
    pub routes: Vec<SearchRoute>,
    /// Collapse all groups into one ranked plan.
    pub fuse: bool,
    /// Result limit.
    pub limit: Option<usize>,
    /// Include per-hit trace payloads.
    pub trace: bool,
    /// Prefer exact indexed symbols when the query names a symbol.
    pub prefer_symbol: bool,
    /// Restrict indexed results to symbol types.
    pub symbol_types: Vec<CodeSymbolType>,
    /// Ordered case-sensitive glob rules.
    pub globs: Vec<String>,
    /// Ordered case-insensitive glob rules.
    pub insensitive_globs: Vec<String>,
    /// Ripgrep file type names to include.
    pub file_types: Vec<String>,
    /// Ripgrep file type names to exclude.
    pub excluded_file_types: Vec<String>,
    /// Only query files modified after this time.
    pub modified_after: Option<UnixMillis>,
    /// Only query files modified before this time.
    pub modified_before: Option<UnixMillis>,
    /// Requested freshness.
    pub freshness: SearchFreshness,
    /// A stale index may schedule a background refresh.
    pub auto_update: bool,
}

/// Compact background-indexing snapshot attached to possibly-stale
/// results, mirroring TS `ZvecGrepSearchIndexing`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SearchIndexing {
    /// Current background indexing state.
    pub state: BackgroundIndexState,
    /// Up-to-date indexed files in scope, when known.
    pub completed: Option<usize>,
    /// Total files in scope, when known.
    pub total: Option<usize>,
}

/// Background indexing state, mirroring TS
/// `"idle" | "queued" | "running" | "failed" | "cancelled"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BackgroundIndexState {
    /// No live job (or the latest job succeeded).
    Idle,
    /// A job is queued.
    Queued,
    /// A job is running.
    Running,
    /// The latest job failed.
    Failed,
    /// The latest job was cancelled.
    Cancelled,
}

impl BackgroundIndexState {
    pub(crate) fn of(job: Option<&crate::job_scheduler::IndexJobSnapshot>) -> Self {
        use zg_core::index_status::IndexJobState;
        match job.map(|job| job.state) {
            None | Some(IndexJobState::Succeeded) => Self::Idle,
            Some(IndexJobState::Queued) => Self::Queued,
            Some(IndexJobState::Running) => Self::Running,
            Some(IndexJobState::Failed) => Self::Failed,
            Some(IndexJobState::Cancelled) => Self::Cancelled,
        }
    }
}

/// Search response: the context result plus daemon-computed freshness.
#[derive(Debug, Clone)]
pub struct DaemonSearchResult {
    /// Hybrid search result over the committed index.
    pub result: ZvecGrepContextResult,
    /// Whether the index covers all known changes.
    pub freshness: ResultFreshness,
    /// Background refresh snapshot, present when possibly stale.
    pub indexing: Option<SearchIndexing>,
}
