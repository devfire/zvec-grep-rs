//! Interior scheduler state: records, maps, queue, and creation helpers.
//!
//! The state mutex is never held across an await (concurrency rule): every
//! method drops the guard before spawning or awaiting. A poisoned mutex is
//! recovered via `into_inner` — the panicking holder already records its
//! failure through its own job outcome, so wedging the whole scheduler
//! would be strictly worse.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};

use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use zg_core::index_status::IndexJobState;
use zg_core::pipeline::indexing::IndexProgressSink;
use zg_core::types::{IndexProgress, UnixMillis};

use crate::errors::DaemonError;
use crate::logger::DaemonLogger;

use super::failure::JobRun;
use super::id::JobId;
use super::reason::JobReason;
use super::snapshot::{IndexJobError, IndexJobSnapshot, SubmitIndexJob};

pub(crate) struct JobRecord {
    pub(crate) id: JobId,
    pub(crate) canonical_root: String,
    pub(crate) reason: JobReason,
    pub(crate) state: IndexJobState,
    pub(crate) attempt: u32,
    pub(crate) created_at_ms: u64,
    pub(crate) started_at_ms: Option<u64>,
    pub(crate) finished_at_ms: Option<u64>,
    pub(crate) progress: Option<IndexProgress>,
    pub(crate) error: Option<IndexJobError>,
    pub(crate) run: JobRun,
    pub(crate) cancel: CancellationToken,
    pub(crate) completed: watch::Sender<IndexJobSnapshot>,
    pub(crate) listeners: Vec<IndexProgressSink>,
    pub(crate) followup: Option<JobId>,
    pub(crate) retry_pending: bool,
}

impl JobRecord {
    pub(crate) fn snapshot(&self) -> IndexJobSnapshot {
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

    pub(crate) fn publish(&self) {
        // `send_replace`, not `send`: since tokio 1.53 `send` drops the
        // value when no receiver exists, which would leave late waiters
        // staring at a stale slot forever. The slot must always hold the
        // latest snapshot.
        self.completed.send_replace(self.snapshot());
    }
}

#[derive(Default)]
pub(crate) struct Inner {
    pub(crate) jobs: HashMap<JobId, JobRecord>,
    pub(crate) active_by_root: HashMap<String, JobId>,
    pub(crate) latest_by_root: HashMap<String, JobId>,
    pub(crate) queue: Vec<JobId>,
    pub(crate) running: usize,
    pub(crate) closed: bool,
}

pub(crate) struct Shared {
    pub(crate) state: Mutex<Inner>,
    pub(crate) concurrency: usize,
    pub(crate) max_attempts: u32,
    pub(crate) retry_base_delay_ms: u64,
    pub(crate) logger: Option<DaemonLogger>,
}

pub(crate) fn lock(state: &Mutex<Inner>) -> MutexGuard<'_, Inner> {
    state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub(crate) fn create_job(state: &mut Inner, input: SubmitIndexJob) -> JobId {
    let id = JobId::new(uuid::Uuid::new_v4().to_string());
    let (sender, _) = watch::channel(IndexJobSnapshot {
        id: id.clone(),
        canonical_root: input.canonical_root.clone(),
        reason: input.reason,
        state: IndexJobState::Queued,
        attempt: 0,
        // Clock-unavailable direction: metadata plus dedupe tiebreak /
        // duration display; ties keep stable sort order (fail-safe).
        created_at_ms: UnixMillis::now_ms_or(0),
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
            // Clock-unavailable direction: metadata plus dedupe tiebreak /
            // duration display; ties keep stable sort order (fail-safe).
            created_at_ms: UnixMillis::now_ms_or(0),
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

pub(crate) fn activate(state: &mut Inner, id: &JobId) {
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

pub(crate) fn snapshot_missing(id: &JobId) -> IndexJobSnapshot {
    IndexJobSnapshot {
        id: id.clone(),
        canonical_root: String::new(),
        reason: JobReason::Manual,
        state: IndexJobState::Failed,
        attempt: 0,
        // Clock-unavailable direction: metadata plus dedupe tiebreak /
        // duration display; ties keep stable sort order (fail-safe).
        created_at_ms: UnixMillis::now_ms_or(0),
        started_at_ms: None,
        // Same direction: terminal metadata, display only.
        finished_at_ms: Some(UnixMillis::now_ms_or(0)),
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

pub(crate) fn job_attempt(state: &Inner, id: &JobId) -> u32 {
    state.jobs.get(id).map(|job| job.attempt).unwrap_or(1)
}
