//! Daemon backend: one actor task per root over a shared scheduler and pool.
//!
//! Mirrors `../zvec-grep/src/daemon/backend.ts` (`DaemonBackend`: search /
//! index / index-drop / rg / index-status / server-status). The TS class
//! holds thirteen fields of plain `Map`s mutated across `await` points —
//! safe only on the single-threaded event loop. The Rust shape is
//! deliberately different (see `docs/ts-divergence.md`):
//!
//! - Each root is owned by one actor task holding `RootRuntime`, its
//!   `IndexCoordinator`, its `WatchManager`, and its read-session cache as
//!   plain `&mut` state. `DaemonBackend` holds only the
//!   [`RuntimeManager`] (key → sender + join handle).
//! - Promise-chain generation indexing becomes sequential message
//!   processing; [`Generation`](crate::root_runtime::Generation) survives
//!   as a staleness newtype, not a concurrency mechanism.
//! - `droppingRoots` / `shuttingDown` are control-flow states, and
//!   `closePromise` is [`RuntimeManager::close`] awaiting actor joins plus
//!   [`JobScheduler::close`](crate::job_scheduler::JobScheduler::close)
//!   awaiting in-flight blocking work (M6).
//! - Searches during an index read the last committed session instead of
//!   the TS writer context (documented staleness trade-off).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use zg_core::authorization::operation::{
    RemoteEmbeddingAuthorizationManager, with_remote_embedding_operation_permit,
};
use zg_core::authorization::planner::{
    PlanIndexInput, PlanSearchInput, plan_remote_index_authorization,
    plan_remote_search_authorization,
};
use zg_core::authorization::types::RemoteEmbeddingPermit;
use zg_core::error::{EngineError, EngineResult};
use zg_core::index_status::{
    IndexCompletion, IndexJobState, index_completion_for_job, index_completion_from_status,
};
use zg_core::lexical::{LexicalSearchOptions, LexicalSearchResult, run_lexical_search};
use zg_core::models::catalog::{EMBEDDING_MODEL_CATALOG, EmbeddingCatalogEntry, ModelReference};
use zg_core::models::embeddings::{CreateEmbeddingModelOptions, DeviceKind};
use zg_core::models::error::ModelError;
use zg_core::models::{EmbeddingModel, EmbeddingModelInfo};
use zg_core::service::facade::{CreateZvecGrepOptions, ReadSession, ZvecGrepService};
use zg_core::service::types::{
    ZvecGrepContextOptions, ZvecGrepContextResult, ZvecGrepIndexOptions, ZvecGrepInfoResult,
    workspace_index_not_found,
};
use zg_core::types::{
    CodeSymbolType, FileScanDiagnostics, IndexResult, SearchPlanRoute, SearchPlanRouteMode,
    UnixMillis, WorkspaceIndexStatus,
};

use crate::change_set::ChangeSetSnapshot;
use crate::errors::DaemonError;
use crate::index_coordinator::{CoordinatorReason, IndexCoordinator, TakePending};
use crate::job_scheduler::{
    IndexJobSnapshot, JobFailure, JobOutcome, JobReason, JobRun, JobScheduler, JobSchedulerOptions,
    SubmitIndexJobResult, bridge_cancellation,
};
use crate::logger::{DaemonLogger, LogField};
use crate::model_pool::{
    AcquireError, EmbeddingModelPool, EmbeddingModelPoolOptions, ModelLease, ModelLoadRequest,
};
use crate::read_session_cache::{
    ClosableHandle, OpenSession, SessionError, WorkspaceReadSessionCache,
};
use crate::root_runtime::{Generation, RootKey, RootRuntime, resolve_requested_root};
use crate::runtime_manager::{RuntimeManager, send_command};
use crate::watch_manager::{WatchManager, WatchManagerOptions, WatchReason};

/// Service-level options shared by every root actor.
#[derive(Clone, Default)]
pub struct ServiceConfig {
    /// Explicit embedding reference; wins over the manifest entry.
    pub embedding: Option<ModelReference>,
    /// API key for remote providers.
    pub api_key: Option<String>,
    /// Endpoint override for remote providers.
    pub endpoint: Option<String>,
    /// Local model cache directory override.
    pub model_cache_dir: Option<PathBuf>,
    /// Injected model handle (tests, pool bypass). Wins over everything.
    pub model_override: Option<Arc<dyn EmbeddingModel>>,
}

impl ServiceConfig {
    /// Facade options bound to `root` with an explicit model handle.
    pub fn facade_options(
        &self,
        root: &str,
        model: Option<Arc<dyn EmbeddingModel>>,
    ) -> CreateZvecGrepOptions {
        CreateZvecGrepOptions {
            root: Some(PathBuf::from(root)),
            embedding: self.embedding.clone(),
            embedding_model: model.or_else(|| self.model_override.clone()),
            api_key: self.api_key.clone(),
            endpoint: self.endpoint.clone(),
            model_cache_dir: self.model_cache_dir.clone(),
        }
    }

    /// Model construction options for pool loads.
    pub fn load_options(&self) -> CreateEmbeddingModelOptions {
        CreateEmbeddingModelOptions {
            api_key: self.api_key.clone(),
            endpoint: self.endpoint.clone(),
            model_cache_dir: self.model_cache_dir.clone(),
            device: DeviceKind::Auto,
        }
    }
}

/// State shared by the backend, the manager, and every root actor.
#[derive(Clone)]
pub struct BackendShared {
    /// Shared index job scheduler.
    pub scheduler: JobScheduler,
    /// Shared model pool (owns the M6 embed gate).
    pub pool: EmbeddingModelPool,
    /// Service options for facade construction.
    pub service: ServiceConfig,
    /// Remote-embedding grant manager.
    pub auth: Arc<RemoteEmbeddingAuthorizationManager>,
    /// Daemon logger.
    pub logger: Option<DaemonLogger>,
    /// Read-session idle TTL.
    pub read_session_ttl: Duration,
    /// Quiet-actor idle TTL.
    pub runtime_idle_ttl: Duration,
}

impl BackendShared {
    /// Facade options for `root` with no explicit model (catalog path).
    pub fn catalog_options(&self, root: &str) -> CreateZvecGrepOptions {
        self.service.facade_options(root, None)
    }
}

/// Backend failure: typed daemon and engine errors converge here without
/// merging their wire codes (M1) — [`BackendError::code`] returns the raw
/// string from either side.
#[derive(Debug)]
pub enum BackendError {
    /// Daemon-layer failure (bare wire code).
    Daemon(DaemonError),
    /// Engine-layer failure (`ZVEC_GREP.ENGINE.*` code).
    Engine(EngineError),
}

impl BackendError {
    /// Raw wire code from either side.
    pub fn code(&self) -> String {
        match self {
            Self::Daemon(error) => error.code().to_owned(),
            Self::Engine(error) => error.code().to_string(),
        }
    }
}

impl std::fmt::Display for BackendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Daemon(error) => write!(f, "{error}"),
            Self::Engine(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for BackendError {}

impl From<DaemonError> for BackendError {
    fn from(error: DaemonError) -> Self {
        Self::Daemon(error)
    }
}

impl From<EngineError> for BackendError {
    fn from(error: EngineError) -> Self {
        Self::Engine(error)
    }
}

impl From<SessionError> for BackendError {
    fn from(error: SessionError) -> Self {
        match error {
            SessionError::Closed => Self::Daemon(DaemonError::ShuttingDown),
            SessionError::Open(error) => Self::Engine(error),
        }
    }
}

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
    const fn plan_mode(self) -> SearchPlanRouteMode {
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
    fn of(job: Option<&IndexJobSnapshot>) -> Self {
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

/// Owned lexical request (no borrows: crosses the actor boundary).
#[derive(Debug, Clone, Default)]
pub struct RgQuery {
    /// Regex (or literal) alternatives.
    pub patterns: Vec<String>,
    /// Search paths relative to the root (empty searches everything).
    pub paths: Vec<String>,
    /// Maximum matches collected.
    pub limit: Option<usize>,
    /// Treat patterns as literals.
    pub fixed_strings: bool,
    /// Case-insensitive matching.
    pub ignore_case: bool,
    /// Case-insensitive when every pattern is lowercase.
    pub smart_case: bool,
    /// Wrap patterns with word boundaries.
    pub word_regexp: bool,
    /// Maximum matches per file.
    pub max_count: Option<usize>,
    /// Pattern files: every non-empty line is one more pattern.
    pub pattern_files: Vec<String>,
    /// Case-sensitive glob filters.
    pub globs: Vec<String>,
    /// Case-insensitive glob filters.
    pub insensitive_globs: Vec<String>,
    /// Ripgrep file-type names to include.
    pub file_types: Vec<String>,
    /// Ripgrep file-type names to exclude.
    pub excluded_file_types: Vec<String>,
    /// Search hidden files.
    pub hidden: bool,
    /// Ignore ignore-files.
    pub no_ignore: bool,
    /// Extra ignore files.
    pub ignore_files: Vec<String>,
    /// Maximum directory depth.
    pub max_depth: Option<usize>,
    /// Skip files larger than this.
    pub max_file_size_bytes: Option<u64>,
    /// Context lines before each match.
    pub before_context: usize,
    /// Context lines after each match.
    pub after_context: usize,
}

/// Owned index request.
#[derive(Debug, Clone, Default)]
pub struct IndexInput {
    /// Rebuild from scratch.
    pub rebuild: bool,
    /// Changed paths for an incremental run (empty reconciles fully).
    pub changed_paths: Vec<PathBuf>,
}

/// Index status with its live job overlay.
#[derive(Debug, Clone)]
pub struct DaemonIndexStatus {
    /// Persisted status (cached after each finished run).
    pub status: WorkspaceIndexStatus,
    /// Latest job for the root, if any.
    pub job: Option<IndexJobSnapshot>,
    /// Completion counters with live progress overlaid while running.
    pub completion: Option<IndexCompletion>,
    /// Skipped-file diagnostics from the latest finished run, if any.
    pub scan_diagnostics: Option<FileScanDiagnostics>,
    /// Workspace info (policy, embedding, manifest); `None` when the info
    /// read fails (e.g. a disabled index) while status stays available.
    pub info: Option<ZvecGrepInfoResult>,
    /// Dirty revision counter at read time.
    pub dirty_revision: Generation,
    /// Newest indexed revision counter at read time.
    pub indexed_revision: Generation,
    /// Whether the root's filesystem watcher is active.
    pub watcher_active: bool,
}

/// Daemon liveness snapshot.
#[derive(Debug, Clone)]
pub struct DaemonServerStatus {
    /// Backend creation time, unix millis.
    pub started_at_ms: u64,
    /// Live root actors.
    pub runtimes: usize,
    /// Scheduler queue depth.
    pub queued_jobs: usize,
    /// Running jobs.
    pub running_jobs: usize,
    /// Resident models and active leases.
    pub pool_loaded: usize,
    /// Active model leases.
    pub pool_leases: usize,
}

/// Handle to a live root actor: its key plus its command sender. The
/// sender stays crate-visible: external layers command actors through
/// [`DaemonBackend`], never directly.
#[derive(Clone)]
pub struct RootHandle {
    /// Canonical root the actor owns.
    pub key: RootKey,
    /// Actor command sender.
    pub(crate) tx: UnboundedSender<RootCommand>,
}

/// Commands one root actor processes sequentially.
pub(crate) enum RootCommand {
    /// Hybrid search through the read-session cache.
    Search {
        /// Owned query.
        query: SearchQuery,
        /// Search result with daemon-computed freshness.
        reply: oneshot::Sender<Result<DaemonSearchResult, BackendError>>,
    },
    /// Index (or reindex) through the scheduler.
    Index {
        /// Owned index input.
        input: IndexInput,
        /// Submitted job snapshot plus whether a live job was reused.
        reply: oneshot::Sender<Result<SubmitIndexJobResult, BackendError>>,
    },
    /// Cached-or-read status with live job overlay.
    Status {
        /// Status plus overlay.
        reply: oneshot::Sender<Result<DaemonIndexStatus, BackendError>>,
    },
    /// In-process lexical search.
    Rg {
        /// Owned lexical query.
        query: RgQuery,
        /// Lexical result.
        reply: oneshot::Sender<Result<LexicalSearchResult, BackendError>>,
    },
    /// Cancels jobs, closes handles, deletes storage.
    Drop {
        /// True when storage existed.
        reply: oneshot::Sender<Result<bool, BackendError>>,
    },
    /// Watcher batch from the watch manager.
    WatchBatch {
        /// Flushed changes.
        changes: ChangeSetSnapshot,
        /// Flush reason.
        reason: WatchReason,
    },
    /// A scheduler index run finished (sent by the run closure).
    IndexFinished {
        /// Outcome with its target revision, boxed: the status payload
        /// dwarfs every other variant (M3).
        finished: Box<FinishedIndex>,
    },
    /// Stop the actor and unregister.
    Shutdown,
}

/// Outcome of one index run, stamped with its target revision.
#[derive(Debug)]
pub(crate) struct FinishedIndex {
    /// Revision the run reconciled toward.
    revision: Generation,
    /// Whether the run reconciled fully.
    force_full: bool,
    /// Fresh status on success; `None` keeps the previous cache.
    ok: Option<FinishedOk>,
}

/// Successful run payload.
#[derive(Debug)]
struct FinishedOk {
    /// Raw index counters (refreshes scan diagnostics).
    index_result: IndexResult,
    /// Freshly read status.
    status: WorkspaceIndexStatus,
}

/// Cached read session: the facade guard, the model identity behind it
/// (for permit planning), and the pool lease that keeps the model
/// resident. The session closes before the lease releases (declaration
/// order).
struct CachedSession {
    /// Open read guard.
    session: ReadSession,
    /// Static identity of the backing model.
    model_info: EmbeddingModelInfo,
    /// Model lease backing the session (unused beyond ownership).
    _lease: Option<ModelLease>,
}

impl CachedSession {
    /// Static identity of the backing model.
    fn model_info(&self) -> &EmbeddingModelInfo {
        &self.model_info
    }

    /// Open read guard.
    fn session(&self) -> &ReadSession {
        &self.session
    }
}

#[async_trait::async_trait]
impl ClosableHandle for CachedSession {
    async fn close(self) {
        self.session.close();
    }
}

/// Options for [`DaemonBackend`].
#[derive(Clone, Default)]
pub struct DaemonBackendOptions {
    /// Service options (embedding selection, credentials, test override).
    pub service: ServiceConfig,
    /// Scheduler tuning.
    pub scheduler: JobSchedulerOptions,
    /// Pool tuning.
    pub pool: EmbeddingModelPoolOptions,
    /// Grant manager; defaults to a fresh manager.
    pub auth: Option<Arc<RemoteEmbeddingAuthorizationManager>>,
    /// Daemon logger.
    pub logger: Option<DaemonLogger>,
    /// Read-session idle TTL.
    pub read_session_ttl: Option<Duration>,
    /// Quiet-actor idle TTL.
    pub runtime_idle_ttl: Option<Duration>,
}

/// Daemon command surface over per-root actors.
#[derive(Clone)]
pub struct DaemonBackend {
    manager: RuntimeManager,
    scheduler: JobScheduler,
    pool: EmbeddingModelPool,
    started_at_ms: u64,
}

impl DaemonBackend {
    /// Builds the backend: shared scheduler, pool, and actor registry.
    /// No actors spawn until the first command.
    pub fn new(options: DaemonBackendOptions) -> Self {
        let scheduler = JobScheduler::new(options.scheduler);
        let pool = EmbeddingModelPool::new(options.pool);
        let shared = BackendShared {
            scheduler: scheduler.clone(),
            pool: pool.clone(),
            service: options.service,
            auth: options.auth.unwrap_or_default(),
            logger: options.logger,
            read_session_ttl: options
                .read_session_ttl
                .unwrap_or(crate::read_session_cache::DEFAULT_READ_SESSION_IDLE_TTL),
            runtime_idle_ttl: options
                .runtime_idle_ttl
                .unwrap_or(crate::runtime_manager::DEFAULT_RUNTIME_IDLE_TTL),
        };
        let manager = RuntimeManager::new(shared);
        Self {
            manager,
            scheduler,
            pool,
            started_at_ms: now_ms(),
        }
    }

    /// Shared scheduler (waiting on jobs, load reporting).
    pub fn scheduler(&self) -> &JobScheduler {
        &self.scheduler
    }

    /// Shared model pool.
    pub fn pool(&self) -> &EmbeddingModelPool {
        &self.pool
    }

    /// Hybrid search over an indexed root.
    pub async fn search(
        &self,
        root: &str,
        query: SearchQuery,
    ) -> Result<DaemonSearchResult, BackendError> {
        let handle = self.manager.activate_for_search(root).await?;
        send_recv(&handle, |reply| RootCommand::Search { query, reply }).await?
    }

    /// Submits an index job for a root.
    pub async fn index(
        &self,
        root: &str,
        input: IndexInput,
    ) -> Result<SubmitIndexJobResult, BackendError> {
        let handle = self.manager.activate_for_index(root)?;
        send_recv(&handle, |reply| RootCommand::Index { input, reply }).await?
    }

    /// Deletes a root's index storage and stops its actor.
    pub async fn index_drop(&self, root: &str) -> Result<bool, BackendError> {
        let key = resolve_requested_root(root, true)?;
        if let Some(handle) = self.manager.get(&key) {
            let dropped: bool = send_recv(&handle, |reply| RootCommand::Drop { reply }).await??;
            self.manager.unregister(&key);
            return Ok(dropped);
        }
        // Never-activated roots drop straight through the facade: no model
        // is needed to delete a manifest and storage.
        let service =
            ZvecGrepService::new(ServiceConfig::default().facade_options(key.as_str(), None));
        let root_buf = PathBuf::from(key.as_str());
        tokio::task::spawn_blocking(move || service.drop_index(Some(root_buf.as_path())))
            .await
            .map_err(join_backend_error)?
            .map_err(BackendError::Engine)
    }

    /// In-process lexical search over a root.
    pub async fn rg_search(
        &self,
        root: &str,
        query: RgQuery,
    ) -> Result<LexicalSearchResult, BackendError> {
        let handle = self.manager.activate_for_index(root)?;
        send_recv(&handle, |reply| RootCommand::Rg { query, reply }).await?
    }

    /// Index status with live job overlay.
    pub async fn index_status(&self, root: &str) -> Result<DaemonIndexStatus, BackendError> {
        let handle = self.manager.activate_for_index(root)?;
        send_recv(&handle, |reply| RootCommand::Status { reply }).await?
    }

    /// Daemon liveness snapshot.
    pub fn server_status(&self) -> DaemonServerStatus {
        let load = self.scheduler.load();
        let pool = self.pool.snapshot();
        DaemonServerStatus {
            started_at_ms: self.started_at_ms,
            runtimes: self.manager.actor_count(),
            queued_jobs: load.queued,
            running_jobs: load.running,
            pool_loaded: pool.loaded,
            pool_leases: pool.active_leases,
        }
    }

    /// Stops every actor, then awaits in-flight work (M6) and unloads models.
    pub async fn close(&self) {
        self.manager.close().await;
    }
}

/// Sends one command and awaits its reply. A dead actor (dropped reply
/// half) means the root is gone: surfaces as a shutdown error.
async fn send_recv<T>(
    handle: &RootHandle,
    make: impl FnOnce(oneshot::Sender<T>) -> RootCommand,
) -> Result<T, BackendError> {
    let (tx, rx) = oneshot::channel();
    send_command(&handle.tx, make(tx))?;
    rx.await
        .map_err(|_| BackendError::Daemon(DaemonError::ShuttingDown))
}

/// Spawns one root actor task. Called by the manager with a fresh channel;
/// `tx` is shared back into the watcher callbacks so flushed batches
/// re-enter as [`RootCommand::WatchBatch`].
pub(crate) async fn spawn_root_actor(
    shared: BackendShared,
    manager: RuntimeManager,
    key: RootKey,
    tx: UnboundedSender<RootCommand>,
    rx: UnboundedReceiver<RootCommand>,
) {
    let coordinator = IndexCoordinator::new(key.as_str(), None);
    let opener = session_opener(shared.clone(), key.clone());
    let sessions = WorkspaceReadSessionCache::new(opener, Some(shared.read_session_ttl));
    let batch_sender = tx.clone();
    let on_changes = Arc::new(move |snapshot: ChangeSetSnapshot, reason: WatchReason| {
        let _ = batch_sender.send(RootCommand::WatchBatch {
            changes: snapshot,
            reason,
        });
    });
    let service = shared.service.clone();
    let root = key.to_string();
    let get_roots = Arc::new(move || {
        let svc = ZvecGrepService::new(service.facade_options(&root, None));
        svc.workspace_info(None)
            .ok()
            .and_then(|info| info.workspace_index)
            .map(|index| index.root_paths)
            .unwrap_or_default()
    });
    let mut watcher = WatchManager::new(WatchManagerOptions {
        root: key.to_string(),
        debounce: None,
        max_wait: None,
        max_changed_paths: None,
        on_changes,
        get_root_paths: Some(get_roots),
        on_pending: None,
    });
    if let Err(error) = watcher.start() {
        log_event(
            &shared.logger,
            "watch.start_failed",
            [
                ("root_id", LogField::from(key.to_string())),
                ("error_code", LogField::from(error.code().to_owned())),
            ],
        );
    }
    let mut actor = RootActor {
        key: key.clone(),
        manager,
        runtime: RootRuntime::new(key),
        coordinator,
        sessions,
        watcher,
        status: None,
        scan: None,
        rx,
        tx,
        shared,
    };
    actor.run().await;
}

/// Session opener: resolves the manifest model (or the override) and
/// checks out a lease the session keeps alive.
fn session_opener(shared: BackendShared, key: RootKey) -> OpenSession<CachedSession> {
    Arc::new(move || {
        let shared = shared.clone();
        let key = key.clone();
        Box::pin(async move {
            if let Some(model) = shared.service.model_override.clone() {
                let service = ZvecGrepService::new(
                    shared
                        .service
                        .facade_options(key.as_str(), Some(model.clone())),
                );
                let session = service
                    .open_read_session(None)
                    .map_err(SessionError::Open)?;
                return Ok(CachedSession {
                    session,
                    model_info: model.info().clone(),
                    _lease: None,
                });
            }
            let request = load_request_for(&shared.service, &key).map_err(SessionError::Open)?;
            let lease = shared
                .pool
                .acquire(&request)
                .await
                .map_err(|error| match error {
                    AcquireError::Closed => SessionError::Closed,
                    AcquireError::Load { error, .. } => SessionError::Open(error),
                })?;
            let model_info = lease.model().info().clone();
            let service = ZvecGrepService::new(
                shared
                    .service
                    .facade_options(key.as_str(), Some(lease.model().clone())),
            );
            let session = service
                .open_read_session(None)
                .map_err(SessionError::Open)?;
            Ok(CachedSession {
                session,
                model_info,
                _lease: Some(lease),
            })
        }) as BoxFuture<'static, Result<CachedSession, SessionError>>
    })
}

/// Resolves the manifest embedding schema to a pool load request.
fn load_request_for(service: &ServiceConfig, key: &RootKey) -> EngineResult<ModelLoadRequest> {
    if let Some(reference) = &service.embedding {
        return Ok(ModelLoadRequest {
            reference: reference.clone(),
            options: service.load_options(),
        });
    }
    let facade = ZvecGrepService::new(service.facade_options(key.as_str(), None));
    let info = facade.workspace_info(None)?;
    let schema = info
        .workspace_index
        .as_ref()
        .and_then(|index| index.embedding.clone())
        .flatten()
        .ok_or_else(|| workspace_index_not_found(key.as_str()))?;
    let reference =
        catalog_reference_for_schema(&schema.provider, &schema.model).ok_or_else(|| {
            EngineError::from(ModelError::CatalogModelNotFound {
                reference: format!("{}/{}", schema.provider, schema.model),
            })
        })?;
    Ok(ModelLoadRequest {
        reference,
        options: service.load_options(),
    })
}

/// Finds the catalog reference for a manifest embedding schema.
fn catalog_reference_for_schema(provider: &str, model: &str) -> Option<ModelReference> {
    EMBEDDING_MODEL_CATALOG.iter().find_map(|entry| {
        let (entry_provider, entry_model, reference) = match entry {
            EmbeddingCatalogEntry::LlamaCpp(entry) => {
                (entry.provider, entry.model, entry.reference)
            }
            EmbeddingCatalogEntry::QwenText(entry) => {
                (entry.provider, entry.model, entry.reference)
            }
            EmbeddingCatalogEntry::QwenMultimodal(entry) => {
                (entry.provider, entry.model, entry.reference)
            }
            EmbeddingCatalogEntry::TransformersJs(entry) => {
                (entry.provider, entry.model, entry.reference)
            }
            EmbeddingCatalogEntry::Model2Vec(entry) => {
                (entry.provider, entry.model, entry.reference)
            }
        };
        (entry_provider == provider && entry_model == model).then(|| ModelReference::new(reference))
    })
}

struct RootActor {
    key: RootKey,
    shared: BackendShared,
    manager: RuntimeManager,
    runtime: RootRuntime,
    coordinator: IndexCoordinator,
    sessions: WorkspaceReadSessionCache<CachedSession>,
    watcher: WatchManager,
    status: Option<WorkspaceIndexStatus>,
    scan: Option<FileScanDiagnostics>,
    rx: UnboundedReceiver<RootCommand>,
    tx: UnboundedSender<RootCommand>,
}

impl RootActor {
    async fn run(&mut self) {
        let mut idle = Box::pin(tokio::time::sleep(self.shared.runtime_idle_ttl));
        loop {
            tokio::select! {
                biased;
                Some(cmd) = self.rx.recv() => {
                    idle.as_mut().reset(tokio::time::Instant::now() + self.shared.runtime_idle_ttl);
                    if matches!(cmd, RootCommand::Shutdown) {
                        break;
                    }
                    // A completed drop owns no state worth keeping: exit so
                    // the manager can reclaim the task.
                    if !self.handle(cmd).await {
                        break;
                    }
                }
                () = &mut idle => {
                    if self.runtime.is_quiet() && !self.runtime.needs_reconciliation() {
                        break;
                    }
                    idle.as_mut().reset(tokio::time::Instant::now() + self.shared.runtime_idle_ttl);
                }
            }
        }
        self.teardown().await;
    }

    /// Handles one command. Returns false when the actor should exit
    /// (after a completed drop).
    async fn handle(&mut self, cmd: RootCommand) -> bool {
        match cmd {
            RootCommand::Search { query, reply } => {
                let _ = reply.send(self.handle_search(query).await);
            }
            RootCommand::Index { input, reply } => {
                let _ = reply.send(self.handle_index(input));
            }
            RootCommand::Status { reply } => {
                let _ = reply.send(self.handle_status());
            }
            RootCommand::Rg { query, reply } => {
                let _ = reply.send(self.handle_rg(query).await);
            }
            RootCommand::Drop { reply } => {
                let _ = reply.send(self.handle_drop().await);
                return false;
            }
            RootCommand::WatchBatch { changes, reason } => {
                self.handle_watch_batch(changes, reason);
            }
            RootCommand::IndexFinished { finished } => {
                self.apply_finished(*finished);
            }
            RootCommand::Shutdown => {}
        }
        true
    }

    async fn teardown(&mut self) {
        self.runtime.close();
        self.watcher.close().await;
        self.sessions.close().await;
        self.manager.unregister(&self.key);
    }

    fn temp_service(&self) -> ZvecGrepService {
        ZvecGrepService::new(self.shared.catalog_options(self.key.as_str()))
    }

    async fn handle_search(
        &mut self,
        query: SearchQuery,
    ) -> Result<DaemonSearchResult, BackendError> {
        self.runtime.begin_operation();
        let result = self.search_inner(query).await;
        self.runtime.end_operation();
        result
    }

    async fn search_inner(
        &mut self,
        query: SearchQuery,
    ) -> Result<DaemonSearchResult, BackendError> {
        if query.freshness == SearchFreshness::WaitForFresh {
            self.wait_for_fresh().await?;
        }
        let result = self.execute_search(&query).await?;
        if query.auto_update {
            // Best effort: a failed background submit never fails a search.
            let _ = self.background_job();
        }
        Ok(self.finish_search(result))
    }

    async fn execute_search(
        &mut self,
        query: &SearchQuery,
    ) -> Result<ZvecGrepContextResult, BackendError> {
        let info = self.temp_service().workspace_info(None)?;
        let options = ZvecGrepContextOptions {
            query: query.query.clone(),
            queries: query.queries.clone(),
            routes: query
                .routes
                .iter()
                .map(|route| SearchPlanRoute {
                    mode: route.mode.plan_mode(),
                    query: route.query.clone(),
                })
                .collect(),
            fuse: query.fuse,
            limit: query.limit,
            trace: query.trace,
            prefer_symbol: query.prefer_symbol,
            symbol_types: query.symbol_types.clone(),
            globs: query.globs.clone(),
            insensitive_globs: query.insensitive_globs.clone(),
            file_types: query.file_types.clone(),
            excluded_file_types: query.excluded_file_types.clone(),
            modified_after: query.modified_after,
            modified_before: query.modified_before,
            // The daemon owns refresh through the scheduler; the facade
            // must not reindex inline inside a read session (its
            // read-session guard forces this off as well).
            auto_update: false,
            ..ZvecGrepContextOptions::default()
        };
        let auth = Arc::clone(&self.shared.auth);
        let needs_reconcile = self.runtime.needs_reconciliation();
        self.sessions
            .with_read(|cached| {
                let permit =
                    plan_search_permit(&auth, &info, cached.model_info(), needs_reconcile)?;
                Ok(with_remote_embedding_operation_permit(permit, || {
                    cached.session().context(&options)
                })?)
            })
            .await?
    }

    /// Waits for the active index to become fresh: settles the live job,
    /// triggers a reconcile when stale work is waiting, and re-checks.
    /// Bounded so watcher churn during the wait cannot loop forever.
    async fn wait_for_fresh(&mut self) -> Result<(), BackendError> {
        for _ in 0..3 {
            if !self.runtime.needs_reconciliation() {
                return Ok(());
            }
            let Some(job) = self.background_job() else {
                return Ok(());
            };
            if job.is_terminal() {
                return Self::check_terminal_job(&job, self.runtime.needs_reconciliation());
            }
            let snapshot = self.shared.scheduler.wait(&job.id, None).await?;
            Self::check_terminal_job(&snapshot, self.runtime.needs_reconciliation())?;
        }
        Ok(())
    }

    fn check_terminal_job(job: &IndexJobSnapshot, still_dirty: bool) -> Result<(), BackendError> {
        if !still_dirty {
            return Ok(());
        }
        match job.state {
            IndexJobState::Failed | IndexJobState::Cancelled => {
                let message = job
                    .error
                    .as_ref()
                    .map(|error| error.message.clone())
                    .unwrap_or_else(|| "index job did not complete".to_owned());
                Err(DaemonError::IndexFailed { message }.into())
            }
            IndexJobState::Queued | IndexJobState::Running | IndexJobState::Succeeded => Ok(()),
        }
    }

    /// Submits a background reconcile when stale work is waiting and no
    /// job is active, mirroring TS `background_reconcile`. Returns the
    /// live job to wait on, if any. Never enqueues when nothing is
    /// pending: the take would be empty and the dirty-revision bump
    /// would buy nothing.
    fn background_job(&mut self) -> Option<IndexJobSnapshot> {
        if self.runtime.needs_reconciliation()
            && !self.shared.scheduler.has_active_root(self.key.as_str())
        {
            let full = self.runtime.requires_full_reconciliation();
            if full || self.coordinator.has_pending() {
                let changes = ChangeSetSnapshot {
                    force_full_reconcile: full,
                    ..ChangeSetSnapshot::default()
                };
                let tx = self.tx.clone();
                let key = self.key.clone();
                let shared = self.shared.clone();
                if let Ok(submitted) = self.coordinator.enqueue(
                    &changes,
                    CoordinatorReason::BackgroundReconcile,
                    &mut self.runtime,
                    &self.shared.scheduler,
                    |take| build_index_run(tx, key, shared, take),
                ) {
                    self.runtime.set_writer_pending(true);
                    return Some(submitted.job);
                }
            }
        }
        self.shared.scheduler.get_by_root(self.key.as_str())
    }

    /// Attaches daemon-computed freshness to a search result, mirroring
    /// the TS `fresh` / `possibly_stale` derivation.
    fn finish_search(&mut self, result: ZvecGrepContextResult) -> DaemonSearchResult {
        let snapshot = self.runtime.snapshot();
        let job = self.shared.scheduler.get_by_root(self.key.as_str());
        let active_known_change = matches!(&job,
            Some(job) if job.reason != JobReason::BackgroundReconcile && !job.is_terminal());
        let freshness = if self.runtime.needs_reconciliation()
            || snapshot.watcher_pending
            || active_known_change
        {
            ResultFreshness::PossiblyStale
        } else {
            ResultFreshness::Fresh
        };
        let indexing = if freshness == ResultFreshness::PossiblyStale {
            Some(self.search_indexing(job.as_ref()))
        } else {
            None
        };
        DaemonSearchResult {
            result,
            freshness,
            indexing,
        }
    }

    /// Compact background-indexing snapshot for possibly-stale results.
    fn search_indexing(&self, job: Option<&IndexJobSnapshot>) -> SearchIndexing {
        let overlaid = index_completion_for_job(
            index_completion_from_status(self.status.as_ref()),
            job.map(|job| job.state),
            job.and_then(|job| job.progress.as_ref()),
        );
        SearchIndexing {
            state: BackgroundIndexState::of(job),
            completed: overlaid.as_ref().map(|completion| completion.completed),
            total: overlaid.as_ref().map(|completion| completion.total),
        }
    }

    fn handle_index(&mut self, input: IndexInput) -> Result<SubmitIndexJobResult, BackendError> {
        self.runtime.set_writer_pending(true);
        // Explicit rebuilds reconcile fully. An incremental request with no
        // paths reconciles whatever is pending: the run takes the pending
        // snapshot lazily, and an empty take completes as a fresh no-op
        // (see `apply_finished`) instead of rescanning the workspace —
        // except on a root with no index yet, where the first build must
        // scan everything (mirrors the engine's create-on-missing path).
        let changes = if input.rebuild {
            ChangeSetSnapshot {
                force_full_reconcile: true,
                ..ChangeSetSnapshot::default()
            }
        } else if input.changed_paths.is_empty() {
            let indexed = self
                .temp_service()
                .workspace_info(None)
                .map(|info| info.indexed)
                .unwrap_or(false);
            ChangeSetSnapshot {
                force_full_reconcile: !indexed,
                ..ChangeSetSnapshot::default()
            }
        } else {
            ChangeSetSnapshot {
                touched_files: input
                    .changed_paths
                    .iter()
                    .map(|path| path.to_string_lossy().into_owned())
                    .collect(),
                ..ChangeSetSnapshot::default()
            }
        };
        let tx = self.tx.clone();
        let key = self.key.clone();
        let shared = self.shared.clone();
        match self.coordinator.enqueue(
            &changes,
            CoordinatorReason::Reconcile,
            &mut self.runtime,
            &self.shared.scheduler,
            |take| build_index_run(tx, key, shared, take),
        ) {
            Ok(submitted) => Ok(submitted),
            Err(error) => {
                self.runtime.set_writer_pending(false);
                Err(error.into())
            }
        }
    }

    fn handle_watch_batch(&mut self, changes: ChangeSetSnapshot, reason: WatchReason) {
        self.runtime.set_watcher_pending(false);
        let coordinator_reason = match reason {
            WatchReason::Watch => CoordinatorReason::Watch,
            WatchReason::Reconcile => CoordinatorReason::Reconcile,
        };
        let tx = self.tx.clone();
        let key = self.key.clone();
        let shared = self.shared.clone();
        let result = self.coordinator.enqueue(
            &changes,
            coordinator_reason,
            &mut self.runtime,
            &self.shared.scheduler,
            |take| build_index_run(tx, key, shared, take),
        );
        if result.is_ok() {
            self.runtime.set_writer_pending(true);
        }
    }

    fn handle_status(&mut self) -> Result<DaemonIndexStatus, BackendError> {
        let status = match self.status.clone() {
            Some(cached) => cached,
            None => {
                let fresh = self.temp_service().index_status(None)?;
                self.status = Some(fresh.clone());
                fresh
            }
        };
        let job = self.shared.scheduler.get_by_root(self.key.as_str());
        let completion = index_completion_for_job(
            index_completion_from_status(Some(&status)),
            job.as_ref().map(|job| job.state),
            job.as_ref().and_then(|job| job.progress.as_ref()),
        );
        let snapshot = self.runtime.snapshot();
        let info = self.temp_service().workspace_info(None).ok();
        Ok(DaemonIndexStatus {
            status,
            job,
            completion,
            scan_diagnostics: self.scan.clone(),
            info,
            dirty_revision: snapshot.dirty_revision,
            indexed_revision: snapshot.indexed_revision,
            watcher_active: self.runtime.watcher_active(),
        })
    }

    async fn handle_rg(&mut self, query: RgQuery) -> Result<LexicalSearchResult, BackendError> {
        let options = LexicalSearchOptions {
            root: PathBuf::from(self.key.as_str()),
            patterns: query.patterns,
            pattern_files: query.pattern_files.iter().map(PathBuf::from).collect(),
            paths: query.paths,
            limit: query.limit,
            globs: query.globs,
            insensitive_globs: query.insensitive_globs,
            file_types: query.file_types,
            excluded_file_types: query.excluded_file_types,
            hidden: query.hidden,
            no_ignore: query.no_ignore,
            ignore_files: query.ignore_files.iter().map(PathBuf::from).collect(),
            max_depth: query.max_depth,
            max_file_size_bytes: query.max_file_size_bytes,
            fixed_strings: query.fixed_strings,
            ignore_case: query.ignore_case,
            smart_case: query.smart_case,
            word_regexp: query.word_regexp,
            before_context: query.before_context,
            after_context: query.after_context,
            max_count: query.max_count,
            ..LexicalSearchOptions::default()
        };
        tokio::task::spawn_blocking(move || run_lexical_search(&options))
            .await
            .map_err(join_backend_error)?
            .map_err(BackendError::Engine)
    }

    async fn handle_drop(&mut self) -> Result<bool, BackendError> {
        self.shared.scheduler.cancel_root(self.key.as_str());
        self.watcher.close().await;
        self.sessions.close().await;
        let service = self.temp_service();
        let root = PathBuf::from(self.key.as_str());
        tokio::task::spawn_blocking(move || service.drop_index(Some(root.as_path())))
            .await
            .map_err(join_backend_error)?
            .map_err(BackendError::Engine)
    }

    fn apply_finished(&mut self, finished: FinishedIndex) {
        self.runtime.set_writer_pending(false);
        if let Some(ok) = finished.ok {
            if finished.force_full {
                let epoch = self.runtime.reconciliation_epoch();
                self.runtime.mark_reconciled(finished.revision, epoch);
            } else {
                self.runtime.mark_indexed(finished.revision);
            }
            self.scan = ok.index_result.scan_diagnostics.clone();
            self.status = Some(ok.status);
        } else if !finished.force_full {
            // Empty take: nothing was pending when the run took its
            // snapshot, so the stamped target revision is already fresh.
            // Without this, background reconciles that find no work would
            // leave the dirty revision they bumped at enqueue time.
            self.runtime.mark_indexed(finished.revision);
        }
    }
}

/// Builds the scheduler run for one submitted job: takes the pending
/// snapshot lazily, holds the M6 embed permit across the blocking index,
/// and reports back through [`RootCommand::IndexFinished`].
fn build_index_run(
    tx: UnboundedSender<RootCommand>,
    key: RootKey,
    shared: BackendShared,
    take: TakePending,
) -> JobRun {
    Arc::new(move |reporter, token| {
        let tx = tx.clone();
        let key = key.clone();
        let shared = shared.clone();
        // Cloned per invocation: retries replay the same take handle, and
        // `Fn` cannot move the captured original into the future.
        let take = take.clone();
        Box::pin(async move {
            let (snapshot, revision) = take();
            let force_full = snapshot.force_full_reconcile;
            if !force_full
                && snapshot.touched_files.is_empty()
                && snapshot.rescan_directories.is_empty()
                && snapshot.deleted_prefixes.is_empty()
            {
                let _ = tx.send(RootCommand::IndexFinished {
                    finished: Box::new(FinishedIndex {
                        revision,
                        force_full: false,
                        ok: None,
                    }),
                });
                return Ok(());
            }
            if token.is_cancelled() {
                return Err(JobFailure::Cancelled);
            }
            // M6: one embed permit per index job, held across model load
            // and the blocking run, bounding CPU oversubscription.
            let _embed = shared
                .pool
                .embed_permits()
                .acquire_owned()
                .await
                .map_err(|_| JobFailure::Cancelled)?;
            let resolved = match resolve_model(&shared, &key).await {
                Ok(resolved) => resolved,
                Err(AcquireError::Closed) => return Err(JobFailure::Cancelled),
                Err(AcquireError::Load { error, .. }) => return Err(JobFailure::Engine(error)),
            };
            // `resolved` (and its pool lease) lives to the end of this
            // scope — past the blocking run below; dropping it releases
            // the lease.
            let model = resolved.model.clone();
            let _resolved = resolved;
            let outcome = run_blocking_index(
                &shared, &key, model, &snapshot, force_full, reporter, &token,
            )
            .await;
            let (finished_ok, job_result) = match outcome {
                Ok(finished) => (finished, Ok(())),
                Err(error) => (None, Err(error)),
            };
            let _ = tx.send(RootCommand::IndexFinished {
                finished: Box::new(FinishedIndex {
                    revision,
                    force_full,
                    ok: finished_ok,
                }),
            });
            if token.is_cancelled() {
                return Err(JobFailure::Cancelled);
            }
            job_result.map_err(|error| match error {
                IndexRunError::Engine(error) => JobFailure::Engine(error),
                IndexRunError::Join(message) => JobFailure::Failed(message),
            })
        }) as BoxFuture<'static, JobOutcome>
    })
}

enum IndexRunError {
    Engine(EngineError),
    Join(String),
}

/// Blocking index body: permit-scoped, cancellation-aware, returning the
/// fresh status alongside the counters.
async fn run_blocking_index(
    shared: &BackendShared,
    key: &RootKey,
    model: Arc<dyn EmbeddingModel>,
    snapshot: &ChangeSetSnapshot,
    force_full: bool,
    reporter: zg_core::pipeline::indexing::IndexProgressSink,
    token: &CancellationToken,
) -> Result<Option<FinishedOk>, IndexRunError> {
    let facade = ZvecGrepService::new(
        shared
            .service
            .facade_options(key.as_str(), Some(model.clone())),
    );
    let info = facade.workspace_info(None).map_err(IndexRunError::Engine)?;
    let permit = plan_index_permit(
        &shared.auth,
        &info,
        model.info(),
        force_full,
        !snapshot.touched_files.is_empty()
            || !snapshot.rescan_directories.is_empty()
            || !snapshot.deleted_prefixes.is_empty(),
    )
    .map_err(IndexRunError::Engine)?;
    // Cooperative cancel crosses as an owned abort probe (M4): the facade
    // polls it on a helper thread and trips the blocking body's CancelFlag.
    let abort = Arc::new({
        let token = token.clone();
        move || token.is_cancelled()
    });
    let changed: Vec<PathBuf> = snapshot
        .touched_files
        .iter()
        .chain(snapshot.rescan_directories.iter())
        .chain(snapshot.deleted_prefixes.iter())
        .map(PathBuf::from)
        .collect();
    let root = PathBuf::from(key.as_str());
    let joined = tokio::task::spawn_blocking(move || {
        with_remote_embedding_operation_permit(permit, || {
            let index_result = facade.ensure_index(&ZvecGrepIndexOptions {
                root: Some(root.as_path()),
                rebuild: force_full,
                changed_paths: changed,
                on_progress: Some(reporter),
                signal: Some(abort),
                ..ZvecGrepIndexOptions::default()
            })?;
            let status = facade.index_status(None)?;
            Ok(FinishedOk {
                index_result,
                status,
            })
        })
    })
    .await;
    match joined {
        Err(error) => Err(IndexRunError::Join(format!("index task panicked: {error}"))),
        Ok(Err(error)) => Err(IndexRunError::Engine(error)),
        Ok(Ok(finished)) => Ok(Some(finished)),
    }
}

/// Model for one run plus the pool lease that keeps it resident (held
/// until the run completes; overrides carry no lease).
struct ResolvedModel {
    /// Model handle for the facade.
    model: Arc<dyn EmbeddingModel>,
    /// Pool lease, alive for the whole run.
    _lease: Option<ModelLease>,
}

/// Resolves the model for one run: override first, pool otherwise.
async fn resolve_model(
    shared: &BackendShared,
    key: &RootKey,
) -> Result<ResolvedModel, AcquireError> {
    if let Some(model) = shared.service.model_override.clone() {
        return Ok(ResolvedModel {
            model,
            _lease: None,
        });
    }
    let request = load_request_for(&shared.service, key).map_err(|error| AcquireError::Load {
        reference: key.to_string(),
        error,
    })?;
    let lease = shared.pool.acquire(&request).await?;
    Ok(ResolvedModel {
        model: lease.model().clone(),
        _lease: Some(lease),
    })
}

/// Index-side permit: plans remote authorization and returns the existing
/// workspace permit, if any. `None` runs unscoped — local models ignore
/// the scope and remote backends fail closed without a grant.
fn plan_index_permit(
    auth: &RemoteEmbeddingAuthorizationManager,
    info: &ZvecGrepInfoResult,
    model: &EmbeddingModelInfo,
    rebuild: bool,
    needs_update: bool,
) -> EngineResult<Option<RemoteEmbeddingPermit>> {
    let plan = plan_remote_index_authorization(&PlanIndexInput {
        info,
        model,
        rebuild,
        needs_update,
    })?;
    match plan {
        None => Ok(None),
        Some(plan) => auth.existing_workspace_permit(&plan.target),
    }
}

/// Search-side permit (sessions never auto-update).
fn plan_search_permit(
    auth: &RemoteEmbeddingAuthorizationManager,
    info: &ZvecGrepInfoResult,
    model: &EmbeddingModelInfo,
    needs_reconciliation: bool,
) -> EngineResult<Option<RemoteEmbeddingPermit>> {
    let plan = plan_remote_search_authorization(&PlanSearchInput {
        info,
        model,
        uses_vector: true,
        auto_update: false,
        freshness_wait: false,
        runtime_needs_reconciliation: needs_reconciliation,
    })?;
    match plan {
        None => Ok(None),
        Some(plan) => auth.existing_workspace_permit(&plan.target),
    }
}

fn log_event(logger: &Option<DaemonLogger>, name: &str, fields: [(&str, LogField); 2]) {
    if let Some(logger) = logger {
        logger.event(
            name,
            fields
                .into_iter()
                .map(|(key, value)| (key.to_owned(), value))
                .collect::<BTreeMap<_, _>>(),
        );
    }
}

/// Maps a `spawn_blocking` join failure: cancellation races the daemon
/// shutdown path, anything else is a failed index-side task. No new wire
/// code is invented — both variants already exist in the registries.
fn join_backend_error(error: tokio::task::JoinError) -> BackendError {
    if error.is_cancelled() {
        BackendError::Daemon(DaemonError::ShuttingDown)
    } else {
        BackendError::Daemon(DaemonError::IndexFailed {
            message: format!("blocking task panicked: {error}"),
        })
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

/// Keeps `bridge_cancellation` referenced at the M6 seam: index runs cross
/// cancellation as an owned abort probe (see `run_blocking_index`), while
/// leaf sync helpers that take a [`CancelFlag`](zg_core::pipeline::indexing::scanner::CancelFlag)
/// adapt tokens through [`bridge_cancellation`].
#[allow(dead_code)]
fn cancel_flag_for(token: &CancellationToken) -> zg_core::pipeline::indexing::scanner::CancelFlag {
    bridge_cancellation(token)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use zg_core::index_status::IndexJobState;
    use zg_core::models::{
        EmbeddingInput, EmbeddingModel, EmbeddingPurpose, embeddings::EmbeddingResult,
    };

    fn stub_service_config(dimension: usize) -> ServiceConfig {
        ServiceConfig {
            model_override: Some(Arc::new(zg_core::models::stub::StubEmbeddingModel::new(
                dimension,
            ))),
            ..ServiceConfig::default()
        }
    }

    fn backend() -> DaemonBackend {
        DaemonBackend::new(DaemonBackendOptions {
            service: stub_service_config(16),
            ..DaemonBackendOptions::default()
        })
    }

    fn fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn alpha() {}\n").unwrap();
        std::fs::write(dir.path().join("b.rs"), "fn beta() {}\n").unwrap();
        dir
    }

    fn root(dir: &tempfile::TempDir) -> String {
        dir.path().to_string_lossy().into_owned()
    }

    #[tokio::test]
    async fn index_status_search_round_trip() {
        let backend = backend();
        let dir = fixture();
        let root = root(&dir);
        let submitted = backend
            .index(
                &root,
                IndexInput {
                    rebuild: true,
                    ..IndexInput::default()
                },
            )
            .await
            .unwrap();
        let terminal = backend
            .scheduler()
            .wait(&submitted.job.id, None)
            .await
            .unwrap();
        assert_eq!(terminal.state, IndexJobState::Succeeded);
        let status = backend.index_status(&root).await.unwrap();
        assert!(status.status.files_scanned >= 2, "{status:?}");
        assert!(status.completion.is_some());
        // Lexical path is exact: the fixture provably contains this symbol.
        let rg = backend
            .rg_search(
                &root,
                RgQuery {
                    patterns: vec!["fn alpha".to_owned()],
                    ..RgQuery::default()
                },
            )
            .await
            .unwrap();
        assert!(!rg.items.is_empty());
        // Vector path proves the plumbing; ranking over stub hashes is
        // arbitrary, so only the contract is asserted.
        let searched = backend
            .search(
                &root,
                SearchQuery {
                    query: Some("alpha function".to_owned()),
                    limit: Some(5),
                    ..SearchQuery::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(searched.result.root, root);
        assert_eq!(searched.freshness, ResultFreshness::Fresh);
        assert!(searched.indexing.is_none());
        backend.close().await;
    }

    #[tokio::test]
    async fn late_waiter_observes_terminal_state() {
        // Regression: tokio 1.53+ `watch::send` drops values without
        // receivers, so the scheduler must store snapshots
        // unconditionally. An empty reconcile finishes before `wait`
        // subscribes; the late waiter must still see terminal state
        // instead of hanging on a stale slot.
        let backend = backend();
        let dir = fixture();
        let root = root(&dir);
        let submitted = backend.index(&root, IndexInput::default()).await.unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if backend
                    .scheduler()
                    .get(&submitted.job.id)
                    .is_some_and(|snapshot| snapshot.is_terminal())
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        let terminal = tokio::time::timeout(
            Duration::from_secs(10),
            backend.scheduler().wait(&submitted.job.id, None),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(terminal.state, IndexJobState::Succeeded);
        backend.close().await;
    }

    #[tokio::test]
    async fn search_without_index_reports_missing() {
        let backend = backend();
        let dir = tempfile::tempdir().unwrap();
        let error = backend
            .search(
                dir.path().to_str().unwrap(),
                SearchQuery {
                    query: Some("x".to_owned()),
                    ..SearchQuery::default()
                },
            )
            .await
            .unwrap_err();
        assert_eq!(error.code(), "INDEX_MISSING");
        backend.close().await;
    }

    /// Slow embedding model: every batch sleeps, so an index stays
    /// in-flight long enough to shut down underneath it.
    struct SlowModel {
        inner: zg_core::models::stub::StubEmbeddingModel,
        batches: Arc<AtomicUsize>,
    }

    impl EmbeddingModel for SlowModel {
        fn info(&self) -> &EmbeddingModelInfo {
            self.inner.info()
        }

        fn max_batch_size(&self) -> usize {
            1
        }

        fn embed(
            &self,
            purpose: EmbeddingPurpose,
            inputs: &[EmbeddingInput<'_>],
        ) -> EngineResult<EmbeddingResult> {
            self.batches.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(100));
            self.inner.embed(purpose, inputs)
        }
    }

    #[tokio::test]
    async fn shutdown_awaits_in_flight_index() {
        let batches = Arc::new(AtomicUsize::new(0));
        let backend = DaemonBackend::new(DaemonBackendOptions {
            service: ServiceConfig {
                model_override: Some(Arc::new(SlowModel {
                    inner: zg_core::models::stub::StubEmbeddingModel::new(16),
                    batches: batches.clone(),
                })),
                ..ServiceConfig::default()
            },
            ..DaemonBackendOptions::default()
        });
        let dir = tempfile::tempdir().unwrap();
        for index in 0..8 {
            std::fs::write(
                dir.path().join(format!("file{index}.rs")),
                format!("fn symbol{index}() {{}}\n"),
            )
            .unwrap();
        }
        let root = root(&dir);
        let submitted = backend
            .index(
                &root,
                IndexInput {
                    rebuild: true,
                    ..IndexInput::default()
                },
            )
            .await
            .unwrap();
        // Let the run reach the blocking embed body.
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(
            batches.load(Ordering::SeqCst) > 0,
            "index must be in-flight"
        );
        backend.close().await;
        // Close awaited the blocking body instead of orphaning it: the
        // scheduler is drained and the job reached a terminal state.
        assert_eq!(backend.scheduler().load().running, 0);
        let snapshot = backend.scheduler().get(&submitted.job.id).unwrap();
        assert!(snapshot.is_terminal(), "{snapshot:?}");
        assert_eq!(backend.server_status().runtimes, 0);
    }
}
