//! Scheduler behavior tests: round trip, dedupe, followup chaining, retry,
//! cancel, and shutdown. Moved verbatim from `job_scheduler.rs`; imports
//! are explicit (the old `use super::*` relied on the single-file
//! parent's `use` bindings, which the facade no longer re-exports).

use std::sync::Arc;

use futures::future::BoxFuture;
use zg_core::error::EngineError;
use zg_core::index_status::IndexJobState;

use super::failure::{JobFailure, JobOutcome, JobRun};
use super::reason::JobReason;
use super::scheduler::JobScheduler;
use super::snapshot::{JobSchedulerOptions, SubmitIndexJob, SubmitIndexJobResult};
use crate::errors::DaemonError;

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
    // Wait until the body is actually running. A fixed sleep flakes on
    // loaded runners, where milliseconds of wall-clock buy little CPU
    // time for the spawned job to be polled.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(15);
    while !entered.load(std::sync::atomic::Ordering::SeqCst)
        && tokio::time::Instant::now() < deadline
    {
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
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
