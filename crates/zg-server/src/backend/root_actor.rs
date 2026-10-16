//! Root actor: one task owning one root's index state.
//!
//! The actor holds [`RootRuntime`](crate::root_runtime::RootRuntime), its
//! [`IndexCoordinator`], [`WatchManager`], and read-session cache as plain
//! `&mut` state; sequential message processing replaces the TS
//! promise-chain generation indexing. Search-side freshness lives in
//! [`super::search`]; scheduler-run construction in
//! [`super::index_run`].

use std::path::PathBuf;

use tokio::sync::mpsc::{Receiver, UnboundedReceiver, UnboundedSender, error::TryRecvError};
use zg_core::index_status::{index_completion_for_job, index_completion_from_status};
use zg_core::service::facade::ZvecGrepService;

use super::actor::{CachedSession, FinishedIndex, IndexOutcome, RootCommand};
use super::config::BackendShared;
use super::error::BackendError;
use super::index_run::build_index_run;
use super::request_types::{DaemonIndexStatus, IndexInput};
use super::util::join_backend_error;
use crate::change_set::ChangeSetSnapshot;
use crate::index_coordinator::{CoordinatorReason, IndexCoordinator};
use crate::job_scheduler::SubmitIndexJobResult;
use crate::read_session_cache::WorkspaceReadSessionCache;
use crate::root_runtime::{RootKey, RootRuntime};
use crate::runtime_manager::RuntimeManager;
use crate::watch_manager::{WatchManager, WatchReason};

pub(crate) struct RootActor {
    pub(crate) key: RootKey,
    pub(crate) generation: u64,
    pub(crate) shared: BackendShared,
    pub(crate) manager: RuntimeManager,
    pub(crate) runtime: RootRuntime,
    pub(crate) coordinator: IndexCoordinator,
    pub(crate) sessions: WorkspaceReadSessionCache<CachedSession>,
    pub(crate) watcher: WatchManager,
    pub(crate) status: Option<zg_core::types::WorkspaceIndexStatus>,
    pub(crate) scan: Option<zg_core::types::FileScanDiagnostics>,
    pub(crate) rx: Receiver<RootCommand>,
    pub(crate) internal_rx: UnboundedReceiver<RootCommand>,
    pub(crate) internal_tx: UnboundedSender<RootCommand>,
}

impl RootActor {
    pub(crate) async fn run(&mut self) {
        let mut idle = Box::pin(tokio::time::sleep(self.shared.runtime_idle_ttl));
        loop {
            tokio::select! {
                biased;
                Some(cmd) = self.internal_rx.recv() => {
                    idle.as_mut().reset(tokio::time::Instant::now() + self.shared.runtime_idle_ttl);
                    if !self.dispatch(cmd).await {
                        break;
                    }
                }
                Some(cmd) = self.rx.recv() => {
                    idle.as_mut().reset(tokio::time::Instant::now() + self.shared.runtime_idle_ttl);
                    if !self.dispatch(cmd).await {
                        break;
                    }
                }
                () = &mut idle => {
                    // Drain debounced-but-unflushed watcher batches before
                    // deciding to exit: teardown drops pending changes, so
                    // exiting over an unflushed batch would silently lose
                    // it (#34).
                    self.watcher.flush_now();
                    if self.runtime.is_quiet() && !self.runtime.needs_reconciliation() {
                        // A flushed batch re-enters as `WatchBatch`; handle
                        // one queued command (if any) instead of exiting
                        // over it. Internal delivery is checked first so a
                        // queued shutdown/finished is never skipped.
                        let queued = match self.internal_rx.try_recv() {
                            Ok(cmd) => Ok(cmd),
                            Err(TryRecvError::Empty) => self.rx.try_recv(),
                            Err(error) => Err(error),
                        };
                        match queued {
                            Ok(cmd) => {
                                idle.as_mut().reset(tokio::time::Instant::now() + self.shared.runtime_idle_ttl);
                                if !self.dispatch(cmd).await {
                                    break;
                                }
                            }
                            Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
                        }
                    } else {
                        idle.as_mut().reset(tokio::time::Instant::now() + self.shared.runtime_idle_ttl);
                    }
                }
            }
        }
        self.teardown().await;
    }

    /// Dispatches one command; returns false when the actor should exit
    /// (shutdown, or a completed drop — see `handle`).
    async fn dispatch(&mut self, cmd: RootCommand) -> bool {
        if matches!(cmd, RootCommand::Shutdown) {
            return false;
        }
        self.handle(cmd).await
    }

    /// Handles one command. Returns false when the actor should exit
    /// (after a completed drop).
    async fn handle(&mut self, cmd: RootCommand) -> bool {
        match cmd {
            RootCommand::Search { query, reply } => {
                let _ = reply.send(self.handle_search(query).await);
            }
            RootCommand::Index { input, reply } => {
                let _ = reply.send(self.handle_index(input).await);
            }
            RootCommand::Status { reply } => {
                let _ = reply.send(self.handle_status().await);
            }
            RootCommand::Drop { reply } => {
                let result = self.handle_drop().await;
                // Only a completed drop exits: on failure the index is
                // still on disk, so the actor (and its watcher) must
                // stay alive to keep serving it (#72).
                let completed = result.is_ok();
                let _ = reply.send(result);
                return !completed;
            }
            RootCommand::WatcherPending { pending } => {
                self.runtime.set_watcher_pending(pending);
            }
            RootCommand::WatcherActive { active } => {
                self.runtime.set_watcher_active(active);
                if !active {
                    // No live watcher: unseen edits are possible from
                    // now on, so a full reconciliation is owed and
                    // `watcher_active() == false` keeps searches from
                    // reading as fresh (#70).
                    self.runtime.require_full_reconciliation(false);
                }
            }
            RootCommand::WatchBatch { changes, reason } => {
                self.handle_watch_batch(&changes, reason);
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
        // Identity-checked (#33): a replacement spawned since this actor
        // exited keeps its own generation, so this is a no-op for it.
        let _ = self.manager.unregister(&self.key, self.generation);
    }

    pub(crate) fn temp_service(&self) -> ZvecGrepService {
        ZvecGrepService::new(self.shared.catalog_options(self.key.as_str()))
    }

    async fn handle_index(
        &mut self,
        input: IndexInput,
    ) -> Result<SubmitIndexJobResult, BackendError> {
        self.runtime.set_writer_pending(true);
        // Explicit rebuilds reconcile fully. An incremental request with no
        // paths reconciles whatever is pending: the run takes the pending
        // snapshot lazily, and an empty take completes as a fresh no-op
        // (see `apply_finished`) instead of rescanning the workspace —
        // except on a root with no index yet, where the first build must
        // scan everything (mirrors the engine's create-on-missing path).
        // The manifest read is blocking IO: it runs on a blocking thread,
        // never on this async worker (#44).
        let changes = if input.rebuild {
            ChangeSetSnapshot {
                force_full_reconcile: true,
                ..ChangeSetSnapshot::default()
            }
        } else if input.changed_paths.is_empty() {
            let service = self.temp_service();
            let indexed = tokio::task::spawn_blocking(move || service.workspace_info(None))
                .await
                .map_err(|error| join_backend_error(&error))?
                .is_ok_and(|info| info.indexed);
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
        let tx = self.internal_tx.clone();
        let key = self.key.clone();
        let shared = self.shared.clone();
        let embedding = input.embedding.clone();
        match self.coordinator.enqueue(
            &changes,
            CoordinatorReason::Reconcile,
            &mut self.runtime,
            &self.shared.scheduler,
            |take| build_index_run(tx, key, shared, self.sessions.clone(), take, embedding),
        ) {
            Ok(submitted) => Ok(submitted),
            Err(error) => {
                self.runtime.set_writer_pending(false);
                Err(error.into())
            }
        }
    }

    fn handle_watch_batch(&mut self, changes: &ChangeSetSnapshot, reason: WatchReason) {
        self.runtime.set_watcher_pending(false);
        let coordinator_reason = match reason {
            WatchReason::Watch => CoordinatorReason::Watch,
            WatchReason::Reconcile => CoordinatorReason::Reconcile,
        };
        let tx = self.internal_tx.clone();
        let key = self.key.clone();
        let shared = self.shared.clone();
        let result = self.coordinator.enqueue(
            changes,
            coordinator_reason,
            &mut self.runtime,
            &self.shared.scheduler,
            |take| build_index_run(tx, key, shared, self.sessions.clone(), take, None),
        );
        if result.is_ok() {
            self.runtime.set_writer_pending(true);
        }
    }

    async fn handle_status(&mut self) -> Result<DaemonIndexStatus, BackendError> {
        // Both engine reads below are blocking storage IO: each runs on a
        // blocking thread, never on this async worker (#44).
        let status = if let Some(cached) = self.status.clone() {
            cached
        } else {
            let service = self.temp_service();
            let fresh = tokio::task::spawn_blocking(move || service.index_status(None))
                .await
                .map_err(|error| join_backend_error(&error))?
                .map_err(BackendError::Engine)?;
            self.status = Some(fresh.clone());
            fresh
        };
        let job = self.shared.scheduler.get_by_root(self.key.as_str());
        let completion = index_completion_for_job(
            index_completion_from_status(Some(&status)),
            job.as_ref().map(|job| job.state),
            job.as_ref().and_then(|job| job.progress.as_ref()),
        );
        let snapshot = self.runtime.snapshot();
        let service = self.temp_service();
        let info = tokio::task::spawn_blocking(move || service.workspace_info(None).ok())
            .await
            .map_err(|error| join_backend_error(&error))?;
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

    async fn handle_drop(&mut self) -> Result<bool, BackendError> {
        // Cancel, then await the job's terminal state before touching
        // storage: `cancel_root` is request-only, and the running
        // blocking index holds the storage write lock until it unwinds,
        // so an immediate drop fails LOCK.BUSY (#72). The job's
        // `IndexFinished` queues in this actor's mailbox meanwhile;
        // scheduler completion does not depend on consuming it.
        let _ = self.shared.scheduler.cancel_root(self.key.as_str());
        self.shared
            .scheduler
            .wait_for_root_idle(self.key.as_str())
            .await;
        // Release the resident read session (its read lock would also
        // fail the drop) without closing the cache or the watcher: if
        // the drop fails, this actor stays alive and both must keep
        // working. `teardown` closes them after a completed drop.
        self.sessions.retire().await;
        let service = self.temp_service();
        let root = PathBuf::from(self.key.as_str());
        tokio::task::spawn_blocking(move || service.drop_index(Some(root.as_path())))
            .await
            .map_err(|error| join_backend_error(&error))?
            .map_err(BackendError::Engine)
    }

    pub(crate) fn drain_finished(&mut self) {
        let mut deferred = Vec::new();
        // IndexFinished arrives only on the internal channel; client
        // commands drained from the bounded mailbox are deferred too.
        // Re-queueing deferred commands on the lossless internal channel
        // preserves their relative order and cannot fail on a full
        // bounded mailbox refilled by concurrent clients (#93).
        loop {
            match self.internal_rx.try_recv() {
                Ok(RootCommand::IndexFinished { finished }) => {
                    self.apply_finished(*finished);
                }
                Ok(cmd) => deferred.push(cmd),
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
        loop {
            match self.rx.try_recv() {
                Ok(RootCommand::IndexFinished { finished }) => {
                    self.apply_finished(*finished);
                }
                Ok(cmd) => deferred.push(cmd),
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
        for cmd in deferred {
            let _ = self.internal_tx.send(cmd);
        }
    }

    fn apply_finished(&mut self, finished: FinishedIndex) {
        self.runtime.set_writer_pending(false);
        match finished.outcome {
            IndexOutcome::Completed(ok) => {
                if finished.force_full {
                    let epoch = self.runtime.reconciliation_epoch();
                    self.runtime.mark_reconciled(finished.revision, epoch);
                } else {
                    self.runtime.mark_indexed(finished.revision);
                }
                self.scan.clone_from(&ok.index_result.scan_diagnostics);
                self.status = Some(ok.status);
                // The run (re)wrote the manifest: the watcher's cached
                // root paths may predate the first index (empty) or a
                // rescope, so the next batch must re-snapshot (#70).
                self.watcher.mark_roots_stale();
            }
            IndexOutcome::Noop => {
                if !finished.force_full {
                    // Empty take: nothing was pending when the run took its
                    // snapshot, so the stamped target revision is already fresh.
                    // Without this, background reconciles that find no work would
                    // leave the dirty revision they bumped at enqueue time.
                    self.runtime.mark_indexed(finished.revision);
                }
            }
            IndexOutcome::Failed(error) => {
                // Failed run: leave the stamped revision dirty so the next
                // run retries the pending work instead of reading as fresh.
                // The typed payload stays with the outcome (rather than a
                // bare unit) so failures remain programmatically
                // inspectable; the scheduler snapshot is the observable
                // channel for the error itself.
                let _error = error;
            }
        }
    }
}
