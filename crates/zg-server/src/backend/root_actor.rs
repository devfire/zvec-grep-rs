//! Root actor: one task owning one root's index state.
//!
//! The actor holds [`RootRuntime`](crate::root_runtime::RootRuntime), its
//! [`IndexCoordinator`], [`WatchManager`], and read-session cache as plain
//! `&mut` state; sequential message processing replaces the TS
//! promise-chain generation indexing. Search-side freshness lives in
//! [`super::search`]; scheduler-run construction in
//! [`super::index_run`].

use std::path::PathBuf;

use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, error::TryRecvError};
use zg_core::index_status::{index_completion_for_job, index_completion_from_status};
use zg_core::lexical::{LexicalSearchOptions, LexicalSearchResult, run_lexical_search};
use zg_core::service::facade::ZvecGrepService;

use super::actor::{CachedSession, FinishedIndex, IndexOutcome, RootCommand};
use super::config::BackendShared;
use super::error::BackendError;
use super::index_run::build_index_run;
use super::request_types::{DaemonIndexStatus, IndexInput, RgQuery};
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
    pub(crate) rx: UnboundedReceiver<RootCommand>,
    pub(crate) tx: UnboundedSender<RootCommand>,
}

impl RootActor {
    pub(crate) async fn run(&mut self) {
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
                    // Drain debounced-but-unflushed watcher batches before
                    // deciding to exit: teardown drops pending changes, so
                    // exiting over an unflushed batch would silently lose
                    // it (#34).
                    self.watcher.flush_now().await;
                    if self.runtime.is_quiet() && !self.runtime.needs_reconciliation() {
                        // A flushed batch re-enters as `WatchBatch`; handle
                        // one queued command (if any) instead of exiting
                        // over it.
                        match self.rx.try_recv() {
                            Ok(cmd) => {
                                idle.as_mut().reset(tokio::time::Instant::now() + self.shared.runtime_idle_ttl);
                                if matches!(cmd, RootCommand::Shutdown) {
                                    break;
                                }
                                if !self.handle(cmd).await {
                                    break;
                                }
                            }
                            Err(
                                TryRecvError::Empty | TryRecvError::Disconnected,
                            ) => break,
                        }
                    } else {
                        idle.as_mut().reset(tokio::time::Instant::now() + self.shared.runtime_idle_ttl);
                    }
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
                let _ = reply.send(self.handle_index(input).await);
            }
            RootCommand::Status { reply } => {
                let _ = reply.send(self.handle_status().await);
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
                .map_err(join_backend_error)?
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

    async fn handle_status(&mut self) -> Result<DaemonIndexStatus, BackendError> {
        // Both engine reads below are blocking storage IO: each runs on a
        // blocking thread, never on this async worker (#44).
        let status = match self.status.clone() {
            Some(cached) => cached,
            None => {
                let service = self.temp_service();
                let fresh = tokio::task::spawn_blocking(move || service.index_status(None))
                    .await
                    .map_err(join_backend_error)?
                    .map_err(BackendError::Engine)?;
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
        let service = self.temp_service();
        let info = tokio::task::spawn_blocking(move || service.workspace_info(None).ok())
            .await
            .map_err(join_backend_error)?;
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
            whole_line: query.whole_line,
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
        let _ = self.shared.scheduler.cancel_root(self.key.as_str());
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
        match finished.outcome {
            IndexOutcome::Completed(ok) => {
                if finished.force_full {
                    let epoch = self.runtime.reconciliation_epoch();
                    self.runtime.mark_reconciled(finished.revision, epoch);
                } else {
                    self.runtime.mark_indexed(finished.revision);
                }
                self.scan = ok.index_result.scan_diagnostics.clone();
                self.status = Some(ok.status);
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
