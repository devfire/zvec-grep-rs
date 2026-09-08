//! Per-root index job scheduling: dedupe, priority, retry, cooperative cancel.
//!
//! Mirrors `../zvec-grep/src/daemon/job-scheduler.ts` (`JobScheduler`:
//! per-root active/latest maps, priority queue, `maxAttempts` retry with
//! exponential backoff, watch-reason followups, progress listeners).
//! Two deliberate divergences (see `docs/ts-divergence.md`):
//!
//! - `AbortController`/`AbortSignal` become
//!   [`CancellationToken`](tokio_util::sync::CancellationToken) at the
//!   async edge; the sync index body only ever sees a [`CancelFlag`](zg_core::pipeline::indexing::scanner::CancelFlag)
//!   via [`bridge_cancellation`] (M6). Dropping a `spawn_blocking` handle
//!   cannot abort it, so shutdown awaits in-flight work instead of
//!   orphaning it.
//! - The TS promise-chain generation idiom is sequential task execution:
//!   one task per job, one followup slot per root.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use futures::future::BoxFuture;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use zg_core::error::EngineError;
use zg_core::index_status::IndexJobState;
use zg_core::pipeline::indexing::IndexProgressSink;
use zg_core::pipeline::indexing::scanner::CancelFlag;
use zg_core::types::IndexProgress;

use crate::errors::DaemonError;
use crate::logger::{DaemonLogger, LogField, root_identity};

/// How many index jobs run concurrently by default.
pub const DEFAULT_SCHEDULER_CONCURRENCY: usize = 1;

/// Default retry budget per job.
pub const DEFAULT_MAX_ATTEMPTS: u32 = 3;

/// Base delay for exponential retry backoff.
pub const DEFAULT_RETRY_BASE_DELAY_MS: u64 = 250;

/// Why an index job was submitted. Priority order mirrors TS `priority`:
/// manual (4) > fresh-query (3) > watch (2) > reconcile/background (1).
/// Serialized snake_case (`fresh_query`), matching the TS wire strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JobReason {
    /// Filesystem watcher event.
    Watch,
    /// Periodic or resume reconciliation.
    Reconcile,
    /// Low-priority background reconciliation.
    BackgroundReconcile,
    /// Explicit user/CLI request.
    Manual,
    /// A query observed a stale index.
    FreshQuery,
}

impl JobReason {
    /// Queue priority: higher runs first; ties break by creation time.
    const fn priority(self) -> u32 {
        match self {
            Self::Manual => 4,
            Self::FreshQuery => 3,
            Self::Watch => 2,
            Self::Reconcile | Self::BackgroundReconcile => 1,
        }
    }
}

/// Opaque job handle, a UUID string on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize)]
pub struct JobId(String);

impl JobId {
    /// Raw id string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for JobId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Terminal failure of one job run. The scheduler maps this to an
/// [`IndexJobError`] snapshot and decides retryability from it.
#[derive(Debug)]
pub enum JobFailure {
    /// Cooperative cancel won (or the token was already cancelled).
    Cancelled,
    /// Typed daemon failure.
    Daemon(DaemonError),
    /// Engine failure (only `LOCK.BUSY` retries).
    Engine(EngineError),
    /// Untyped failure; recorded as `INDEX_FAILED`.
    Failed(String),
}

impl From<DaemonError> for JobFailure {
    fn from(error: DaemonError) -> Self {
        if error == DaemonError::IndexCancelled {
            Self::Cancelled
        } else {
            Self::Daemon(error)
        }
    }
}

impl From<EngineError> for JobFailure {
    fn from(error: EngineError) -> Self {
        Self::Engine(error)
    }
}

/// Outcome of one [`JobRun`]: success or a [`JobFailure`].
pub type JobOutcome = Result<(), JobFailure>;

/// One unit of index work. Receives an owned progress sink and a
/// cancellation token; returns success or a [`JobFailure`]. Must be
/// cooperative: check the token (or the bridged [`CancelFlag`]) and return
/// [`JobFailure::Cancelled`] promptly.
pub type JobRun = Arc<
    dyn Fn(IndexProgressSink, CancellationToken) -> BoxFuture<'static, JobOutcome> + Send + Sync,
>;

/// Redacted, code-shaped job error carried in snapshots.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexJobError {
    /// Frozen code (`INDEX_*`, daemon codes, or qualified engine codes).
    pub code: String,
    /// Redacted one-line message (512 chars max).
    pub message: String,
    /// Redacted engine context block, when present.
    pub context: Option<String>,
    /// Redacted cause chain summary, when present.
    pub cause: Option<String>,
}

/// Observable snapshot of one job (mirrors TS `IndexJobSnapshot`).
/// `Eq` is impossible: `IndexProgress` carries `f64` scores.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexJobSnapshot {
    /// Opaque job id.
    pub id: JobId,
    /// Canonical root the job indexes.
    pub canonical_root: String,
    /// Why the job was submitted.
    pub reason: JobReason,
    /// Lifecycle state.
    pub state: IndexJobState,
    /// 1-based attempt counter.
    pub attempt: u32,
    /// Submission time, unix millis.
    pub created_at_ms: u64,
    /// First start time, unix millis.
    pub started_at_ms: Option<u64>,
    /// Terminal time, unix millis.
    pub finished_at_ms: Option<u64>,
    /// Latest progress report.
    pub progress: Option<IndexProgress>,
    /// Terminal error, if any.
    pub error: Option<IndexJobError>,
}

impl IndexJobSnapshot {
    /// True once the job reached succeeded/failed/cancelled.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self.state,
            IndexJobState::Succeeded | IndexJobState::Failed | IndexJobState::Cancelled
        )
    }
}

/// Input to [`JobScheduler::submit`].
pub struct SubmitIndexJob {
    /// Canonical root to index.
    pub canonical_root: String,
    /// Submission reason (drives dedupe and priority).
    pub reason: JobReason,
    /// Work to run.
    pub run: JobRun,
    /// When a job is already active for the root, chain after it instead
    /// of reusing its snapshot.
    pub followup_if_running: bool,
}

/// Result of [`JobScheduler::submit`].
pub struct SubmitIndexJobResult {
    /// Current snapshot (fresh or reused).
    pub job: IndexJobSnapshot,
    /// True when an existing job was reused or chained.
    pub reused: bool,
}

/// Options for [`JobScheduler`].
#[derive(Debug, Clone, Default)]
pub struct JobSchedulerOptions {
    /// Concurrent running jobs; defaults to 1.
    pub concurrency: Option<usize>,
    /// Attempts per job including the first; defaults to 3.
    pub max_attempts: Option<u32>,
    /// Base retry delay; attempt N waits `base * 2^(N-1)`.
    pub retry_base_delay_ms: Option<u64>,
    /// Daemon logger for `job.started` / `job.retry` / `job.finished`.
    pub logger: Option<DaemonLogger>,
}

/// Queue-depth snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SchedulerLoad {
    /// Jobs waiting for a slot.
    pub queued: usize,
    /// Jobs currently running.
    pub running: usize,
}

struct JobRecord {
    id: JobId,
    canonical_root: String,
    reason: JobReason,
    state: IndexJobState,
    attempt: u32,
    created_at_ms: u64,
    started_at_ms: Option<u64>,
    finished_at_ms: Option<u64>,
    progress: Option<IndexProgress>,
    error: Option<IndexJobError>,
    run: JobRun,
    cancel: CancellationToken,
    completed: watch::Sender<IndexJobSnapshot>,
    listeners: Vec<IndexProgressSink>,
    followup: Option<JobId>,
    retry_pending: bool,
}

impl JobRecord {
    fn snapshot(&self) -> IndexJobSnapshot {
        IndexJobSnapshot {
            id: self.id.clone(),
            canonical_root: self.canonical_root.clone(),
            reason: self.reason,
            state: self.state,
            attempt: self.attempt,
            created_at_ms: self.created_at_ms,
            started_at_ms: self.started_at_ms,
            finished_at_ms: self.finished_at_ms,
            progress: self.progress.clone(),
            error: self.error.clone(),
        }
    }

    fn publish(&self) {
        // `send_replace`, not `send`: since tokio 1.53 `send` drops the
        // value when no receiver exists, which would leave late waiters
        // staring at a stale slot forever. The slot must always hold the
        // latest snapshot.
        self.completed.send_replace(self.snapshot());
    }
}

#[derive(Default)]
struct Inner {
    jobs: HashMap<JobId, JobRecord>,
    active_by_root: HashMap<String, JobId>,
    latest_by_root: HashMap<String, JobId>,
    queue: Vec<JobId>,
    running: usize,
    closed: bool,
}

struct Shared {
    state: Mutex<Inner>,
    concurrency: usize,
    max_attempts: u32,
    retry_base_delay_ms: u64,
    logger: Option<DaemonLogger>,
}

/// Per-root index job scheduler. `Clone` shares one queue; every method
/// takes `&self` and never holds the state lock across an await.
#[derive(Clone)]
pub struct JobScheduler {
    shared: Arc<Shared>,
}

impl JobScheduler {
    /// Empty scheduler with the given options.
    pub fn new(options: JobSchedulerOptions) -> Self {
        Self {
            shared: Arc::new(Shared {
                state: Mutex::new(Inner::default()),
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
    pub fn submit(&self, input: SubmitIndexJob) -> Result<SubmitIndexJobResult, DaemonError> {
        let mut state = lock(&self.shared.state);
        if state.closed {
            return Err(DaemonError::ShuttingDown);
        }
        if let Some(active_id) = state.active_by_root.get(&input.canonical_root).cloned() {
            let reused = self.absorb_or_chain(&mut state, &active_id, input);
            let snapshot = state
                .jobs
                .get(&reused)
                .map_or_else(|| snapshot_missing(&reused), JobRecord::snapshot);
            return Ok(SubmitIndexJobResult {
                job: snapshot,
                reused: true,
            });
        }
        let id = self.enqueue(&mut state, input);
        let snapshot = state
            .jobs
            .get(&id)
            .map_or_else(|| snapshot_missing(&id), JobRecord::snapshot);
        drop(state);
        self.pump();
        Ok(SubmitIndexJobResult {
            job: snapshot,
            reused: false,
        })
    }

    /// Latest snapshot for a root, if any.
    pub fn get_by_root(&self, canonical_root: &str) -> Option<IndexJobSnapshot> {
        let state = lock(&self.shared.state);
        state
            .latest_by_root
            .get(canonical_root)
            .and_then(|id| state.jobs.get(id))
            .map(JobRecord::snapshot)
    }

    /// True while a job for the root is queued or running.
    pub fn has_active_root(&self, canonical_root: &str) -> bool {
        lock(&self.shared.state)
            .active_by_root
            .contains_key(canonical_root)
    }

    /// Snapshot by job id.
    pub fn get(&self, id: &JobId) -> Option<IndexJobSnapshot> {
        lock(&self.shared.state)
            .jobs
            .get(id)
            .map(JobRecord::snapshot)
    }

    /// Awaits a terminal snapshot, optionally streaming progress. Progress
    /// observers never affect the outcome: panics inside `on_progress` are
    /// contained per call.
    pub async fn wait(
        &self,
        id: &JobId,
        on_progress: Option<IndexProgressSink>,
    ) -> Result<IndexJobSnapshot, DaemonError> {
        let (receiver, replay) = {
            let mut state = lock(&self.shared.state);
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
            let mut state = lock(&self.shared.state);
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
                let state = lock(&self.shared.state);
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
    pub fn cancel_root(&self, canonical_root: &str) -> bool {
        let id = lock(&self.shared.state)
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
    pub fn load(&self) -> SchedulerLoad {
        let state = lock(&self.shared.state);
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
    /// be awaited rather than orphaned (M6).
    pub async fn close(&self) {
        {
            let mut state = lock(&self.shared.state);
            if state.closed {
                return;
            }
            state.closed = true;
            state.queue.clear();
        }
        let ids: Vec<JobId> = lock(&self.shared.state).jobs.keys().cloned().collect();
        for id in &ids {
            self.cancel_job(
                id,
                "indexing was cancelled because the daemon is shutting down",
            );
        }
        loop {
            let running = lock(&self.shared.state).running;
            if running == 0 {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    }

    fn absorb_or_chain(
        &self,
        state: &mut Inner,
        active_id: &JobId,
        input: SubmitIndexJob,
    ) -> JobId {
        let SubmitIndexJob {
            canonical_root: _,
            reason: incoming_reason,
            run: incoming_run,
            followup_if_running,
        } = input;
        // One owner for the incoming run across the exclusive branches
        // below: each branch takes it at most once and always returns.
        let mut incoming_run = Some(incoming_run);
        let chainable = incoming_reason == JobReason::Watch || followup_if_running;
        let (queued_without_retry, followup_id) = state
            .jobs
            .get(active_id)
            .map(|job| {
                (
                    job.state == IndexJobState::Queued && !job.retry_pending,
                    job.followup.clone(),
                )
            })
            .unwrap_or((false, None));
        if queued_without_retry {
            // A queued job absorbs watch/followup runs and priority
            // upgrades in place (mirrors TS `submit`). A manual resubmit
            // reuses the queued job as-is and drops the incoming run.
            let current = state.jobs.get(active_id).map(record_run);
            if let Some(job) = state.jobs.get_mut(active_id) {
                if chainable {
                    if let (Some(current), Some(run)) = (current, incoming_run.take()) {
                        job.run = combine_runs(current, run);
                    }
                }
                if incoming_reason.priority() > job.reason.priority() {
                    job.reason = incoming_reason;
                }
            }
            Self::sort_queue(state, &self.shared);
            if let Some(job) = state.jobs.get(active_id) {
                job.publish();
            }
            return active_id.clone();
        }
        if chainable {
            if let Some(existing) = followup_id {
                let current = state.jobs.get(&existing).map(record_run);
                if let (Some(current), Some(job)) = (current, state.jobs.get_mut(&existing)) {
                    if let Some(run) = incoming_run.take() {
                        job.run = combine_runs(current, run);
                    }
                    if incoming_reason.priority() > job.reason.priority() {
                        job.reason = incoming_reason;
                    }
                    job.publish();
                }
                return existing;
            }
            if let Some(run) = incoming_run.take() {
                let id = create_job(
                    state,
                    SubmitIndexJob {
                        canonical_root: state
                            .jobs
                            .get(active_id)
                            .map(|job| job.canonical_root.clone())
                            .unwrap_or_default(),
                        reason: incoming_reason,
                        run,
                        followup_if_running,
                    },
                );
                if let Some(active) = state.jobs.get_mut(active_id) {
                    active.followup = Some(id.clone());
                }
                return id;
            }
        }
        active_id.clone()
    }

    fn enqueue(&self, state: &mut Inner, input: SubmitIndexJob) -> JobId {
        let id = create_job(state, input);
        activate(state, &id);
        id
    }

    fn pump(&self) {
        loop {
            let next = {
                let mut state = lock(&self.shared.state);
                if state.closed || state.running >= self.shared.concurrency {
                    return;
                }
                let position = state.queue.iter().position(|id| {
                    state
                        .jobs
                        .get(id)
                        .is_some_and(|job| job.state == IndexJobState::Queued && !job.retry_pending)
                });
                let Some(position) = position else { return };
                let id = state.queue.remove(position);
                let started = {
                    let Some(job) = state.jobs.get_mut(&id) else {
                        continue;
                    };
                    job.state = IndexJobState::Running;
                    job.attempt += 1;
                    job.error = None;
                    if job.started_at_ms.is_none() {
                        job.started_at_ms = Some(now_ms());
                    }
                    job.publish();
                    (job.canonical_root.clone(), job.attempt, job.id.clone())
                };
                state.running += 1;
                let (root, attempt, job_id) = started;
                self.log(
                    "job.started",
                    fields([
                        ("root_id", LogField::from(root_identity(&root))),
                        ("job_id", LogField::from(job_id.to_string())),
                        ("attempt", LogField::from(u64::from(attempt))),
                    ]),
                );
                id
            };
            let slf = self.clone();
            tokio::spawn(async move { slf.run_job(next).await });
        }
    }

    async fn run_job(&self, id: JobId) {
        let (run, reporter, cancel) = {
            let state = lock(&self.shared.state);
            let Some(job) = state.jobs.get(&id) else {
                return;
            };
            (
                job.run.clone(),
                progress_reporter(self, &id),
                job.cancel.clone(),
            )
        };
        let outcome = run(reporter, cancel.clone()).await;
        {
            let mut state = lock(&self.shared.state);
            let Some(job) = state.jobs.get_mut(&id) else {
                return;
            };
            if cancel.is_cancelled() {
                if job.error.is_none() {
                    // Mirrors TS `runJob`: an aborted run keeps the outcome's
                    // error when it has one, and finishes cancelled either way.
                    job.error = Some(match outcome {
                        Err(failure) => error_info(&failure),
                        Ok(()) => error_info(&JobFailure::from_cancelled()),
                    });
                }
                drop(state);
                self.finish(&id, IndexJobState::Cancelled);
            } else {
                match outcome {
                    Ok(()) => {
                        drop(state);
                        self.finish(&id, IndexJobState::Succeeded);
                    }
                    Err(failure) => {
                        let retryable = is_retryable(&failure);
                        let attempts_left = {
                            let job = state
                                .jobs
                                .get(&id)
                                .map(|job| job.attempt)
                                .unwrap_or(u32::MAX);
                            job < self.shared.max_attempts
                        };
                        if retryable && attempts_left && !state.closed {
                            let delay = self.shared.retry_base_delay_ms.saturating_mul(
                                1 << job_attempt(&state, &id).saturating_sub(1).min(10),
                            );
                            if let Some(job) = state.jobs.get_mut(&id) {
                                job.state = IndexJobState::Queued;
                                job.error = Some(error_info(&failure));
                                job.retry_pending = true;
                                job.publish();
                                let code = job
                                    .error
                                    .as_ref()
                                    .map(|error| error.code.clone())
                                    .unwrap_or_default();
                                drop(state);
                                self.log(
                                    "job.retry",
                                    fields([
                                        ("job_id", LogField::from(id.to_string())),
                                        ("error_code", LogField::from(code)),
                                        ("retry_after_ms", LogField::from(delay)),
                                    ]),
                                );
                                let slf = self.clone();
                                let retry_id = id.clone();
                                tokio::spawn(async move {
                                    tokio::time::sleep(std::time::Duration::from_millis(delay))
                                        .await;
                                    let requeue = {
                                        let mut state = lock(&slf.shared.state);
                                        if let Some(job) = state.jobs.get_mut(&retry_id) {
                                            if job.retry_pending
                                                && job.state == IndexJobState::Queued
                                            {
                                                job.retry_pending = false;
                                                job.publish();
                                                state.queue.push(retry_id.clone());
                                                Self::sort_queue(&mut state, &slf.shared);
                                                true
                                            } else {
                                                false
                                            }
                                        } else {
                                            false
                                        }
                                    };
                                    if requeue {
                                        slf.pump();
                                    }
                                });
                            }
                        } else {
                            let closed = state.closed;
                            if let Some(job) = state.jobs.get_mut(&id) {
                                job.error = Some(error_info(&failure));
                            }
                            drop(state);
                            self.finish(
                                &id,
                                if closed {
                                    IndexJobState::Cancelled
                                } else {
                                    IndexJobState::Failed
                                },
                            );
                        }
                    }
                }
            }
        }
        {
            let mut state = lock(&self.shared.state);
            state.running = state.running.saturating_sub(1);
        }
        self.pump();
    }

    fn finish(&self, id: &JobId, state: IndexJobState) {
        debug_assert!(matches!(
            state,
            IndexJobState::Succeeded | IndexJobState::Failed | IndexJobState::Cancelled
        ));
        let followup = {
            let mut guard = lock(&self.shared.state);
            let Some(job) = guard.jobs.get_mut(id) else {
                return;
            };
            job.state = state;
            job.finished_at_ms = Some(now_ms());
            job.retry_pending = false;
            job.publish();
            // Borrow ends here: sibling maps are touched with fresh lookups.
            let (canonical_root, attempt, code, duration, followup_candidate) = (
                job.canonical_root.clone(),
                job.attempt,
                job.error.as_ref().map(|error| error.code.clone()),
                job.finished_at_ms
                    .unwrap_or(0)
                    .saturating_sub(job.started_at_ms.unwrap_or(0)),
                job.followup.clone(),
            );
            if guard
                .active_by_root
                .get(&canonical_root)
                .is_some_and(|active| active == id)
            {
                guard.active_by_root.remove(&canonical_root);
            }
            let followup = if !guard.closed && state != IndexJobState::Cancelled {
                followup_candidate.filter(|followup| {
                    guard
                        .jobs
                        .get(followup)
                        .is_some_and(|job| job.state == IndexJobState::Queued)
                })
            } else {
                None
            };
            if let Some(job) = guard.jobs.get_mut(id) {
                job.followup = None;
            }
            let mut log_fields = fields([
                ("root_id", LogField::from(root_identity(&canonical_root))),
                ("job_id", LogField::from(id.to_string())),
                ("attempt", LogField::from(u64::from(attempt))),
                ("duration_ms", LogField::from(duration)),
            ]);
            if let Some(code) = code {
                log_fields.insert("error_code".to_owned(), LogField::from(code));
            }
            drop(guard);
            self.log("job.finished", log_fields);
            followup
        };
        if let Some(next) = followup {
            {
                let mut guard = lock(&self.shared.state);
                activate(&mut guard, &next);
            }
            self.pump();
        }
    }

    fn cancel_job(&self, id: &JobId, message: &str) {
        // Collect the followup chain first: recursion would re-lock the
        // state mutex on the same thread and deadlock.
        let mut chain = vec![id.clone()];
        {
            let state = lock(&self.shared.state);
            let mut current = id.clone();
            while let Some(next) = state
                .jobs
                .get(&current)
                .and_then(|job| job.followup.clone())
            {
                chain.push(next.clone());
                current = next;
            }
        }
        for member in &chain {
            let mut state = lock(&self.shared.state);
            let Some(job) = state.jobs.get_mut(member) else {
                continue;
            };
            if matches!(
                job.state,
                IndexJobState::Succeeded | IndexJobState::Failed | IndexJobState::Cancelled
            ) {
                continue;
            }
            job.cancel.cancel();
            job.retry_pending = false;
            job.followup = None;
        }
        // Queued members finish synchronously; running members observe
        // their token and finish from the run task.
        for member in &chain {
            let queued = lock(&self.shared.state)
                .jobs
                .get(member)
                .is_some_and(|job| job.state == IndexJobState::Queued);
            if queued {
                if let Some(job) = lock(&self.shared.state).jobs.get_mut(member) {
                    job.error = Some(IndexJobError {
                        code: DaemonError::IndexCancelled.code().to_owned(),
                        message: zg_core::error::redact_error_text(message, 512),
                        context: None,
                        cause: None,
                    });
                }
                self.finish(member, IndexJobState::Cancelled);
            }
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

    fn log(&self, name: &str, fields: BTreeMap<String, LogField>) {
        if let Some(logger) = &self.shared.logger {
            logger.event(name, fields);
        }
    }

    fn sort_queue(state: &mut Inner, #[allow(unused_variables)] shared: &Shared) {
        let jobs = &state.jobs;
        state.queue.sort_by(|left, right| {
            let left_job = jobs.get(left);
            let right_job = jobs.get(right);
            match (left_job, right_job) {
                (Some(left_job), Some(right_job)) => right_job
                    .reason
                    .priority()
                    .cmp(&left_job.reason.priority())
                    .then(left_job.created_at_ms.cmp(&right_job.created_at_ms)),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            }
        });
    }
}

impl JobFailure {
    /// Cancellation with the frozen `INDEX_CANCELLED` code.
    pub fn from_cancelled() -> Self {
        Self::Cancelled
    }
}

/// Bridges async cancellation into the sync index body (M6). Returns a
/// [`CancelFlag`] the blocking code polls; a background task trips it the
/// moment `token` is cancelled and exits once the flag trips or the token
/// fires, whichever comes first. This is the single documented adaptation
/// point between [`CancellationToken`] and [`CancelFlag`].
pub fn bridge_cancellation(token: &CancellationToken) -> CancelFlag {
    let flag = CancelFlag::new();
    if token.is_cancelled() {
        flag.cancel();
        return flag;
    }
    let flag_clone = flag.clone();
    let token_clone = token.clone();
    tokio::spawn(async move {
        token_clone.cancelled().await;
        flag_clone.cancel();
    });
    flag
}

fn lock(state: &Mutex<Inner>) -> std::sync::MutexGuard<'_, Inner> {
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

fn create_job(state: &mut Inner, input: SubmitIndexJob) -> JobId {
    let id = JobId(uuid::Uuid::new_v4().to_string());
    let (sender, _) = watch::channel(IndexJobSnapshot {
        id: id.clone(),
        canonical_root: input.canonical_root.clone(),
        reason: input.reason,
        state: IndexJobState::Queued,
        attempt: 0,
        created_at_ms: now_ms(),
        started_at_ms: None,
        finished_at_ms: None,
        progress: None,
        error: None,
    });
    state.jobs.insert(
        id.clone(),
        JobRecord {
            id: id.clone(),
            canonical_root: input.canonical_root,
            reason: input.reason,
            state: IndexJobState::Queued,
            attempt: 0,
            created_at_ms: now_ms(),
            started_at_ms: None,
            finished_at_ms: None,
            progress: None,
            error: None,
            run: input.run,
            cancel: CancellationToken::new(),
            completed: sender,
            listeners: Vec::new(),
            followup: None,
            retry_pending: false,
        },
    );
    id
}

fn activate(state: &mut Inner, id: &JobId) {
    if let Some(job) = state.jobs.get(id) {
        state
            .active_by_root
            .insert(job.canonical_root.clone(), id.clone());
        state
            .latest_by_root
            .insert(job.canonical_root.clone(), id.clone());
    }
    state.queue.push(id.clone());
}

fn record_run(job: &JobRecord) -> JobRun {
    job.run.clone()
}

fn combine_runs(first: JobRun, second: JobRun) -> JobRun {
    Arc::new(
        move |report: IndexProgressSink, cancel: CancellationToken| {
            let first = first.clone();
            let second = second.clone();
            Box::pin(async move {
                first(report.clone(), cancel.clone()).await?;
                second(report, cancel).await
            }) as BoxFuture<'static, JobOutcome>
        },
    )
}

fn snapshot_missing(id: &JobId) -> IndexJobSnapshot {
    IndexJobSnapshot {
        id: id.clone(),
        canonical_root: String::new(),
        reason: JobReason::Manual,
        state: IndexJobState::Failed,
        attempt: 0,
        created_at_ms: now_ms(),
        started_at_ms: None,
        finished_at_ms: Some(now_ms()),
        progress: None,
        error: Some(IndexJobError {
            code: DaemonError::IndexFailed {
                message: String::new(),
            }
            .code()
            .to_owned(),
            message: "job record vanished mid-submit".to_owned(),
            context: None,
            cause: None,
        }),
    }
}

fn job_attempt(state: &Inner, id: &JobId) -> u32 {
    state.jobs.get(id).map(|job| job.attempt).unwrap_or(1)
}

fn progress_reporter(scheduler: &JobScheduler, id: &JobId) -> IndexProgressSink {
    let slf = scheduler.clone();
    let job_id = id.clone();
    Arc::new(move |progress: IndexProgress| {
        let mut state = lock(&slf.shared.state);
        if let Some(job) = state.jobs.get_mut(&job_id) {
            job.progress = Some(progress.clone());
            job.publish();
            for listener in job.listeners.clone() {
                drop(state);
                safe_report(&Some(listener), &progress);
                state = lock(&slf.shared.state);
                if !state.jobs.contains_key(&job_id) {
                    return;
                }
            }
        }
    })
}

fn safe_report(sink: &Option<IndexProgressSink>, progress: &IndexProgress) {
    if let Some(sink) = sink {
        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| sink(progress.clone())));
        let _ = result;
    }
}

fn on_progress_clone(sink: &Option<IndexProgressSink>) -> IndexProgressSink {
    sink.clone().unwrap_or_else(|| Arc::new(|_| {}))
}

fn fields<const N: usize>(entries: [(&str, LogField); N]) -> BTreeMap<String, LogField> {
    entries
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value))
        .collect()
}

/// Retryable exactly like TS `isRetryable`: a retryable `DaemonError`, or
/// an engine error carrying `ZVEC_GREP.ENGINE.LOCK.BUSY`.
fn is_retryable(failure: &JobFailure) -> bool {
    match failure {
        JobFailure::Cancelled => false,
        JobFailure::Daemon(error) => error.retryable(),
        JobFailure::Engine(error) => error.code().to_string() == "ZVEC_GREP.ENGINE.LOCK.BUSY",
        JobFailure::Failed(_) => false,
    }
}

fn error_info(failure: &JobFailure) -> IndexJobError {
    match failure {
        JobFailure::Cancelled => IndexJobError {
            code: DaemonError::IndexCancelled.code().to_owned(),
            message: "indexing was cancelled".to_owned(),
            context: None,
            cause: None,
        },
        JobFailure::Daemon(error) => IndexJobError {
            code: safe_code(error.code()).unwrap_or("INDEX_FAILED").to_owned(),
            message: zg_core::error::redact_error_text(&error.to_string(), 512),
            context: None,
            cause: None,
        },
        JobFailure::Engine(error) => IndexJobError {
            code: safe_engine_code(&error.code().to_string()),
            message: zg_core::error::redact_error_text(error.message(), 512),
            context: error
                .context()
                .map(|context| zg_core::error::redact_error_text(context, 4096)),
            cause: None,
        },
        JobFailure::Failed(message) => IndexJobError {
            code: "INDEX_FAILED".to_owned(),
            message: zg_core::error::redact_error_text(message, 512),
            context: None,
            cause: None,
        },
    }
}

/// TS `safeErrorCode`: uppercase-led `A-Z0-9_.-`, at most 128 chars.
fn safe_code(code: &str) -> Option<&str> {
    let trimmed = code.trim();
    if trimmed.is_empty() || trimmed.len() > 128 {
        return None;
    }
    let mut chars = trimmed.bytes();
    if !chars.next().is_some_and(|byte| byte.is_ascii_uppercase()) {
        return None;
    }
    if chars.all(|byte| {
        byte.is_ascii_uppercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'.' | b'-')
    }) {
        Some(trimmed)
    } else {
        None
    }
}

fn safe_engine_code(code: &str) -> String {
    let redacted = zg_core::error::redact_error_text(code.trim(), 128);
    if redacted.chars().all(|char| {
        char.is_ascii_uppercase() || char.is_ascii_digit() || matches!(char, '_' | '.' | '-')
    }) && redacted.starts_with(|char: char| char.is_ascii_uppercase())
    {
        redacted
    } else {
        "INDEX_FAILED".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn successful_run() -> JobRun {
        Arc::new(|_, _| Box::pin(async { Ok(()) }) as BoxFuture<'static, JobOutcome>)
    }

    fn failing_run(message: &str) -> JobRun {
        let message = message.to_owned();
        Arc::new(move |_, _| {
            let message = message.clone();
            Box::pin(async { Err(JobFailure::Failed(message)) }) as BoxFuture<'static, JobOutcome>
        })
    }

    fn submit(
        scheduler: &JobScheduler,
        root: &str,
        reason: JobReason,
        run: JobRun,
    ) -> SubmitIndexJobResult {
        scheduler
            .submit(SubmitIndexJob {
                canonical_root: root.to_owned(),
                reason,
                run,
                followup_if_running: false,
            })
            .unwrap()
    }

    #[tokio::test]
    async fn success_round_trip() {
        let scheduler = JobScheduler::new(JobSchedulerOptions::default());
        let submitted = submit(&scheduler, "/repo", JobReason::Manual, successful_run());
        assert!(!submitted.reused);
        let snapshot = scheduler.wait(&submitted.job.id, None).await.unwrap();
        assert_eq!(snapshot.state, IndexJobState::Succeeded);
        assert_eq!(snapshot.attempt, 1);
    }

    #[tokio::test]
    async fn duplicate_submit_reuses_active_job() {
        let scheduler = JobScheduler::new(JobSchedulerOptions::default());
        let first = submit(&scheduler, "/repo", JobReason::Manual, successful_run());
        let second = submit(&scheduler, "/repo", JobReason::FreshQuery, successful_run());
        assert!(second.reused);
        assert_eq!(first.job.id, second.job.id);
        scheduler.wait(&first.job.id, None).await.unwrap();
    }

    #[tokio::test]
    async fn watch_chains_a_followup_while_running() {
        let scheduler = JobScheduler::new(JobSchedulerOptions::default());
        let gate = Arc::new(tokio::sync::Notify::new());
        let gate_clone = gate.clone();
        let blocking: JobRun = Arc::new(move |_, cancel| {
            let gate = gate_clone.clone();
            Box::pin(async move {
                tokio::select! {
                    () = gate.notified() => Ok(()),
                    () = cancel.cancelled() => Err(JobFailure::Cancelled),
                }
            }) as BoxFuture<'static, JobOutcome>
        });
        let first = scheduler
            .submit(SubmitIndexJob {
                canonical_root: "/repo".to_owned(),
                reason: JobReason::Manual,
                run: blocking,
                followup_if_running: false,
            })
            .unwrap();
        // Let the first job start.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let chained = scheduler
            .submit(SubmitIndexJob {
                canonical_root: "/repo".to_owned(),
                reason: JobReason::Watch,
                run: successful_run(),
                followup_if_running: false,
            })
            .unwrap();
        assert!(chained.reused);
        assert_ne!(first.job.id, chained.job.id);
        gate.notify_one();
        scheduler.wait_for_root_idle("/repo").await;
        let followup = scheduler.get(&chained.job.id).unwrap();
        assert_eq!(followup.state, IndexJobState::Succeeded);
    }

    #[tokio::test]
    async fn non_retryable_failure_fails_without_retry() {
        let scheduler = JobScheduler::new(JobSchedulerOptions {
            retry_base_delay_ms: Some(1),
            ..JobSchedulerOptions::default()
        });
        let submitted = submit(&scheduler, "/repo", JobReason::Manual, failing_run("boom"));
        let snapshot = scheduler.wait(&submitted.job.id, None).await.unwrap();
        assert_eq!(snapshot.state, IndexJobState::Failed);
        assert_eq!(snapshot.attempt, 1);
        assert_eq!(
            snapshot.error.as_ref().map(|error| error.code.as_str()),
            Some("INDEX_FAILED")
        );
    }

    #[tokio::test]
    async fn lock_busy_retries_until_attempts_run_out() {
        let scheduler = JobScheduler::new(JobSchedulerOptions {
            max_attempts: Some(3),
            retry_base_delay_ms: Some(1),
            ..JobSchedulerOptions::default()
        });
        let busy: JobRun = Arc::new(|_, _| {
            Box::pin(async {
                Err(JobFailure::Engine(EngineError::new(
                    zg_core::error::EngineErrorCode::from_static("LOCK.BUSY"),
                    "locked",
                )))
            }) as BoxFuture<'static, JobOutcome>
        });
        let submitted = submit(&scheduler, "/repo", JobReason::Manual, busy);
        let snapshot = scheduler.wait(&submitted.job.id, None).await.unwrap();
        assert_eq!(snapshot.state, IndexJobState::Failed);
        assert_eq!(snapshot.attempt, 3);
        assert_eq!(
            snapshot.error.as_ref().map(|error| error.code.as_str()),
            Some("ZVEC_GREP.ENGINE.LOCK.BUSY")
        );
    }

    #[tokio::test]
    async fn cancel_root_cancels_a_running_job() {
        let scheduler = JobScheduler::new(JobSchedulerOptions::default());
        let hanging: JobRun = Arc::new(|_, cancel| {
            Box::pin(async move {
                cancel.cancelled().await;
                Err(JobFailure::Cancelled)
            }) as BoxFuture<'static, JobOutcome>
        });
        let submitted = submit(&scheduler, "/repo", JobReason::Manual, hanging);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(scheduler.cancel_root("/repo"));
        let snapshot = scheduler.wait(&submitted.job.id, None).await.unwrap();
        assert_eq!(snapshot.state, IndexJobState::Cancelled);
        assert!(!scheduler.cancel_root("/repo"));
    }

    #[tokio::test]
    async fn close_cancels_and_awaits_in_flight_work() {
        let scheduler = JobScheduler::new(JobSchedulerOptions::default());
        let entered = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let finished = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let entered_clone = entered.clone();
        let finished_clone = finished.clone();
        let blocking: JobRun = Arc::new(move |_, cancel| {
            let entered = entered_clone.clone();
            let finished = finished_clone.clone();
            Box::pin(async move {
                entered.store(true, std::sync::atomic::Ordering::SeqCst);
                // Blocking-style body: observes cancel, then cleans up.
                cancel.cancelled().await;
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                finished.store(true, std::sync::atomic::Ordering::SeqCst);
                Err(JobFailure::Cancelled)
            }) as BoxFuture<'static, JobOutcome>
        });
        let submitted = submit(&scheduler, "/repo", JobReason::Manual, blocking);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(entered.load(std::sync::atomic::Ordering::SeqCst));
        scheduler.close().await;
        // Close awaited the in-flight body instead of orphaning it.
        assert!(finished.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(scheduler.load().running, 0);
        let snapshot = scheduler.get(&submitted.job.id).unwrap();
        assert_eq!(snapshot.state, IndexJobState::Cancelled);
        // Submitting after close is a typed shutdown error.
        assert!(matches!(
            scheduler.submit(SubmitIndexJob {
                canonical_root: "/repo".to_owned(),
                reason: JobReason::Manual,
                run: successful_run(),
                followup_if_running: false,
            }),
            Err(DaemonError::ShuttingDown)
        ));
    }
}
