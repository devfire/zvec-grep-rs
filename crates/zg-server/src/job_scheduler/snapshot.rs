//! Observable wire model: snapshots, submit types, options, load.
//!
//! Pure data, no logic (except [`IndexJobSnapshot::is_terminal`]): the
//! scheduler state and execution layers consume these but they depend only
//! on the leaf `id` / `reason` / `failure` modules, keeping the layer edge
//! acyclic.

use zg_core::index_status::IndexJobState;
use zg_core::types::IndexProgress;

use crate::logger::DaemonLogger;

use super::failure::JobRun;
use super::id::JobId;
use super::reason::JobReason;

/// How many index jobs run concurrently by default.
pub const DEFAULT_SCHEDULER_CONCURRENCY: usize = 1;

/// Default retry budget per job.
pub const DEFAULT_MAX_ATTEMPTS: u32 = 3;

/// Base delay for exponential retry backoff.
pub const DEFAULT_RETRY_BASE_DELAY_MS: u64 = 250;

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
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(
            self.state,
            IndexJobState::Succeeded | IndexJobState::Failed | IndexJobState::Cancelled
        )
    }
}

/// Input to [`JobScheduler::submit`](crate::job_scheduler::JobScheduler::submit).
///
/// `followup_if_running` stays a plain bool to preserve the exact TS
/// `submit` call shape: a two-state flag with a documented meaning.
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

/// Result of [`JobScheduler::submit`](crate::job_scheduler::JobScheduler::submit).
#[derive(Debug)]
pub struct SubmitIndexJobResult {
    /// Current snapshot (fresh or reused).
    pub job: IndexJobSnapshot,
    /// True when an existing job was reused or chained.
    pub reused: bool,
}

/// Options for [`JobScheduler`](super::scheduler::JobScheduler).
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
