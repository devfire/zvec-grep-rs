//! Async-boundary adapters: progress reporting, cancellation bridging, log
//! fields.
//!
//! These are the only places the scheduler touches caller-supplied
//! callbacks and the sync/async cancellation seam, so containment rules
//! (panic isolation, flag bridging) are reviewed here, not scattered
//! through the execution layer.

use std::collections::BTreeMap;
use std::sync::Arc;

use tokio_util::sync::CancellationToken;
use zg_core::pipeline::indexing::IndexProgressSink;
use zg_core::pipeline::indexing::scanner::CancelFlag;
use zg_core::types::IndexProgress;

use crate::logger::LogField;

use super::id::JobId;
use super::scheduler::JobScheduler;
use crate::sync::MutexExt;

/// Bridges async cancellation into the sync index body. Returns a
/// [`CancelFlag`] the blocking code polls; a background task trips it the
/// moment `token` is cancelled and exits once the flag trips or the token
/// fires, whichever comes first. This is the single documented adaptation
/// point between [`CancellationToken`] and [`CancelFlag`].
#[must_use]
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
