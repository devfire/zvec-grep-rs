//! Public scheduler surface: construction, submit, queries, waiting, close.
//!
//! `Clone` shares one queue; every method takes `&self` and never holds
//! the state lock across an await. Dispatch, retry, and cancellation live
//! in the sibling `run` / `dedupe` modules as `pub(crate)` `impl`
//! blocks on this same type.

use std::collections::BTreeMap;
use std::sync::Arc;

use tokio::sync::watch;

use crate::errors::DaemonError;
use crate::logger::LogField;
use crate::sync::MutexExt;

use super::edge::{fields, on_progress_clone, safe_report};
use super::id::JobId;
use super::snapshot::{
    DEFAULT_MAX_ATTEMPTS, DEFAULT_RETRY_BASE_DELAY_MS, DEFAULT_SCHEDULER_CONCURRENCY,
    IndexJobSnapshot, JobSchedulerOptions, SchedulerLoad, SubmitIndexJob, SubmitIndexJobResult,
};
use super::state::{Inner, Shared, snapshot_missing};
use zg_core::index_status::IndexJobState;
use zg_core::pipeline::indexing::IndexProgressSink;

/// Per-root index job scheduler. `Clone` shares one queue; every method
/// takes `&self` and never holds the state lock across an await.
#[derive(Clone)]
pub struct JobScheduler {
    pub(crate) shared: Arc<Shared>,
}

/// Grace period `close()` grants in-flight bodies to observe cancellation
/// before it logs the stuck job ids and returns. A body that ignores
/// cancellation must not hang shutdown forever.
const CLOSE_GRACE_PERIOD: std::time::Duration = std::time::Duration::from_secs(5);

impl JobScheduler {
    /// Empty scheduler with the given options.
    #[must_use]
    pub fn new(options: JobSchedulerOptions) -> Self {
        Self {
            shared: Arc::new(Shared {
                state: std::sync::Mutex::new(Inner::default()),
                concurrency: options
                    .concurrency
                    .unwrap_or(DEFAULT_SCHEDULER_CONCURRENCY)
                    .max(1),
                max_attempts: options.max_attempts.unwrap_or(DEFAULT_MAX_ATTEMPTS).max(1),
                retry_base_delay_ms: options
                    .retry_base_delay_ms
                    .unwrap_or(DEFAULT_RETRY_BASE_DELAY_MS),
                logger: options.logger,
            }),
        }
    }

    /// Submits work for `input.canonical_root`, deduping per root exactly
    /// like TS `submit`: a queued job absorbs watch/followup runs and
    /// priority upgrades; a running job gains a chained followup for
    /// watch/followup runs; anything else reuses the active snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError::ShuttingDown`] when the scheduler is closed.
    pub fn submit(&self, input: SubmitIndexJob) -> Result<SubmitIndexJobResult, DaemonError> {
        let root = input.canonical_root.clone();
        let mut state = self.shared.state.lock_ignore_poison();
        if state.closed {
            return Err(DaemonError::ShuttingDown);
        }
        if let Some(active_id) = state.active_by_root.get(&input.canonical_root).cloned() {
            let reused = self.absorb_or_chain(&mut state, &active_id, input);
            let snapshot = state.jobs.get(&reused).map_or_else(
                || snapshot_missing(&reused),
                super::state::JobRecord::snapshot,
            );
            Self::evict_completed_for_root(&mut state, &root);
            return Ok(SubmitIndexJobResult {
                job: snapshot,
                reused: true,
            });
        }
        let id = self.enqueue(&mut state, input);
        let snapshot = state
            .jobs
            .get(&id)
            .map_or_else(|| snapshot_missing(&id), super::state::JobRecord::snapshot);
        Self::evict_completed_for_root(&mut state, &root);
        drop(state);
        self.pump();
        Ok(SubmitIndexJobResult {
            job: snapshot,
            reused: false,
        })
    }

    /// Latest snapshot for a root, if any.
    pub fn get_by_root(&self, canonical_root: &str) -> Option<IndexJobSnapshot> {
        let state = self.shared.state.lock_ignore_poison();
        state
            .latest_by_root
            .get(canonical_root)
            .and_then(|id| state.jobs.get(id))
            .map(super::state::JobRecord::snapshot)
    }

    /// True while a job for the root is queued or running.
    #[must_use]
    pub fn has_active_root(&self, canonical_root: &str) -> bool {
        self.shared
            .state
            .lock_ignore_poison()
            .active_by_root
            .contains_key(canonical_root)
    }

    /// Snapshot by job id.
    pub fn get(&self, id: &JobId) -> Option<IndexJobSnapshot> {
        self.shared
            .state
            .lock_ignore_poison()
            .jobs
            .get(id)
            .map(super::state::JobRecord::snapshot)
    }

    /// Awaits a terminal snapshot, optionally streaming progress. Progress
    /// observers never affect the outcome: panics inside `on_progress` are
    /// contained per call.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError::UnknownJob`] when the id is unknown, or
    /// [`DaemonError::IndexCancelled`] when the completion channel closes first.
    pub async fn wait(
        &self,
        id: &JobId,
        on_progress: Option<IndexProgressSink>,
    ) -> Result<IndexJobSnapshot, DaemonError> {
        let (receiver, replay) = {
            let mut state = self.shared.state.lock_ignore_poison();
            let Some(job) = state.jobs.get_mut(id) else {
                return Err(DaemonError::UnknownJob { id: id.to_string() });
            };
            if let Some(sink) = &on_progress {
                job.listeners.push(sink.clone());
            }
            (job.completed.subscribe(), job.progress.clone())
        };
        if let Some(progress) = replay {
            safe_report(&on_progress, &progress);
        }
        let result = self.await_terminal(receiver).await;
        if on_progress.is_some() {
            let mut state = self.shared.state.lock_ignore_poison();
            if let Some(job) = state.jobs.get_mut(id) {
                job.listeners
                    .retain(|listener| !Arc::ptr_eq(listener, &on_progress_clone(&on_progress)));
            }
        }
        result
    }

    /// Awaits quiescence for one root (active job plus any chained
    /// followup), mirroring TS `waitForRootIdle`.
    pub async fn wait_for_root_idle(&self, canonical_root: &str) {
        loop {
            let receiver = {
                let state = self.shared.state.lock_ignore_poison();
                let Some(id) = state.active_by_root.get(canonical_root).cloned() else {
                    return;
                };
                state.jobs.get(&id).map(|job| job.completed.subscribe())
            };
            if let Some(receiver) = receiver {
                let _ = self.await_terminal(receiver).await;
            }
        }
    }

    /// Cancels the active job for a root. Returns false when idle.
    #[must_use]
    pub fn cancel_root(&self, canonical_root: &str) -> bool {
        let id = self
            .shared
            .state
            .lock_ignore_poison()
            .active_by_root
            .get(canonical_root)
            .cloned();
        if let Some(id) = id {
            self.cancel_job(&id, "indexing was cancelled");
            true
        } else {
            false
        }
    }

    /// Current queue depth.
    #[must_use]
    pub fn load(&self) -> SchedulerLoad {
        let state = self.shared.state.lock_ignore_poison();
        SchedulerLoad {
            queued: state
                .jobs
                .values()
                .filter(|job| job.state == IndexJobState::Queued)
                .count(),
            running: state.running,
        }
    }

    /// Cancels everything, clears the queue, and awaits in-flight work —
    /// including `spawn_blocking` bodies, which cannot be aborted and must
    /// be awaited rather than orphaned.
    ///
    /// The wait is bounded by [`CLOSE_GRACE_PERIOD`]: a body that ignores
    /// cancellation is logged with its stuck job ids and left to finish on
    /// its own instead of hanging shutdown forever.
    pub async fn close(&self) {
        {
            let mut state = self.shared.state.lock_ignore_poison();
            if state.closed {
                return;
            }
            state.closed = true;
            state.queue.clear();
        }
        let ids: Vec<JobId> = self
            .shared
            .state
            .lock_ignore_poison()
            .jobs
            .keys()
            .cloned()
            .collect();
        for id in &ids {
            self.cancel_job(
                id,
                "indexing was cancelled because the daemon is shutting down",
            );
        }
        let deadline = tokio::time::Instant::now() + CLOSE_GRACE_PERIOD;
        loop {
            let running = self.shared.state.lock_ignore_poison().running;
            if running == 0 {
                return;
            }
            if tokio::time::Instant::now() >= deadline {
                let (stuck, running) = {
                    let state = self.shared.state.lock_ignore_poison();
                    let stuck: Vec<String> = state
                        .jobs
                        .values()
                        .filter(|job| {
                            !matches!(
                                job.state,
                                IndexJobState::Succeeded
                                    | IndexJobState::Failed
                                    | IndexJobState::Cancelled
                            )
                        })
                        .map(|job| job.id.to_string())
                        .collect();
                    (stuck, state.running)
                };
                self.log(
                    "scheduler.close.timeout",
                    fields([
                        ("job_ids", LogField::from(stuck.join(", "))),
                        ("running", LogField::from(running)),
                    ]),
                );
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    }

    /// Drops terminal records for `root` that are neither the active job
    /// nor the latest snapshot, so completed jobs (watch senders,
    /// listeners, run closures) cannot accumulate one entry per submission
    /// forever. Queued and running jobs are never touched.
    fn evict_completed_for_root(state: &mut Inner, root: &str) {
        let active = state.active_by_root.get(root).cloned();
        let latest = state.latest_by_root.get(root).cloned();
        let doomed: Vec<JobId> = state
            .jobs
            .values()
            .filter(|job| {
                job.canonical_root.as_str() == root
                    && matches!(
                        job.state,
                        IndexJobState::Succeeded | IndexJobState::Failed | IndexJobState::Cancelled
                    )
                    && !active.as_ref().is_some_and(|id| *id == job.id)
                    && !latest.as_ref().is_some_and(|id| *id == job.id)
            })
            .map(|job| job.id.clone())
            .collect();
        for id in &doomed {
            state.jobs.remove(id);
            state.queue.retain(|queued| queued != id);
        }
    }

    async fn await_terminal(
        &self,
        mut receiver: watch::Receiver<IndexJobSnapshot>,
    ) -> Result<IndexJobSnapshot, DaemonError> {
        loop {
            let snapshot = receiver.borrow().clone();
            if snapshot.is_terminal() {
                return Ok(snapshot);
            }
            receiver
                .changed()
                .await
                .map_err(|_| DaemonError::IndexCancelled)?;
        }
    }

    pub(crate) fn log(&self, name: &str, fields: BTreeMap<String, LogField>) {
        if let Some(logger) = &self.shared.logger {
            logger.event(name, fields);
        }
    }
}
