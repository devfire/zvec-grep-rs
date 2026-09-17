//! Pool: checkout, single-flight loads, LRU trim, idle TTL, and close.

use std::collections::HashMap;
use std::sync::{
    Arc, LazyLock, Mutex,
    atomic::{AtomicBool, Ordering},
};

use tokio::sync::Semaphore;
use tokio::task::AbortHandle;
use zg_core::error::EngineError;
use zg_core::models::EmbeddingModel;

use crate::logger::{LogField, opaque_identity};
use crate::sync::MutexExt;

use super::error::{AcquireError, LoadError};
use super::lease::ModelLease;
use super::options::{
    DEFAULT_MAX_LOADED_MODELS, DEFAULT_MODEL_IDLE_TTL, EmbeddingModelPoolOptions, ModelPoolSnapshot,
};
use super::request::{ModelLoadRequest, default_create_model};
use super::state::{Entry, Loading, NextAction, Shared, State};

/// LRU embedding-model cache. `Clone` shares one pool.
#[derive(Clone)]
pub struct EmbeddingModelPool {
    shared: Arc<Shared>,
}

/// One pending TTL sleeper per pooled key: idle sequence, last-used stamp,
/// and the abort handle of the sleeping task.
type TtlSleeperTable = HashMap<(usize, String), (u64, u64, AbortHandle)>;

/// Abortable idle-TTL sleepers, one per pool entry.
///
/// `release_key` runs on short-lived `EmbeddingModelPool` handles rebuilt
/// from `Arc<Shared>` on every lease drop, so a pending sleeper cannot live
/// on the handle: it is tracked here, keyed by the `Shared` allocation plus
/// the cache key, with the scheduling release's `(idle_seq, last_used_ms)`
/// so a superseded sleeper never forgets its replacement's entry. Every new
/// release aborts the superseded sleeper before scheduling its replacement,
/// so rapid acquire/release cycles keep at most one pending sleeper per key
/// instead of one task per release. Entries are removed when the sleeper
/// fires, is superseded, or its key is evicted/closed; each sleeper holds an
/// `Arc<Shared>`, so a pool identity cannot be recycled while its entries
/// exist. Never nested with the state lock: map and state are always taken
/// separately.
static TTL_SLEEPERS: LazyLock<Mutex<TtlSleeperTable>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Identity of the pool behind a handle: the `Shared` allocation address.
fn pool_identity(shared: &Arc<Shared>) -> usize {
    Arc::as_ptr(shared) as usize
}

/// Aborts and forgets the pending TTL sleeper for `key`, if any.
fn abort_ttl_sleeper(shared: &Arc<Shared>, key: &str) {
    let id = (pool_identity(shared), key.to_owned());
    if let Some((_, _, handle)) = TTL_SLEEPERS.lock_ignore_poison().remove(&id) {
        handle.abort();
    }
}

/// Forgets a sleeper entry only when it is still ours: a superseding release
/// schedules under a bumped `(idle_seq, last_used_ms)`, so a stale match
/// leaves the replacement registered.
fn forget_ttl_sleeper_if_current(shared: &Arc<Shared>, key: &str, seq: u64, used: u64) {
    let id = (pool_identity(shared), key.to_owned());
    let mut sleepers = TTL_SLEEPERS.lock_ignore_poison();
    let current = sleepers
        .get(&id)
        .is_some_and(|(stored_seq, stored_used, _)| *stored_seq == seq && *stored_used == used);
    if current {
        sleepers.remove(&id);
    }
}

/// Aborts every pending TTL sleeper of this pool.
fn abort_pool_ttl_sleepers(shared: &Arc<Shared>) {
    let identity = pool_identity(shared);
    let handles: Vec<AbortHandle> = {
        let mut sleepers = TTL_SLEEPERS.lock_ignore_poison();
        let stale: Vec<(usize, String)> = sleepers
            .keys()
            .filter(|(pool, _)| *pool == identity)
            .cloned()
            .collect();
        stale
            .into_iter()
            .filter_map(|id| sleepers.remove(&id).map(|(_, _, handle)| handle))
            .collect()
    };
    for handle in handles {
        handle.abort();
    }
}

impl EmbeddingModelPool {
    /// Empty pool with the given options.
    #[must_use]
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

    pub(crate) fn from_shared(shared: Arc<Shared>) -> Self {
        Self { shared }
    }

    /// Cache key for a request (exposed so runtimes can compare generations).
    #[must_use]
    pub fn key_for(&self, request: &ModelLoadRequest) -> String {
        request.key()
    }

    /// Bounded embed gate (M6): hold across CPU-bound embed work.
    #[must_use]
    pub fn embed_permits(&self) -> Arc<Semaphore> {
        Arc::clone(&self.shared.embed_permits)
    }

    /// Checks out a model, loading it on a blocking thread on first use.
    /// Concurrent acquirers of a loading key share the load (and its
    /// failure) instead of re-running construction.
    ///
    /// # Errors
    ///
    /// Returns [`AcquireError::Closed`] when the pool is closed, or
    /// [`AcquireError::Load`] when model construction fails.
    pub async fn acquire(&self, request: &ModelLoadRequest) -> Result<ModelLease, AcquireError> {
        if self.shared.closed.load(Ordering::SeqCst) {
            return Err(AcquireError::Closed);
        }
        let key = request.key();
        loop {
            let action = {
                let mut state = self.shared.state.lock_ignore_poison();
                let entry = state
                    .entries
                    .entry(key.clone())
                    .or_insert_with(Entry::empty);
                if entry.model.is_some() {
                    NextAction::Checkout
                } else if let Some(loading) = entry.loading.clone() {
                    NextAction::Wait(loading)
                } else {
                    let loading = Arc::new(Loading::new());
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
                    let mut state = self.shared.state.lock_ignore_poison();
                    let Some(entry) = state.entries.get_mut(&key) else {
                        continue;
                    };
                    let Some(model) = entry.model.clone() else {
                        continue;
                    };
                    entry.leases += 1;
                    entry.touch();
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
                    // Subscribe-before-check: `notify_waiters` wakes only
                    // futures that are already registered, and stores no
                    // permit, so the `Notified` future must be created (and
                    // polled once via `enable`) BEFORE reading the published
                    // result. Checking first and subscribing after would miss
                    // a publish-then-notify landing in between and park
                    // forever.
                    loop {
                        let notified = loading.notify.notified();
                        tokio::pin!(notified);
                        notified.as_mut().enable();
                        let published = loading.lock_result().clone();
                        match published {
                            Some(Ok(())) => break,
                            Some(Err(error)) => {
                                return Err(AcquireError::Load {
                                    reference: request.reference.as_str().to_owned(),
                                    error: error.rehydrate(),
                                });
                            }
                            None => notified.await,
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
        *loading.lock_result() = Some(match &outcome {
            Ok(_) => Ok(()),
            Err(error) => Err(LoadError::dehydrate(error)),
        });
        loading.notify.notify_waiters();
        let mut state = self.shared.state.lock_ignore_poison();
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
                    entry.touch();
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
    #[must_use]
    pub fn snapshot(&self) -> ModelPoolSnapshot {
        let state = self.shared.state.lock_ignore_poison();
        ModelPoolSnapshot {
            loaded: state.loaded_count(),
            active_leases: state.active_leases(),
        }
    }

    /// Marks the pool closed and evicts every zero-lease model. In-flight
    /// loads finish and are dropped; outstanding leases dispose on release.
    pub async fn close(&self) {
        self.shared.closed.store(true, Ordering::SeqCst);
        loop {
            let loadings: Vec<Arc<Loading>> = self
                .shared
                .state
                .lock_ignore_poison()
                .entries
                .values()
                .filter_map(|entry| entry.loading.clone())
                .collect();
            if loadings.is_empty() {
                break;
            }
            for loading in loadings {
                // Same subscribe-before-check discipline as `acquire`: the
                // result is re-checked after subscribing, so a load that
                // finishes between the snapshot above and this park cannot
                // hang `close()` on an already-fired notify.
                let notified = loading.notify.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                if loading.lock_result().is_none() {
                    notified.await;
                }
            }
        }
        abort_pool_ttl_sleepers(&self.shared);
        self.shared
            .state
            .lock_ignore_poison()
            .entries
            .retain(|_, entry| {
                entry.retired = true;
                entry.leases > 0
            });
    }

    fn trim_idle(&self, except_key: &str) {
        let mut victims: Vec<String> = Vec::new();
        {
            let state = self.shared.state.lock_ignore_poison();
            let loaded_count = state.loaded_count();
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
        // A pending TTL sleeper for an evicted key would only wake to find
        // its entry gone; abort it instead of letting it sleep out the TTL.
        abort_ttl_sleeper(&self.shared, key);
        let removed = self.shared.state.lock_ignore_poison().entries.remove(key);
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

    pub(crate) fn release_key(&self, key: &str) {
        let (empty, ttl, seq, used) = {
            let mut state = self.shared.state.lock_ignore_poison();
            let Some(entry) = state.entries.get_mut(key) else {
                return;
            };
            entry.leases = entry.leases.saturating_sub(1);
            entry.touch();
            (
                entry.leases == 0,
                self.shared.idle_ttl,
                entry.idle_seq,
                entry.last_used_ms,
            )
        };
        // Every release bumps `idle_seq`, superseding any pending sleeper for
        // this key; abort it now instead of letting it sleep out the full
        // TTL only to discover its staleness. Without this each release
        // leaks one task per zero-lease spell.
        abort_ttl_sleeper(&self.shared, key);
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
        let identity = pool_identity(&self.shared);
        let task_key = owned_key.clone();
        let handle = tokio::spawn(async move {
            tokio::time::sleep(ttl).await;
            let stale = {
                let state = slf.shared.state.lock_ignore_poison();
                state.entries.get(&task_key).is_some_and(|entry| {
                    entry.leases == 0 && entry.last_used_ms == used && entry.idle_seq == seq
                })
            };
            // Forget our registry entry only if no newer release superseded
            // us; a replacement sleeps under a bumped `(idle_seq,
            // last_used_ms)`.
            forget_ttl_sleeper_if_current(&slf.shared, &task_key, seq, used);
            if stale {
                slf.evict(&task_key);
            }
        });
        // No `.await` runs between `spawn` and this insert, so the sleeper
        // cannot have fired yet: the entry forgotten above is still ours.
        let replaced = TTL_SLEEPERS
            .lock_ignore_poison()
            .insert((identity, owned_key), (seq, used, handle.abort_handle()));
        // Defensive: a concurrent release may have scheduled first; never
        // keep two sleepers for one key.
        if let Some((_, _, prev)) = replaced {
            prev.abort();
        }
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
