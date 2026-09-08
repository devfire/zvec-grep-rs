//! Index coordination: pending-change batching with target-revision tracking.
//!
//! Mirrors `../zvec-grep/src/daemon/index-coordinator.ts`. The coordinator
//! owns the pending [`ChangeSet`](crate::change_set::ChangeSet) and the
//! target revision; `RootRuntime` owns the indexed/reconciled revisions.
//! Each `enqueue` merges changes, bumps the dirty revision via
//! [`RootRuntime::mark_dirty`](crate::root_runtime::RootRuntime::mark_dirty),
//! and submits one scheduler job whose run takes the pending snapshot
//! lazily on first execution — so a chained followup run reuses the same
//! snapshot instead of stealing the next batch (mirrors the TS
//! `jobChanges` closure exactly).

use std::sync::{Arc, Mutex};

use crate::change_set::{ChangeSet, ChangeSetOptions, ChangeSetSnapshot, MaxChangedPaths};
use crate::errors::DaemonError;
use crate::job_scheduler::{IndexJobSnapshot, JobReason, JobRun, JobScheduler, SubmitIndexJob};
use crate::root_runtime::{Generation, RootRuntime};

/// Proof returned by an index run: whether a full reconciliation happened
/// and at which epoch. The actor applies it via
/// [`RootRuntime::mark_reconciled`](crate::root_runtime::RootRuntime::mark_reconciled)
/// (full) or `mark_indexed` (incremental).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexReconciliationProof {
    /// True when the run reconciled the full index at `epoch`.
    pub reconciled: bool,
    /// Full-reconciliation epoch the proof applies to.
    pub epoch: Generation,
}

/// Why the coordinator enqueued work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoordinatorReason {
    /// Filesystem watcher event.
    Watch,
    /// Periodic, resume, or manual reconciliation.
    Reconcile,
}

impl CoordinatorReason {
    const fn scheduler_reason(self) -> JobReason {
        match self {
            Self::Watch => JobReason::Watch,
            Self::Reconcile => JobReason::Reconcile,
        }
    }
}

/// Lazily takes the pending snapshot once per submitted job: the first
/// call snapshots (and resets) the pending set and stamps the target
/// revision; later calls replay the same pair.
pub type TakePending = Arc<dyn Fn() -> (ChangeSetSnapshot, Generation) + Send + Sync>;

#[derive(Debug)]
struct PendingState {
    set: ChangeSet,
    target: Generation,
}

/// Batches watcher/reconcile changes into scheduler jobs with revision
/// tracking. Owned by one root actor; the pending set is shared with
/// in-flight runs through [`TakePending`] handles.
#[derive(Debug, Clone)]
pub struct IndexCoordinator {
    root: String,
    budget: Option<MaxChangedPaths>,
    pending: Arc<Mutex<PendingState>>,
}

impl IndexCoordinator {
    /// Empty coordinator for `root`.
    pub fn new(root: &str, budget: Option<MaxChangedPaths>) -> Self {
        Self {
            root: root.to_owned(),
            budget,
            pending: Arc::new(Mutex::new(PendingState {
                set: ChangeSet::new(ChangeSetOptions {
                    root: Some(root.to_owned()),
                    max_changed_paths: budget,
                }),
                target: Generation::ZERO,
            })),
        }
    }

    /// Merges `changes`, bumps the runtime dirty revision, and submits
    /// one job built by `build_run` (which receives the take handle).
    /// Followup chaining is always on, mirroring TS
    /// `followupIfRunning: true`.
    pub fn enqueue(
        &self,
        changes: &ChangeSetSnapshot,
        reason: CoordinatorReason,
        runtime: &mut RootRuntime,
        scheduler: &JobScheduler,
        build_run: impl FnOnce(TakePending) -> JobRun,
    ) -> Result<IndexJobSnapshot, DaemonError> {
        if changes.force_full_reconcile {
            runtime.require_full_reconciliation(false);
        }
        let handoff: Arc<Mutex<Option<(ChangeSetSnapshot, Generation)>>> =
            Arc::new(Mutex::new(None));
        {
            let mut pending = lock(&self.pending);
            pending.set.merge(changes);
            pending.target = runtime.mark_dirty();
        }
        let pending = Arc::clone(&self.pending);
        let root = self.root.clone();
        let budget = self.budget;
        let take: TakePending = Arc::new(move || {
            let mut handoff = lock(&handoff);
            if let Some(pair) = handoff.clone() {
                return pair;
            }
            let mut pending = lock(&pending);
            let snapshot = pending.set.snapshot();
            let pair = (snapshot, pending.target);
            pending.set = ChangeSet::new(ChangeSetOptions {
                root: Some(root.clone()),
                max_changed_paths: budget,
            });
            *handoff = Some(pair.clone());
            pair
        });
        let run = build_run(take);
        Ok(scheduler
            .submit(SubmitIndexJob {
                canonical_root: self.root.clone(),
                reason: reason.scheduler_reason(),
                run,
                followup_if_running: true,
            })?
            .job)
    }
}

fn lock<T>(state: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::root_runtime::RootKey;
    use futures::future::BoxFuture;
    use tokio_util::sync::CancellationToken;
    use zg_core::index_status::IndexJobState;
    use zg_core::pipeline::indexing::IndexProgressSink;

    fn changes(files: &[&str]) -> ChangeSetSnapshot {
        ChangeSetSnapshot {
            touched_files: files.iter().map(|file| (*file).to_owned()).collect(),
            rescan_directories: Vec::new(),
            deleted_prefixes: Vec::new(),
            force_full_reconcile: false,
        }
    }

    fn runtime() -> RootRuntime {
        RootRuntime::new(RootKey::parse("/repo").unwrap())
    }

    #[tokio::test]
    async fn take_returns_one_snapshot_per_enqueue() {
        let scheduler = JobScheduler::new(crate::job_scheduler::JobSchedulerOptions::default());
        let coordinator = IndexCoordinator::new("/repo", None);
        let mut runtime = runtime();
        let taken: Arc<Mutex<Vec<(ChangeSetSnapshot, Generation)>>> =
            Arc::new(Mutex::new(Vec::new()));
        let taken_clone = taken.clone();
        let ok: JobRun = Arc::new(|_, _| {
            Box::pin(async { Ok(()) }) as BoxFuture<'static, crate::job_scheduler::JobOutcome>
        });
        let submitted = coordinator
            .enqueue(
                &changes(&["/repo/a.rs"]),
                CoordinatorReason::Watch,
                &mut runtime,
                &scheduler,
                |take| {
                    let taken = taken_clone.clone();
                    Arc::new(move |_: IndexProgressSink, _: CancellationToken| {
                        let take = take.clone();
                        let taken = taken.clone();
                        Box::pin(async move {
                            // Twice: the second call must replay the first.
                            let first = take();
                            let second = take();
                            assert_eq!(first, second);
                            taken.lock().unwrap().push(first);
                            Ok(())
                        })
                            as BoxFuture<'static, crate::job_scheduler::JobOutcome>
                    })
                },
            )
            .unwrap();
        assert!(!submitted.is_terminal());
        scheduler.wait(&submitted.id, None).await.unwrap();
        let taken = taken.lock().unwrap();
        assert_eq!(taken.len(), 1);
        assert_eq!(taken[0].0.touched_files, vec!["/repo/a.rs"]);
        // The stamped revision is the runtime's dirty revision: applying it
        // as indexed proves the target-revision handshake.
        runtime.mark_indexed(taken[0].1);
        assert!(!runtime.needs_reconciliation());
        drop(ok);
    }

    #[tokio::test]
    async fn full_reconcile_proof_marks_epoch() {
        let scheduler = JobScheduler::new(crate::job_scheduler::JobSchedulerOptions::default());
        let coordinator = IndexCoordinator::new("/repo", None);
        let mut runtime = runtime();
        let full = ChangeSetSnapshot {
            force_full_reconcile: true,
            ..changes(&[])
        };
        let submitted = coordinator
            .enqueue(
                &full,
                CoordinatorReason::Reconcile,
                &mut runtime,
                &scheduler,
                |take| {
                    Arc::new(move |_: IndexProgressSink, _: CancellationToken| {
                        let take = take.clone();
                        Box::pin(async move {
                            let (snapshot, revision) = take();
                            assert!(snapshot.force_full_reconcile);
                            let _ = revision;
                            Ok(())
                        })
                            as BoxFuture<'static, crate::job_scheduler::JobOutcome>
                    })
                },
            )
            .unwrap();
        // `enqueue` demanded the full reconciliation itself; the proof
        // below applies that same epoch.
        let epoch = runtime.reconciliation_epoch();
        let snapshot = scheduler.wait(&submitted.id, None).await.unwrap();
        assert_eq!(snapshot.state, IndexJobState::Succeeded);
        assert!(runtime.requires_full_reconciliation());
        let revision = runtime.mark_dirty();
        runtime.mark_reconciled(revision, epoch);
        assert!(!runtime.requires_full_reconciliation());
    }
}
