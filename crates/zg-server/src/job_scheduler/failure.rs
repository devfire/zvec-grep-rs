//! Terminal failure of one job run, plus the [`JobRun`] work type.
//!
//! `JobFailure` is a closed enum (defensive: every variant spelled out at
//! each `match`, no wildcard arm, so a new variant forces all handlers to
//! decide). `JobRun` keeps `Arc<dyn ...>` dynamic dispatch deliberately
//! (zero-cost guidance): the set of caller-supplied closures is open and
//! heterogeneous, so static dispatch would not apply.

use futures::future::BoxFuture;
use tokio_util::sync::CancellationToken;
use zg_core::error::EngineError;
use zg_core::pipeline::indexing::IndexProgressSink;

use crate::errors::DaemonError;

/// Terminal failure of one job run. The scheduler maps this to an
/// [`IndexJobError`](crate::job_scheduler::IndexJobError) snapshot and decides
/// retryability from it.
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

impl JobFailure {
    /// Cancellation with the frozen `INDEX_CANCELLED` code.
    pub fn from_cancelled() -> Self {
        Self::Cancelled
    }
}

/// Outcome of one [`JobRun`]: success or a [`JobFailure`].
pub type JobOutcome = Result<(), JobFailure>;

/// One unit of index work. Receives an owned progress sink and a
/// cancellation token; returns success or a [`JobFailure`]. Must be
/// cooperative: check the token (or the bridged
/// [`CancelFlag`](zg_core::pipeline::indexing::scanner::CancelFlag)) and
/// return [`JobFailure::Cancelled`] promptly.
pub type JobRun = std::sync::Arc<
    dyn Fn(IndexProgressSink, CancellationToken) -> BoxFuture<'static, JobOutcome> + Send + Sync,
>;
