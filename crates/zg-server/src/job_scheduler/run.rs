//! Execution: dispatch loop, job runner with retry, finish, cancellation.
//!
//! One task per job, one followup slot per root (the sequential-task
//! analogue of the TS promise-chain idiom). Shutdown awaits in-flight work
//! instead of orphaning it: dropping a `spawn_blocking` handle cannot abort
//! it, so [`JobScheduler::close`](super::scheduler::JobScheduler::close)
//! cancels tokens and polls `running` to zero.

use zg_core::index_status::IndexJobState;
use zg_core::types::UnixMillis;

use crate::errors::DaemonError;
use crate::logger::{LogField, root_identity};
use crate::sync::MutexExt;

use super::edge::{fields, progress_reporter};
use super::error_info::{error_info, is_retryable};
use super::failure::{JobFailure, JobOutcome};
use super::id::JobId;
use super::scheduler::JobScheduler;
use super::snapshot::IndexJobError;
use super::state::{activate, job_attempt};

impl JobScheduler {
    pub(crate) fn pump(&self) {
        loop {
            let next = {
                let mut state = self.shared.state.lock_ignore_poison();
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
                        // Clock-unavailable direction: metadata only (fail-safe).
                        job.started_at_ms = Some(UnixMillis::now_ms_or(0));
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
            let state = self.shared.state.lock_ignore_poison();
            let Some(job) = state.jobs.get(&id) else {
                return;
            };
            (
                job.run.clone(),
                progress_reporter(self, &id),
                job.cancel.clone(),
            )
        };
        let outcome: JobOutcome = run(reporter, cancel.clone()).await;
        {
            let mut state = self.shared.state.lock_ignore_poison();
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
                                        let mut state = slf.shared.state.lock_ignore_poison();
                                        if let Some(job) = state.jobs.get_mut(&retry_id) {
                                            if job.retry_pending
                                                && job.state == IndexJobState::Queued
                                            {
                                                job.retry_pending = false;
                                                job.publish();
                                                state.queue.push(retry_id.clone());
                                                Self::sort_queue(&mut state);
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
            let mut state = self.shared.state.lock_ignore_poison();
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
            let mut guard = self.shared.state.lock_ignore_poison();
            let Some(job) = guard.jobs.get_mut(id) else {
                return;
            };
            job.state = state;
            // Clock-unavailable direction: duration display only (fail-safe).
            job.finished_at_ms = Some(UnixMillis::now_ms_or(0));
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
                let mut guard = self.shared.state.lock_ignore_poison();
                activate(&mut guard, &next);
            }
            self.pump();
        }
    }

    pub(crate) fn cancel_job(&self, id: &JobId, message: &str) {
        // Collect the followup chain first: recursion would re-lock the
        // state mutex on the same thread and deadlock.
        let mut chain = vec![id.clone()];
        {
            let state = self.shared.state.lock_ignore_poison();
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
            let mut state = self.shared.state.lock_ignore_poison();
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
            let queued = self
                .shared
                .state
                .lock_ignore_poison()
                .jobs
                .get(member)
                .is_some_and(|job| job.state == IndexJobState::Queued);
            if queued {
                if let Some(job) = self.shared.state.lock_ignore_poison().jobs.get_mut(member) {
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
}
