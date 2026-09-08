//! Embedding model pool: LRU cache with RAII leases and a bounded embed gate.
//!
//! Mirrors `../zvec-grep/src/daemon/model-pool.ts` (`EmbeddingModelPool`:
//! keyed entries, single-flight loads, lease counting, idle TTL, LRU trim
//! at `maxLoadedModels`). Two divergences (see `docs/ts-divergence.md`):
//!
//! - Loads run on `spawn_blocking` (model construction is synchronous CPU
//!   work) instead of `await createModel(...)`; concurrent acquirers of a
//!   loading key wait on a [`Notify`](tokio::sync::Notify) instead of
//!   sharing a promise. Waiters share a dehydrated error (the
//!   allocation-free [`EngineErrorCode`](zg_core::error::EngineErrorCode) is
//!   `Copy`, so the code survives dehydration exactly) rather than
//!   re-running the load.
//! - TS `model.dispose()` has no Rust analogue: eviction drops the last
//!   `Arc`, and the model destructors run then. The observable behavior —
//!   at most `max_loaded_models` idle models resident — is identical.
//!
//! The pool also owns the single bounded embed gate from M6: all CPU-bound
//! embed work should hold [`EmbeddingModelPool::embed_permits`] (sized to
//! `available_parallelism`) so a many-root daemon cannot oversubscribe the
//! blocking pool.

use std::collections::HashMap;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::sync::{Notify, Semaphore};
use zg_core::error::{EngineError, EngineErrorCode};
use zg_core::models::EmbeddingModel;
use zg_core::models::catalog::ModelReference;
use zg_core::models::embeddings::CreateEmbeddingModelOptions;
use zg_core::models::factory::create_embedding_model;

use crate::errors::DaemonError;
use crate::logger::{DaemonLogger, LogField, opaque_identity};

/// Idle TTL before a zero-lease model is evicted.
pub const DEFAULT_MODEL_IDLE_TTL: Duration = Duration::from_secs(15 * 60);

/// Default cap on resident loaded models.
pub const DEFAULT_MAX_LOADED_MODELS: usize = 1;

/// What to load: a catalog reference plus its runtime options.
///
/// Mirrors TS `EmbeddingModelLoadRequest` (`model` identity + `runtime`
/// overrides); the Rust shape reuses the existing
/// [`CreateEmbeddingModelOptions`] instead of a second runtime struct.
#[derive(Debug, Clone)]
pub struct ModelLoadRequest {
    /// Catalog reference, e.g. `local/potion-retrieval-32m`.
    pub reference: ModelReference,
    /// API key / endpoint / cache dir / device overrides.
    pub options: CreateEmbeddingModelOptions,
}

impl ModelLoadRequest {
    /// Cache key: reference plus every option that changes the built
    /// model, so two runtimes never share one entry by accident.
    pub fn key(&self) -> String {
        format!(
            "{}\0{}\0{}\0{}\0{:?}",
            self.reference.as_str(),
            self.options.api_key.as_deref().unwrap_or(""),
            self.options.endpoint.as_deref().unwrap_or(""),
            self.options
                .model_cache_dir
                .as_ref()
                .map_or(String::new(), |dir| dir.display().to_string()),
            self.options.device,
        )
    }
}

/// Constructor for one model. Runs on `spawn_blocking`; must be `Send`.
pub type CreateModelFn =
    Arc<dyn Fn(&ModelLoadRequest) -> Result<Arc<dyn EmbeddingModel>, EngineError> + Send + Sync>;

/// Default constructor: the `zg-core` factory dispatch.
pub fn default_create_model(
    request: &ModelLoadRequest,
) -> Result<Arc<dyn EmbeddingModel>, EngineError> {
    create_embedding_model(&request.reference, &request.options).map_err(EngineError::from)
}

/// Failure to check out a model: either the pool is closed or the load
/// itself failed (with the engine error preserved, not stringified).
#[derive(Debug)]
pub enum AcquireError {
    /// The pool is closed; the daemon is shutting down.
    Closed,
    /// Model construction failed.
    Load {
        /// Catalog reference that failed to load.
        reference: String,
        /// Engine failure, preserved for diagnostics and retry decisions.
        error: EngineError,
    },
}

impl AcquireError {
    /// Daemon wire code for this failure.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Closed => DaemonError::ShuttingDown.code(),
            Self::Load { .. } => DaemonError::ModelLoadFailed {
                reference: String::new(),
            }
            .code(),
        }
    }

    /// Inner engine error for load failures.
    pub fn engine_error(&self) -> Option<&EngineError> {
        match self {
            Self::Load { error, .. } => Some(error),
            Self::Closed => None,
        }
    }
}

impl std::fmt::Display for AcquireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Closed => write!(f, "embedding model pool is closed"),
            Self::Load { reference, error } => write!(f, "failed to load {reference}: {error}"),
        }
    }
}

impl std::error::Error for AcquireError {}

/// Options for [`EmbeddingModelPool`].
#[derive(Clone, Default)]
pub struct EmbeddingModelPoolOptions {
    /// Zero-lease idle TTL; defaults to 15 minutes. Zero evicts eagerly.
    pub idle_ttl: Option<Duration>,
    /// Cap on resident models; defaults to 1.
    pub max_loaded_models: Option<usize>,
    /// Model constructor; defaults to [`default_create_model`].
    pub create_model: Option<CreateModelFn>,
    /// Daemon logger for `model.load` / `model.cache_hit` / `model.evicted`.
    pub logger: Option<DaemonLogger>,
}

/// Pool load snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelPoolSnapshot {
    /// Entries with a resident model.
    pub loaded: usize,
    /// Leases currently checked out.
    pub active_leases: usize,
}

/// Dehydrated load failure shared with waiting acquirers. The code is the
/// `Copy` [`EngineErrorCode`], so it round-trips exactly; message and
/// context are owned strings.
#[derive(Debug, Clone)]
struct LoadError {
    code: EngineErrorCode,
    message: String,
    context: Option<String>,
}

impl LoadError {
    fn dehydrate(error: &EngineError) -> Self {
        Self {
            code: *error.code(),
            message: error.message().to_owned(),
            context: error.context().map(str::to_owned),
        }
    }

    fn rehydrate(&self) -> EngineError {
        let mut error = EngineError::new(self.code, self.message.clone());
        if let Some(context) = &self.context {
            error = error.with_context(context.clone());
        }
        error
    }
}

struct Loading {
    notify: Notify,
    // Success marker only: the model itself is read from the entry, so
    // waiters share a `Copy`-code error without cloning the model.
    result: Mutex<Option<Result<(), LoadError>>>,
}

struct Entry {
    log_identity: String,
    model: Option<Arc<dyn EmbeddingModel>>,
    leases: usize,
    last_used_ms: u64,
    retired: bool,
    loading: Option<Arc<Loading>>,
    idle_seq: u64,
}

#[derive(Default)]
struct State {
    entries: HashMap<String, Entry>,
}

struct Shared {
    state: Mutex<State>,
    idle_ttl: Duration,
    max_loaded: usize,
    create: CreateModelFn,
    embed_permits: Arc<Semaphore>,
    logger: Option<DaemonLogger>,
    closed: AtomicBool,
}

/// LRU embedding-model cache. `Clone` shares one pool.
#[derive(Clone)]
pub struct EmbeddingModelPool {
    shared: Arc<Shared>,
}

enum NextAction {
    Checkout,
    Load(Arc<Loading>),
    Wait(Arc<Loading>),
}

impl EmbeddingModelPool {
    /// Empty pool with the given options.
    pub fn new(options: EmbeddingModelPoolOptions) -> Self {
        let parallelism = std::thread::available_parallelism()
            .map(|parallelism| parallelism.get())
            .unwrap_or(4)
            .max(1);
        Self {
            shared: Arc::new(Shared {
                state: Mutex::new(State::default()),
                idle_ttl: options.idle_ttl.unwrap_or(DEFAULT_MODEL_IDLE_TTL),
                max_loaded: options
                    .max_loaded_models
                    .unwrap_or(DEFAULT_MAX_LOADED_MODELS)
                    .max(1),
                create: options
                    .create_model
                    .unwrap_or_else(|| Arc::new(default_create_model)),
                embed_permits: Arc::new(Semaphore::new(parallelism)),
                logger: options.logger,
                closed: AtomicBool::new(false),
            }),
        }
    }

    /// Cache key for a request (exposed so runtimes can compare generations).
    pub fn key_for(&self, request: &ModelLoadRequest) -> String {
        request.key()
    }

    /// Bounded embed gate (M6): hold across CPU-bound embed work.
    pub fn embed_permits(&self) -> Arc<Semaphore> {
        Arc::clone(&self.shared.embed_permits)
    }

    /// Checks out a model, loading it on a blocking thread on first use.
    /// Concurrent acquirers of a loading key share the load (and its
    /// failure) instead of re-running construction.
    pub async fn acquire(&self, request: &ModelLoadRequest) -> Result<ModelLease, AcquireError> {
        if self.shared.closed.load(Ordering::SeqCst) {
            return Err(AcquireError::Closed);
        }
        let key = request.key();
        loop {
            let action = {
                let mut state = lock(&self.shared.state);
                let entry = state.entries.entry(key.clone()).or_insert_with(|| Entry {
                    log_identity: uuid::Uuid::new_v4().to_string(),
                    model: None,
                    leases: 0,
                    last_used_ms: now_ms(),
                    retired: false,
                    loading: None,
                    idle_seq: 0,
                });
                if entry.model.is_some() {
                    NextAction::Checkout
                } else if let Some(loading) = entry.loading.clone() {
                    NextAction::Wait(loading)
                } else {
                    let loading = Arc::new(Loading {
                        notify: Notify::new(),
                        result: Mutex::new(None),
                    });
                    entry.loading = Some(loading.clone());
                    let identity = entry.log_identity.clone();
                    drop(state);
                    self.log(
                        "model.load",
                        &[("model_id", LogField::from(opaque_identity(&identity)))],
                    );
                    NextAction::Load(loading)
                }
            };
            match action {
                NextAction::Checkout => {
                    let mut state = lock(&self.shared.state);
                    let Some(entry) = state.entries.get_mut(&key) else {
                        continue;
                    };
                    let Some(model) = entry.model.clone() else {
                        continue;
                    };
                    entry.leases += 1;
                    entry.last_used_ms = now_ms();
                    entry.idle_seq += 1;
                    let identity = entry.log_identity.clone();
                    drop(state);
                    self.log(
                        "model.cache_hit",
                        &[("model_id", LogField::from(opaque_identity(&identity)))],
                    );
                    self.trim_idle(&key);
                    return Ok(ModelLease::new(model, key, self.shared.clone()));
                }
                NextAction::Load(loading) => {
                    return self.finish_load(&key, request, &loading).await;
                }
                NextAction::Wait(loading) => {
                    // Read-before-await: the loader publishes before it
                    // notifies, so a finished load is observed even when the
                    // notify already fired before we subscribed.
                    loop {
                        let published = loading
                            .result
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                            .clone();
                        match published {
                            Some(Ok(())) => break,
                            Some(Err(error)) => {
                                return Err(AcquireError::Load {
                                    reference: request.reference.as_str().to_owned(),
                                    error: error.rehydrate(),
                                });
                            }
                            None => loading.notify.notified().await,
                        }
                    }
                }
            }
        }
    }

    /// Runs construction on a blocking thread, publishes the outcome to
    /// waiters, and either checks out the model or reports the failure.
    async fn finish_load(
        &self,
        key: &str,
        request: &ModelLoadRequest,
        loading: &Arc<Loading>,
    ) -> Result<ModelLease, AcquireError> {
        let create = Arc::clone(&self.shared.create);
        let owned = request.clone();
        let outcome: Result<Arc<dyn EmbeddingModel>, EngineError> =
            tokio::task::spawn_blocking(move || create(&owned))
                .await
                .map_err(|error| {
                    EngineError::new(
                        zg_core::error::codes::daemon_blocking_join_failed(),
                        "embedding model load task failed",
                    )
                    .with_context(format!("detail={error}"))
                })
                .and_then(|result| result);
        // Publish first so `close()` (which waits on these notifies) can
        // never miss the outcome, even when our entry is already gone.
        *loading
            .result
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(match &outcome {
            Ok(_) => Ok(()),
            Err(error) => Err(LoadError::dehydrate(error)),
        });
        loading.notify.notify_waiters();
        let mut state = lock(&self.shared.state);
        match outcome {
            Ok(model) => {
                if self.shared.closed.load(Ordering::SeqCst) {
                    state.entries.remove(key);
                    return Err(AcquireError::Closed);
                }
                if let Some(entry) = state.entries.get_mut(key) {
                    entry.loading = None;
                    entry.model = Some(model.clone());
                    entry.leases += 1;
                    entry.last_used_ms = now_ms();
                    entry.idle_seq += 1;
                    drop(state);
                    self.trim_idle(key);
                    Ok(ModelLease::new(model, key.to_owned(), self.shared.clone()))
                } else {
                    // Only `close()` removes a loading entry; treat it as closed.
                    Err(AcquireError::Closed)
                }
            }
            Err(error) => {
                state.entries.remove(key);
                Err(AcquireError::Load {
                    reference: request.reference.as_str().to_owned(),
                    error,
                })
            }
        }
    }

    /// Current load.
    pub fn snapshot(&self) -> ModelPoolSnapshot {
        let state = lock(&self.shared.state);
        ModelPoolSnapshot {
            loaded: state
                .entries
                .values()
                .filter(|entry| entry.model.is_some())
                .count(),
            active_leases: state.entries.values().map(|entry| entry.leases).sum(),
        }
    }

    /// Marks the pool closed and evicts every zero-lease model. In-flight
    /// loads finish and are dropped; outstanding leases dispose on release.
    pub async fn close(&self) {
        self.shared.closed.store(true, Ordering::SeqCst);
        loop {
            let loadings: Vec<Arc<Loading>> = lock(&self.shared.state)
                .entries
                .values()
                .filter_map(|entry| entry.loading.clone())
                .collect();
            if loadings.is_empty() {
                break;
            }
            for loading in loadings {
                loading.notify.notified().await;
            }
        }
        lock(&self.shared.state).entries.retain(|_, entry| {
            entry.retired = true;
            entry.leases > 0
        });
    }

    fn trim_idle(&self, except_key: &str) {
        let mut victims: Vec<String> = Vec::new();
        {
            let state = lock(&self.shared.state);
            let loaded_count = state
                .entries
                .values()
                .filter(|entry| entry.model.is_some())
                .count();
            if loaded_count <= self.shared.max_loaded {
                return;
            }
            let mut candidates: Vec<(&String, u64)> = state
                .entries
                .iter()
                .filter(|(key, entry)| {
                    key.as_str() != except_key && entry.model.is_some() && entry.leases == 0
                })
                .map(|(key, entry)| (key, entry.last_used_ms))
                .collect();
            candidates.sort_by_key(|(_, used)| *used);
            let mut count = loaded_count;
            for (key, _) in candidates {
                if count <= self.shared.max_loaded {
                    break;
                }
                victims.push(key.clone());
                count -= 1;
            }
        }
        for key in victims {
            self.evict(&key);
        }
    }

    fn evict(&self, key: &str) {
        let removed = lock(&self.shared.state).entries.remove(key);
        if let Some(entry) = removed {
            self.log(
                "model.evicted",
                &[(
                    "model_id",
                    LogField::from(opaque_identity(&entry.log_identity)),
                )],
            );
        }
    }

    fn release_key(&self, key: &str) {
        let (empty, ttl, seq, used) = {
            let mut state = lock(&self.shared.state);
            let Some(entry) = state.entries.get_mut(key) else {
                return;
            };
            entry.leases = entry.leases.saturating_sub(1);
            entry.last_used_ms = now_ms();
            entry.idle_seq += 1;
            (
                entry.leases == 0,
                self.shared.idle_ttl,
                entry.idle_seq,
                entry.last_used_ms,
            )
        };
        if !empty {
            return;
        }
        if self.shared.closed.load(Ordering::SeqCst) || ttl.is_zero() {
            self.evict(key);
            return;
        }
        // `Drop` may run outside a runtime (e.g. in plain tests); without
        // one there is nothing to schedule on, so evict synchronously.
        if tokio::runtime::Handle::try_current().is_err() {
            self.evict(key);
            return;
        }
        let slf = self.clone();
        let owned_key = key.to_owned();
        tokio::spawn(async move {
            tokio::time::sleep(ttl).await;
            let stale = {
                let state = lock(&slf.shared.state);
                state.entries.get(&owned_key).is_some_and(|entry| {
                    entry.leases == 0 && entry.last_used_ms == used && entry.idle_seq == seq
                })
            };
            if stale {
                slf.evict(&owned_key);
            }
        });
    }

    fn log(&self, name: &str, entries: &[(&str, LogField)]) {
        if let Some(logger) = &self.shared.logger {
            logger.event(
                name,
                entries
                    .iter()
                    .map(|(key, value)| ((*key).to_owned(), value.clone()))
                    .collect(),
            );
        }
    }
}

/// Checked-out model. Dropping (or [`ModelLease::release`]) returns the
/// lease to the pool; the model unloads on idle TTL or pool close.
pub struct ModelLease {
    model: Arc<dyn EmbeddingModel>,
    key: String,
    pool: Option<Arc<Shared>>,
}

impl ModelLease {
    fn new(model: Arc<dyn EmbeddingModel>, key: String, pool: Arc<Shared>) -> Self {
        Self {
            model,
            key,
            pool: Some(pool),
        }
    }

    /// Checked-out model handle.
    pub fn model(&self) -> &Arc<dyn EmbeddingModel> {
        &self.model
    }

    /// Pool key the lease was checked out under.
    pub fn key(&self) -> &str {
        &self.key
    }

    /// Returns the lease; equivalent to dropping it.
    /// Returns the lease; equivalent to dropping it.
    pub fn release(mut self) {
        self.return_lease();
    }

    fn return_lease(&mut self) {
        if let Some(pool) = self.pool.take() {
            EmbeddingModelPool { shared: pool }.release_key(&self.key);
        }
    }
}

impl std::fmt::Debug for ModelLease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModelLease")
            .field("key", &self.key)
            .finish_non_exhaustive()
    }
}

impl Drop for ModelLease {
    fn drop(&mut self) {
        self.return_lease();
    }
}

fn lock(state: &Mutex<State>) -> std::sync::MutexGuard<'_, State> {
    state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stub_request() -> ModelLoadRequest {
        ModelLoadRequest {
            reference: ModelReference::new("stub/deterministic"),
            options: CreateEmbeddingModelOptions::default(),
        }
    }

    fn stub_pool(options: EmbeddingModelPoolOptions) -> EmbeddingModelPool {
        let mut options = options;
        options.create_model = Some(Arc::new(|_: &ModelLoadRequest| {
            Ok(Arc::new(zg_core::models::stub::StubEmbeddingModel::new(16))
                as Arc<dyn EmbeddingModel>)
        }));
        EmbeddingModelPool::new(options)
    }

    #[tokio::test]
    async fn acquire_caches_and_counts_leases() {
        let pool = stub_pool(EmbeddingModelPoolOptions::default());
        let first = pool.acquire(&stub_request()).await.unwrap();
        let second = pool.acquire(&stub_request()).await.unwrap();
        assert_eq!(
            pool.snapshot(),
            ModelPoolSnapshot {
                loaded: 1,
                active_leases: 2
            }
        );
        first.release();
        assert_eq!(pool.snapshot().active_leases, 1);
        drop(second);
        assert_eq!(pool.snapshot().active_leases, 0);
    }

    #[tokio::test]
    async fn load_failure_is_shared_not_cached() {
        let pool = EmbeddingModelPool::new(EmbeddingModelPoolOptions {
            create_model: Some(Arc::new(|request: &ModelLoadRequest| {
                Err(EngineError::from(
                    zg_core::models::error::ModelError::CatalogModelNotFound {
                        reference: request.reference.as_str().to_owned(),
                    },
                ))
            })),
            ..EmbeddingModelPoolOptions::default()
        });
        let error = pool.acquire(&stub_request()).await.unwrap_err();
        assert_eq!(error.code(), "MODEL_LOAD_FAILED");
        assert!(matches!(error, AcquireError::Load { .. }));
        assert_eq!(pool.snapshot().loaded, 0);
    }

    #[tokio::test]
    async fn concurrent_acquirers_share_one_load() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let loads = Arc::new(AtomicUsize::new(0));
        let loads_clone = loads.clone();
        let pool = EmbeddingModelPool::new(EmbeddingModelPoolOptions {
            create_model: Some(Arc::new(move |_: &ModelLoadRequest| {
                loads_clone.fetch_add(1, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(50));
                Ok(Arc::new(zg_core::models::stub::StubEmbeddingModel::new(8))
                    as Arc<dyn EmbeddingModel>)
            })),
            ..EmbeddingModelPoolOptions::default()
        });
        let request = stub_request();
        let first = pool.acquire(&request);
        let second = pool.acquire(&request);
        let (first, second) = tokio::join!(first, second);
        first.unwrap();
        second.unwrap();
        assert_eq!(loads.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn lru_trim_evicts_idle_models_beyond_cap() {
        let pool = stub_pool(EmbeddingModelPoolOptions {
            max_loaded_models: Some(1),
            idle_ttl: Some(Duration::from_secs(3600)),
            ..EmbeddingModelPoolOptions::default()
        });
        let first = pool.acquire(&stub_request()).await.unwrap();
        drop(first);
        assert_eq!(pool.snapshot().loaded, 1);
        let mut other = stub_request();
        other.reference = ModelReference::new("stub/other");
        let second = pool.acquire(&other).await.unwrap();
        assert_eq!(pool.snapshot().loaded, 1);
        drop(second);
    }

    #[tokio::test]
    async fn close_evicts_idle_and_rejects_acquire() {
        let pool = stub_pool(EmbeddingModelPoolOptions::default());
        let lease = pool.acquire(&stub_request()).await.unwrap();
        pool.close().await;
        assert_eq!(lease.model().info().dimension, 16);
        drop(lease);
        assert_eq!(pool.snapshot().loaded, 0);
        assert!(matches!(
            pool.acquire(&stub_request()).await,
            Err(AcquireError::Closed)
        ));
    }
}
