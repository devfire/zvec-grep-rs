//! Per-root index job scheduling: dedupe, priority, retry, cooperative cancel.
//!
//! Mirrors `../zvec-grep/src/daemon/job-scheduler.ts` (`JobScheduler`:
//! per-root active/latest maps, priority queue, `maxAttempts` retry with
//! exponential backoff, watch-reason followups, progress listeners).
//! Two deliberate divergences (see `docs/ts-divergence.md`):
//!
//! - `AbortController`/`AbortSignal` become
//!   [`CancellationToken`](tokio_util::sync::CancellationToken) at the
//!   async edge; the sync index body only ever sees a
//!   [`CancelFlag`](zg_core::pipeline::indexing::scanner::CancelFlag)
//!   via [`bridge_cancellation`] (M6). Dropping a `spawn_blocking` handle
//!   cannot abort it, so shutdown awaits in-flight work instead of
//!   orphaning it.
//! - The TS promise-chain generation idiom is sequential task execution:
//!   one task per job, one followup slot per root.
//!
//! Layout: [`JobScheduler`] and the public wire model live in sibling
//! modules (`reason`, `id`, `failure`, `snapshot`); interior state in
//! `state`; submit absorption in `dedupe`; dispatch/retry in `run`;
//! failure mapping in `error_info`; async-boundary adapters in `edge`.
//! The facade re-exports every name used
//! outside this module, so existing `crate::job_scheduler::X` paths work.

mod dedupe;
mod edge;
mod error_info;
mod failure;
mod id;
mod reason;
mod run;
mod scheduler;
mod snapshot;
mod state;
#[cfg(test)]
mod tests;

pub use edge::bridge_cancellation;
pub use failure::{JobFailure, JobOutcome, JobRun};
pub use id::JobId;
pub use reason::JobReason;
pub use scheduler::JobScheduler;
pub use snapshot::{
    DEFAULT_MAX_ATTEMPTS, DEFAULT_RETRY_BASE_DELAY_MS, DEFAULT_SCHEDULER_CONCURRENCY,
    IndexJobError, IndexJobSnapshot, JobSchedulerOptions, SchedulerLoad, SubmitIndexJob,
    SubmitIndexJobResult,
};
