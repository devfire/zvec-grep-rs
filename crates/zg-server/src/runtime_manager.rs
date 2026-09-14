//! Root actor registry: one actor task per canonical root.
//!
//! Mirrors the registry half of `../zvec-grep/src/daemon/runtime-manager.ts`
//! (`activate` / `activateForIndex` / `evict` / idle timers / root aliases).
//! The TS manager stores live `RootRuntime` objects in `Map`s shared across
//! `await` points; here the manager stores only [`RootHandle`]s
//! (`mpsc` senders + join handles) and every `RootRuntime` is owned by its
//! actor task as plain `&mut` state (M3/M6, see `docs/ts-divergence.md`).
//! Idle eviction needs no manager task: each actor loop carries its own
//! idle deadline and unregisters itself; the manager only evicts on
//! request.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use tokio::task::JoinHandle;
use zg_core::service::facade::ZvecGrepService;

use crate::backend::{BackendError, BackendShared, RootCommand, RootHandle, spawn_root_actor};
use crate::errors::DaemonError;
use crate::root_runtime::{RootKey, resolve_requested_root};
use crate::sync::MutexExt;

/// Idle TTL before a quiet actor exits itself; mirrors TS `runtimeIdleTtlMs`.
pub const DEFAULT_RUNTIME_IDLE_TTL: Duration = Duration::from_secs(30 * 60);

struct ActorEntry {
    handle: RootHandle,
    join: Option<JoinHandle<()>>,
}

#[derive(Default)]
struct Inner {
    actors: HashMap<String, ActorEntry>,
    aliases: HashMap<String, String>,
}

/// Registry of live root actors. `Clone` shares one registry.
#[derive(Clone)]
pub struct RuntimeManager {
    inner: Arc<Mutex<Inner>>,
    shared: BackendShared,
    closed: Arc<AtomicBool>,
}

impl RuntimeManager {
    /// Empty registry over shared backend state.
    #[must_use]
    pub fn new(shared: BackendShared) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner::default())),
            shared,
            closed: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Idle TTL for quiet actors.
    #[must_use]
    pub fn idle_ttl(&self) -> Duration {
        self.shared.runtime_idle_ttl
    }

    /// Activates the actor for indexed search: the root must resolve and
    /// carry a built index with a recorded embedding, mirroring TS
    /// `activate` (which throws `INDEX_MISSING` otherwise).
    ///
    /// # Errors
    ///
    /// Returns [`BackendError::Daemon`] when the manager is closed, the root is invalid,
    /// or no built index with an embedding exists, or [`BackendError::Engine`] when
    /// workspace inspection fails.
    pub async fn activate_for_search(
        &self,
        requested_root: &str,
    ) -> Result<RootHandle, BackendError> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(DaemonError::ShuttingDown.into());
        }
        let canonical = resolve_requested_root(requested_root, false)?;
        let info = self.inspect(&canonical)?;
        if !info.indexed || info.embedding.is_none() {
            return Err(DaemonError::IndexMissing {
                root: canonical.to_string(),
            }
            .into());
        }
        Ok(self.get_or_spawn(canonical))
    }

    /// Activates the actor for indexing: the root must resolve and be
    /// writable, with no index required (mirrors TS `activateForIndex`).
    ///
    /// # Errors
    ///
    /// Returns [`BackendError::Daemon`] when the manager is closed or the root is invalid
    /// or not writable.
    pub fn activate_for_index(&self, requested_root: &str) -> Result<RootHandle, BackendError> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(DaemonError::ShuttingDown.into());
        }
        let canonical = resolve_requested_root(requested_root, true)?;
        Ok(self.get_or_spawn(canonical))
    }

    /// Handle for a live actor, if any.
    #[must_use]
    pub fn get(&self, key: &RootKey) -> Option<RootHandle> {
        self.inner
            .lock_ignore_poison()
            .actors
            .get(key.as_str())
            .map(|entry| entry.handle.clone())
    }

    /// Live actor count (for server status).
    #[must_use]
    pub fn actor_count(&self) -> usize {
        self.inner.lock_ignore_poison().actors.len()
    }

    /// Removes a key without stopping anything (actor-initiated exit).
    /// Returns the join handle when the manager still owned the entry.
    #[must_use]
    pub fn unregister(&self, key: &RootKey) -> Option<JoinHandle<()>> {
        let mut inner = self.inner.lock_ignore_poison();
        let entry = inner.actors.remove(key.as_str())?;
        inner.aliases.retain(|_, target| target != key.as_str());
        entry.join
    }

    /// Stops and removes one actor, awaiting its teardown.
    pub async fn evict(&self, key: &RootKey) {
        let entry = self.inner.lock_ignore_poison().actors.remove(key.as_str());
        if let Some(entry) = entry {
            let _ = entry.handle.tx.send(RootCommand::Shutdown);
            if let Some(join) = entry.join {
                let _ = join.await;
            }
        }
        self.inner
            .lock_ignore_poison()
            .aliases
            .retain(|_, target| target != key.as_str());
    }

    /// Stops every actor, then the scheduler (which awaits in-flight
    /// blocking work per M6) and the model pool.
    pub async fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        let entries: Vec<ActorEntry> = self
            .inner
            .lock_ignore_poison()
            .actors
            .drain()
            .map(|(_, entry)| entry)
            .collect();
        self.inner.lock_ignore_poison().aliases.clear();
        for entry in &entries {
            let _ = entry.handle.tx.send(RootCommand::Shutdown);
        }
        // Joins awaited after every actor saw Shutdown: teardowns run
        // concurrently, and none blocks on another actor.
        for entry in entries {
            if let Some(join) = entry.join {
                let _ = join.await;
            }
        }
        self.shared.scheduler.close().await;
        self.shared.pool.close().await;
    }

    fn get_or_spawn(&self, canonical: RootKey) -> RootHandle {
        let inner = self.inner.lock_ignore_poison();
        let resolved = inner
            .aliases
            .get(canonical.as_str())
            .cloned()
            .unwrap_or_else(|| canonical.to_string());
        if let Some(entry) = inner.actors.get(&resolved) {
            return entry.handle.clone();
        }
        drop(inner);
        // Fresh actor; record the alias the caller used.
        let (tx, rx) = unbounded_channel();
        let handle = RootHandle {
            key: canonical.clone(),
            tx: tx.clone(),
        };
        let slf = self.clone();
        let key = canonical.clone();
        let join = tokio::spawn(async move {
            spawn_root_actor(slf.shared.clone(), slf.clone(), key, tx.clone(), rx).await;
        });
        let mut inner = self.inner.lock_ignore_poison();
        inner
            .aliases
            .insert(canonical.to_string(), resolved.clone());
        inner.actors.insert(
            resolved,
            ActorEntry {
                handle: handle.clone(),
                join: Some(join),
            },
        );
        handle
    }

    fn inspect(
        &self,
        canonical: &RootKey,
    ) -> Result<zg_core::service::types::ZvecGrepInfoResult, BackendError> {
        let service =
            ZvecGrepService::new(self.shared.service.facade_options(canonical.as_str(), None));
        Ok(service.workspace_info(None)?)
    }
}

/// Sends one actor command, mapping a dead actor to a shutdown error.
pub(crate) fn send_command(
    tx: &UnboundedSender<RootCommand>,
    command: RootCommand,
) -> Result<(), BackendError> {
    tx.send(command)
        .map_err(|_| BackendError::Daemon(DaemonError::ShuttingDown))
}
