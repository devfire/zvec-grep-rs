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

use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::mpsc::{Sender, channel, unbounded_channel};
use tokio::task::JoinHandle;
use zg_core::service::facade::ZvecGrepService;

use crate::backend::{
    BackendError, BackendShared, RootCommand, RootHandle, join_backend_error, spawn_root_actor,
};
use crate::errors::DaemonError;
use crate::root_runtime::{RootKey, resolve_requested_root};
use crate::sync::MutexExt;

/// Idle TTL before a quiet actor exits itself; mirrors TS `runtimeIdleTtlMs`.
pub const DEFAULT_RUNTIME_IDLE_TTL: Duration = Duration::from_secs(30 * 60);

/// Bounded client-command mailbox per root actor (#93). Client commands
/// (`Search`/`Index`/`Status`/`Drop`) beyond this depth are rejected with
/// `ACTOR_BUSY` instead of queueing unboundedly; watcher, index-finished,
/// and shutdown delivery uses a separate unbounded internal channel.
pub const ROOT_ACTOR_MAILBOX_CAPACITY: usize = 64;

struct ActorEntry {
    handle: RootHandle,
    join: Option<JoinHandle<()>>,
    generation: u64,
}
#[derive(Default)]
struct Inner {
    actors: HashMap<String, ActorEntry>,
    aliases: HashMap<String, String>,
    next_generation: u64,
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
        // A live actor already passed this validation at activation and
        // exits when its index is dropped, so re-opening the index on
        // every search would only add a blocking storage open (and read
        // lock) per query (#69). Only cold activation inspects, and it
        // does so on a blocking thread (see `inspect`).
        if let Some(handle) = self.get(&canonical) {
            return Ok(handle);
        }
        let info = self.inspect(&canonical).await?;
        if !info.indexed || info.embedding.is_none() {
            return Err(DaemonError::IndexMissing {
                root: canonical.to_string(),
            }
            .into());
        }
        self.get_or_spawn(&canonical)
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
        self.get_or_spawn(&canonical)
    }

    /// Resolves a root for a read-only command: canonical key plus the
    /// live actor handle when one already exists. Never spawns an actor
    /// and never requires writability, so read-only queries cannot start
    /// a watcher or trigger a catch-up rebuild (#68).
    ///
    /// # Errors
    ///
    /// Returns [`BackendError::Daemon`] when the manager is closed or the
    /// root is invalid or unreadable.
    pub fn resolve_live(
        &self,
        requested_root: &str,
    ) -> Result<(RootKey, Option<RootHandle>), BackendError> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(DaemonError::ShuttingDown.into());
        }
        let canonical = resolve_requested_root(requested_root, false)?;
        let handle = self.get(&canonical);
        Ok((canonical, handle))
    }

    /// Facade service bound to `key` (blocking IO: call off async workers).
    pub(crate) fn facade_service(&self, key: &RootKey) -> ZvecGrepService {
        ZvecGrepService::new(self.shared.service.facade_options(key.as_str(), None))
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
    /// Compare-and-remove on the spawn generation (#33): a stale teardown
    /// whose root was already replaced is a no-op instead of deleting the
    /// live replacement. Aliases drop only with a matching removal.
    #[must_use]
    pub fn unregister(&self, key: &RootKey, generation: u64) -> Option<JoinHandle<()>> {
        let mut inner = self.inner.lock_ignore_poison();
        let matches = inner
            .actors
            .get(key.as_str())
            .is_some_and(|entry| entry.generation == generation);
        if !matches {
            return None;
        }
        let entry = inner.actors.remove(key.as_str())?;
        inner.aliases.retain(|_, target| target != key.as_str());
        entry.join
    }
    /// Stops and removes one actor, awaiting its teardown. The removed entry
    /// carries its spawn generation into the actor task, so a replacement
    /// spawned mid-teardown gets a fresh generation and the stale teardown's
    /// identity-checked [`Self::unregister`] cannot remove it (#33).
    pub async fn evict(&self, key: &RootKey) {
        let entry = self.inner.lock_ignore_poison().actors.remove(key.as_str());
        if let Some(entry) = entry {
            let _ = entry.handle.internal_tx.send(RootCommand::Shutdown);
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
    /// blocking work per M6) and the model pool. Concurrent calls coalesce:
    /// the first caller drains while the rest return immediately.
    pub async fn close(&self) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        let entries: Vec<ActorEntry> = self
            .inner
            .lock_ignore_poison()
            .actors
            .drain()
            .map(|(_, entry)| entry)
            .collect();
        self.inner.lock_ignore_poison().aliases.clear();
        for entry in &entries {
            let _ = entry.handle.internal_tx.send(RootCommand::Shutdown);
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

    fn get_or_spawn(&self, canonical: &RootKey) -> Result<RootHandle, BackendError> {
        {
            let inner = self.inner.lock_ignore_poison();
            if self.closed.load(Ordering::SeqCst) {
                return Err(DaemonError::ShuttingDown.into());
            }
            let resolved = inner
                .aliases
                .get(canonical.as_str())
                .cloned()
                .unwrap_or_else(|| canonical.to_string());
            if let Some(entry) = inner.actors.get(&resolved) {
                return Ok(entry.handle.clone());
            }
        }
        // Fresh actor; record the alias the caller used. The generation is
        // claimed up front so the spawned task carries its own identity
        // even if a sibling wins the insert race below (#33).
        let generation = {
            let mut inner = self.inner.lock_ignore_poison();
            let generation = inner.next_generation;
            inner.next_generation = generation.wrapping_add(1);
            generation
        };
        let (tx, rx) = channel(ROOT_ACTOR_MAILBOX_CAPACITY);
        let (internal_tx, internal_rx) = unbounded_channel();
        let handle = RootHandle {
            key: canonical.clone(),
            tx: tx.clone(),
            internal_tx: internal_tx.clone(),
            generation,
        };
        let manager = self.clone();
        let key = canonical.clone();
        let join = tokio::spawn(async move {
            spawn_root_actor(
                manager.shared.clone(),
                manager.clone(),
                key,
                generation,
                internal_tx.clone(),
                rx,
                internal_rx,
            )
            .await;
        });
        let mut inner = self.inner.lock_ignore_poison();
        // close() drains under this lock and sets `closed` first: a spawn
        if self.closed.load(Ordering::SeqCst) {
            join.abort();
            return Err(DaemonError::ShuttingDown.into());
        }
        // A sibling may have inserted while this task spawned: keep the
        // first actor and abort the newcomer so only one watcher runs.
        let resolved = inner
            .aliases
            .get(canonical.as_str())
            .cloned()
            .unwrap_or_else(|| canonical.to_string());
        if let Some(entry) = inner.actors.get(&resolved) {
            join.abort();
            return Ok(entry.handle.clone());
        }
        inner
            .aliases
            .insert(canonical.to_string(), resolved.clone());
        inner.actors.insert(
            resolved,
            ActorEntry {
                handle: handle.clone(),
                join: Some(join),
                generation,
            },
        );
        Ok(handle)
    }

    async fn inspect(
        &self,
        canonical: &RootKey,
    ) -> Result<zg_core::service::types::ZvecGrepInfoResult, BackendError> {
        // `workspace_info` opens the full index read-only and takes a read
        // lock: blocking storage IO, never on an async worker (#69).
        let service = self.facade_service(canonical);
        let info = tokio::task::spawn_blocking(move || service.workspace_info(None))
            .await
            .map_err(|error| join_backend_error(&error))??;
        Ok(info)
    }
}

/// Sends one client actor command without queueing unboundedly: a full
/// bounded mailbox is `ACTOR_BUSY`, a dead actor is a shutdown error (#93).
pub(crate) fn send_command(
    tx: &Sender<RootCommand>,
    root: &str,
    command: RootCommand,
) -> Result<(), BackendError> {
    match tx.try_send(command) {
        Ok(()) => Ok(()),
        Err(TrySendError::Full(_)) => Err(BackendError::Daemon(DaemonError::ActorBusy {
            root: root.to_owned(),
        })),
        Err(TrySendError::Closed(_)) => Err(BackendError::Daemon(DaemonError::ShuttingDown)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::backend::ServiceConfig;
    use crate::job_scheduler::{JobScheduler, JobSchedulerOptions};
    use crate::model_pool::{EmbeddingModelPool, EmbeddingModelPoolOptions};

    fn test_manager() -> RuntimeManager {
        let scheduler = JobScheduler::new(JobSchedulerOptions::default());
        let pool = EmbeddingModelPool::new(EmbeddingModelPoolOptions::default());
        let shared = BackendShared {
            scheduler,
            pool,
            service: ServiceConfig::default(),
            auth: Arc::default(),
            logger: None,
            read_session_ttl: crate::read_session_cache::DEFAULT_READ_SESSION_IDLE_TTL,
            runtime_idle_ttl: DEFAULT_RUNTIME_IDLE_TTL,
        };
        RuntimeManager::new(shared)
    }

    fn test_key() -> (tempfile::TempDir, RootKey) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir
            .path()
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let key = RootKey::parse(&path).unwrap();
        (dir, key)
    }

    #[tokio::test]
    async fn parallel_get_or_spawn_yields_one_actor() {
        let manager = test_manager();
        let (_dir, key) = test_key();
        let barrier = Arc::new(tokio::sync::Barrier::new(16));
        let mut tasks = Vec::new();
        for _ in 0..16 {
            let manager = manager.clone();
            let key = key.clone();
            let barrier = barrier.clone();
            tasks.push(tokio::spawn(async move {
                barrier.wait().await;
                manager.get_or_spawn(&key).unwrap()
            }));
        }
        let mut handles = Vec::new();
        for task in tasks {
            handles.push(task.await.unwrap());
        }
        assert_eq!(manager.actor_count(), 1);
        let first = handles.first().unwrap();
        for handle in &handles {
            assert!(first.tx.same_channel(&handle.tx));
        }
        manager.close().await;
    }

    #[test]
    fn full_client_mailbox_rejects_busy_instead_of_queueing() {
        // Capacity-1 stand-in for the bounded client mailbox: the first
        // command fills it, the second must fail fast with ACTOR_BUSY
        // rather than queue behind a busy actor (#93).
        let (tx, _rx) = channel(1);
        send_command(&tx, "/repo", RootCommand::Shutdown).unwrap();
        let error = send_command(&tx, "/repo", RootCommand::Shutdown).unwrap_err();
        assert_eq!(error.code(), "ACTOR_BUSY");
        assert!(matches!(
            error,
            BackendError::Daemon(DaemonError::ActorBusy { .. })
        ));
    }

    #[tokio::test]
    async fn spawn_after_close_returns_shutting_down() {
        let manager = test_manager();
        let (_dir, key) = test_key();
        manager.close().await;
        let result = manager.get_or_spawn(&key);
        assert!(matches!(
            result,
            Err(BackendError::Daemon(DaemonError::ShuttingDown))
        ));
        // Duplicate close() calls coalesce instead of double-draining.
        manager.close().await;
    }

    #[tokio::test]
    async fn activate_for_search_unindexed_returns_index_missing_without_spawning() {
        let manager = test_manager();
        let (dir, _key) = test_key();
        let root = dir.path().to_string_lossy().into_owned();
        // Exercises the spawn_blocking inspect path (#69): an unindexed
        // root is rejected before any actor (watcher) is spawned.
        let result = manager.activate_for_search(&root).await;
        assert!(matches!(
            result,
            Err(BackendError::Daemon(DaemonError::IndexMissing { .. }))
        ));
        assert_eq!(manager.actor_count(), 0);
        manager.close().await;
    }

    #[tokio::test]
    async fn concurrent_close_is_safe() {
        let manager = test_manager();
        let (_dir, key) = test_key();
        let _handle = manager.get_or_spawn(&key).unwrap();
        assert_eq!(manager.actor_count(), 1);
        let first = manager.clone();
        let second = manager.clone();
        let ((), ()) = tokio::join!(first.close(), second.close());
        assert_eq!(manager.actor_count(), 0);
    }

    #[tokio::test]
    async fn stale_unregister_keeps_live_replacement() {
        let manager = test_manager();
        let (_dir, key) = test_key();
        let first = manager.get_or_spawn(&key).unwrap();
        // The live actor exits; its replacement spawns with a fresh
        // generation. Replaying the stale teardown must be a no-op (#33).
        manager.evict(&key).await;
        let second = manager.get_or_spawn(&key).unwrap();
        assert_ne!(first.generation, second.generation);
        assert!(manager.unregister(&key, first.generation).is_none());
        assert!(manager.get(&key).is_some());
        assert!(manager.unregister(&key, second.generation).is_some());
        assert!(manager.get(&key).is_none());
        manager.close().await;
    }
}
