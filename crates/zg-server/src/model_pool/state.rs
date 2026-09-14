//! In-memory entries: single-flight load state, lease counting, and the
//! shared pool state behind one short-held mutex.
//!
//! The [`Shared::state`] mutex guards short, uncontended critical sections
//! only: no `.await` is ever held across [`lock`]. Contended or async work
//! (model construction, TTL sleeps) runs outside the lock on
//! `spawn_blocking` / `tokio::spawn`, with [`tokio::sync::Notify`]
//! carrying the wakeups. That keeps the `Mutex` (rather than an actor)
//! the right tool here.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, atomic::AtomicBool};
use std::time::Duration;

use tokio::sync::{Notify, Semaphore};
use zg_core::models::EmbeddingModel;
use zg_core::types::UnixMillis;

use crate::logger::DaemonLogger;

use super::error::LoadError;
use super::request::CreateModelFn;

/// Single-flight load gate for one key. Waiters read the published outcome
/// (and share its dehydrated error) instead of re-running construction.
pub(crate) struct Loading {
    pub(crate) notify: Notify,
    // Success marker only: the model itself is read from the entry, so
    // waiters share a `Copy`-code error without cloning the model.
    result: Mutex<Option<Result<(), LoadError>>>,
}

impl Loading {
    pub(crate) fn new() -> Self {
        Self {
            notify: Notify::new(),
            result: Mutex::new(None),
        }
    }

    /// Outcome slot. Single poison policy for the whole pool: a poisoned
    /// mutex yields its inner value rather than failing the checkout.
    pub(crate) fn lock_result(&self) -> MutexGuard<'_, Option<Result<(), LoadError>>> {
        self.result
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// One cache key: resident model (if loaded), lease count, freshness, and
/// any in-flight load.
pub(crate) struct Entry {
    pub(crate) log_identity: String,
    pub(crate) model: Option<Arc<dyn EmbeddingModel>>,
    pub(crate) leases: usize,
    pub(crate) last_used_ms: u64,
    pub(crate) retired: bool,
    pub(crate) loading: Option<Arc<Loading>>,
    pub(crate) idle_seq: u64,
}

impl Entry {
    pub(crate) fn empty() -> Self {
        Self {
            log_identity: uuid::Uuid::new_v4().to_string(),
            model: None,
            leases: 0,
            // Clock-unavailable direction: idle-evict is equality-guarded;
            // `0` reads as ancient, evicting eagerly at worst (fail-closed).
            last_used_ms: UnixMillis::now_ms_or(0),
            retired: false,
            loading: None,
            idle_seq: 0,
        }
    }

    /// Refreshes freshness. The lease-count change stays at the call site
    /// so it reads explicitly next to the reason.
    pub(crate) fn touch(&mut self) {
        // Same direction: equality-guarded idle-evict; eager at worst.
        self.last_used_ms = UnixMillis::now_ms_or(0);
        self.idle_seq += 1;
    }
}

#[derive(Default)]
pub(crate) struct State {
    pub(crate) entries: HashMap<String, Entry>,
}

impl State {
    pub(crate) fn loaded_count(&self) -> usize {
        self.entries
            .values()
            .filter(|entry| entry.model.is_some())
            .count()
    }

    pub(crate) fn active_leases(&self) -> usize {
        self.entries.values().map(|entry| entry.leases).sum()
    }
}

pub(crate) struct Shared {
    pub(crate) state: Mutex<State>,
    pub(crate) idle_ttl: Duration,
    pub(crate) max_loaded: usize,
    pub(crate) create: CreateModelFn,
    pub(crate) embed_permits: Arc<Semaphore>,
    pub(crate) logger: Option<DaemonLogger>,
    pub(crate) closed: AtomicBool,
}

pub(crate) enum NextAction {
    Checkout,
    Load(Arc<Loading>),
    Wait(Arc<Loading>),
}

/// Pool state lock. Single poison policy: a poisoned mutex yields its inner
/// value rather than failing the checkout.
pub(crate) fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
