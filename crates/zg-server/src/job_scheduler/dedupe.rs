//! Dedupe and queueing: absorb-or-chain, enqueue, run combination, ordering.
//!
//! Mirrors the TS `submit` absorption rules. Each branch below owns the
//! incoming [`JobRun`](super::failure::JobRun) at most once and always
//! returns, so the run is never dropped silently nor executed twice.

use futures::future::BoxFuture;

use super::failure::{JobOutcome, JobRun};
use super::id::JobId;
use super::reason::JobReason;
use super::scheduler::JobScheduler;
use super::snapshot::SubmitIndexJob;
use super::state::{Inner, JobRecord, activate, create_job};

impl JobScheduler {
    // `canonical_root: _` below is intentional, not `..`: a new
    // `SubmitIndexJob` field must fail compilation here (the hardening plan
    // keeps this as the named-ignore template). Allowed against
    // `unneeded_field_pattern`.
    #[allow(clippy::unneeded_field_pattern)]
    pub(crate) fn absorb_or_chain(
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
                    job.state == zg_core::index_status::IndexJobState::Queued && !job.retry_pending,
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
                if chainable && let (Some(current), Some(run)) = (current, incoming_run.take()) {
                    job.run = combine_runs(current, run);
                }
                if incoming_reason.priority() > job.reason.priority() {
                    job.reason = incoming_reason;
                }
            }
            Self::sort_queue(state);
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

    pub(crate) fn enqueue(&self, state: &mut Inner, input: SubmitIndexJob) -> JobId {
        let id = create_job(state, input);
        activate(state, &id);
        id
    }

    pub(crate) fn sort_queue(state: &mut Inner) {
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

fn record_run(job: &JobRecord) -> JobRun {
    job.run.clone()
}

fn combine_runs(first: JobRun, second: JobRun) -> JobRun {
    std::sync::Arc::new(
        move |report: zg_core::pipeline::indexing::IndexProgressSink,
              cancel: tokio_util::sync::CancellationToken| {
            let first = first.clone();
            let second = second.clone();
            Box::pin(async move {
                first(report.clone(), cancel.clone()).await?;
                second(report, cancel).await
            }) as BoxFuture<'static, JobOutcome>
        },
    )
}
