//! Async-boundary adapters: progress reporting, cancellation bridging, log
//! fields.
//!
//! These are the only places the scheduler touches caller-supplied
//! callbacks and the sync/async cancellation seam, so containment rules
//! (panic isolation, flag bridging) are reviewed here, not scattered
//! through the execution layer.

use std::collections::BTreeMap;
use std::ops::Deref;
use std::sync::Arc;

use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use zg_core::pipeline::indexing::IndexProgressSink;
use zg_core::pipeline::indexing::scanner::CancelFlag;
use zg_core::types::IndexProgress;

use crate::logger::LogField;

use super::id::JobId;
use super::scheduler::JobScheduler;
use crate::sync::MutexExt;

/// Lifetime-bound bridge between async cancellation and the sync index
/// body: holds the [`CancelFlag`] the blocking code polls plus the
/// background waiter that trips it the moment `token` is cancelled.
///
/// Dropping the bridge aborts the waiter, so hold it for the whole run and
/// drop it once the job reaches terminal. A detached waiter would pend on
/// `token` forever whenever the job completes without cancellation — one
/// leaked task per job. This is the single documented adaptation point
/// between [`CancellationToken`] and [`CancelFlag`].
pub struct CancellationBridge {
    flag: CancelFlag,
    waiter: Option<JoinHandle<()>>,
}

impl CancellationBridge {
    /// The bridged flag the blocking code polls.
    #[must_use]
    pub fn flag(&self) -> &CancelFlag {
        &self.flag
    }

    /// True once the background waiter finished (pre-cancelled token, or
    /// the token fired and the flag was tripped). Lets tests prove no
    /// waiter is left pending behind a completed job.
    #[must_use]
    pub fn waiter_finished(&self) -> bool {
        self.waiter.as_ref().is_none_or(JoinHandle::is_finished)
    }
}

impl Deref for CancellationBridge {
    type Target = CancelFlag;

    fn deref(&self) -> &Self::Target {
        &self.flag
    }
}

impl Drop for CancellationBridge {
    fn drop(&mut self) {
        // The job reached terminal (or the run was abandoned): abort the
        // waiter so no per-job task outlives the job.
        if let Some(waiter) = self.waiter.take() {
            waiter.abort();
        }
    }
}

/// Bridges async cancellation into the sync index body. Returns a
/// `CancellationBridge` guarding the [`CancelFlag`] the blocking code
/// polls (derefs to it, so `cancel()` / `is_cancelled()` keep working);
/// hold the guard for the job's lifetime and drop it once the job reaches
/// terminal so the background waiter is aborted instead of leaked.
#[must_use]
pub fn bridge_cancellation(token: &CancellationToken) -> CancellationBridge {
    let flag = CancelFlag::new();
    if token.is_cancelled() {
        flag.cancel();
        return CancellationBridge { flag, waiter: None };
    }
    let flag_clone = flag.clone();
    let token_clone = token.clone();
    let waiter = tokio::spawn(async move {
        token_clone.cancelled().await;
        flag_clone.cancel();
    });
    CancellationBridge {
        flag,
        waiter: Some(waiter),
    }
}

pub(crate) fn progress_reporter(scheduler: &JobScheduler, id: &JobId) -> IndexProgressSink {
    let slf = scheduler.clone();
    let job_id = id.clone();
    Arc::new(move |progress: IndexProgress| {
        let mut state = slf.shared.state.lock_ignore_poison();
        if let Some(job) = state.jobs.get_mut(&job_id) {
            job.progress = Some(progress.clone());
            job.publish();
            for listener in job.listeners.clone() {
                drop(state);
                safe_report(&Some(listener), &progress);
                state = slf.shared.state.lock_ignore_poison();
                if !state.jobs.contains_key(&job_id) {
                    return;
                }
            }
        }
    })
}

/// Progress observers never affect the outcome: panics inside `on_progress`
/// are contained per call.
pub(crate) fn safe_report(sink: &Option<IndexProgressSink>, progress: &IndexProgress) {
    if let Some(sink) = sink {
        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| sink(progress.clone())));
        let _ = result;
    }
}

pub(crate) fn on_progress_clone(sink: &Option<IndexProgressSink>) -> IndexProgressSink {
    sink.clone().unwrap_or_else(|| Arc::new(|_| {}))
}

pub(crate) fn fields<const N: usize>(entries: [(&str, LogField); N]) -> BTreeMap<String, LogField> {
    entries
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value))
        .collect()
}

#[cfg(test)]
mod tests {
    use tokio_util::sync::CancellationToken;

    use super::*;

    #[tokio::test]
    async fn bridge_trips_flag_on_cancel_and_spawns_no_waiter_when_precancelled() {
        let token = CancellationToken::new();
        let bridge = bridge_cancellation(&token);
        assert!(!bridge.is_cancelled());
        assert!(!bridge.waiter_finished());
        token.cancel();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        while !bridge.is_cancelled() && tokio::time::Instant::now() < deadline {
            tokio::task::yield_now().await;
        }
        assert!(bridge.is_cancelled());
        assert!(bridge.waiter_finished());
        drop(bridge);
        let precancelled = bridge_cancellation(&token);
        assert!(precancelled.is_cancelled());
        assert!(precancelled.waiter_finished());
    }
}
