//! Pool: checkout, single-flight loads, LRU trim, idle TTL, and close.

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

use tokio::sync::Semaphore;
use zg_core::error::EngineError;
use zg_core::models::EmbeddingModel;

use crate::logger::{LogField, opaque_identity};

use super::error::{AcquireError, LoadError};
use super::lease::ModelLease;
use super::options::{
    DEFAULT_MAX_LOADED_MODELS, DEFAULT_MODEL_IDLE_TTL, EmbeddingModelPoolOptions, ModelPoolSnapshot,
};
use super::request::{ModelLoadRequest, default_create_model};
use super::state::{Entry, Loading, NextAction, Shared, State, lock};

/// LRU embedding-model cache. `Clone` shares one pool.
#[derive(Clone)]
pub struct EmbeddingModelPool {
    shared: Arc<Shared>,
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
                let mut state = lock(&self.shared.state);
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
                    let mut state = lock(&self.shared.state);
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
                    // Read-before-await: the loader publishes before it
                    // notifies, so a finished load is observed even when the
                    // notify already fired before we subscribed.
                    loop {
                        let published = loading.lock_result().clone();
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
        *loading.lock_result() = Some(match &outcome {
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
        let state = lock(&self.shared.state);
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

    pub(crate) fn release_key(&self, key: &str) {
        let (empty, ttl, seq, used) = {
            let mut state = lock(&self.shared.state);
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
