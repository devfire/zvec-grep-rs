//! Backend round-trip tests: index → status → search over a fixture root.

use std::sync::Arc;
use std::time::Duration;

use super::*;
use crate::job_scheduler::IndexJobSnapshot;
use zg_core::index_status::IndexJobState;

fn stub_service_config(dimension: usize) -> ServiceConfig {
    ServiceConfig {
        model_override: Some(Arc::new(zg_core::models::stub::StubEmbeddingModel::new(
            dimension,
        ))),
        ..ServiceConfig::default()
    }
}

/// First-embed gate: blocks the (blocking) embedding call until the
/// test releases it, so an index job is provably running — with its
/// storage write lock held — when a drop is requested (#72).
struct GatedStubModel {
    inner: zg_core::models::stub::StubEmbeddingModel,
    gate: Arc<(std::sync::Mutex<GateState>, std::sync::Condvar)>,
}

#[derive(Default)]
struct GateState {
    entered: bool,
    released: bool,
}

impl zg_core::models::EmbeddingModel for GatedStubModel {
    fn info(&self) -> &zg_core::models::EmbeddingModelInfo {
        self.inner.info()
    }

    fn max_batch_size(&self) -> usize {
        self.inner.max_batch_size()
    }

    fn embed(
        &self,
        purpose: zg_core::models::EmbeddingPurpose,
        inputs: &[zg_core::models::EmbeddingInput<'_>],
    ) -> zg_core::error::EngineResult<zg_core::models::embeddings::EmbeddingResult> {
        {
            let (lock, cvar) = &*self.gate;
            let mut state = lock.lock().unwrap();
            state.entered = true;
            cvar.notify_all();
            while !state.released {
                state = cvar.wait(state).unwrap();
            }
        }
        self.inner.embed(purpose, inputs)
    }
}

fn release_gate(gate: &Arc<(std::sync::Mutex<GateState>, std::sync::Condvar)>) {
    let (lock, cvar) = &**gate;
    lock.lock().unwrap().released = true;
    cvar.notify_all();
}

fn backend() -> DaemonBackend {
    DaemonBackend::new(DaemonBackendOptions {
        service: stub_service_config(16),
        ..DaemonBackendOptions::default()
    })
}

fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.rs"), "fn alpha() {}\n").unwrap();
    std::fs::write(dir.path().join("b.rs"), "fn beta() {}\n").unwrap();
    dir
}

fn root(dir: &tempfile::TempDir) -> String {
    // Canonicalize: TempDir paths may contain symlinks (`/var` ->
    // `/private/var` on macOS) while the backend normalizes roots via
    // `std::fs::canonicalize` (TS `realpath` parity). Comparing the raw
    // path would fail only on such platforms.
    dir.path()
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

#[tokio::test]
async fn index_status_search_round_trip() {
    let backend = backend();
    let dir = fixture();
    let root = root(&dir);
    let submitted = backend
        .index(
            &root,
            IndexInput {
                rebuild: true,
                ..IndexInput::default()
            },
        )
        .await
        .unwrap();
    let terminal = backend
        .scheduler()
        .wait(&submitted.job.id, None)
        .await
        .unwrap();
    assert_eq!(terminal.state, IndexJobState::Succeeded);
    let status = backend.index_status(&root).await.unwrap();
    assert!(status.status.files_scanned >= 2, "{status:?}");
    assert!(status.completion.is_some());
    // Lexical path is exact: the fixture provably contains this symbol.
    let rg = backend
        .rg_search(
            &root,
            RgQuery {
                patterns: vec!["fn alpha".to_owned()],
                ..RgQuery::default()
            },
        )
        .await
        .unwrap();
    assert!(!rg.items.is_empty());
    // Vector path proves the plumbing; ranking over stub hashes is
    // arbitrary, so only the contract is asserted.
    let searched = backend
        .search(
            &root,
            SearchQuery {
                query: Some("alpha function".to_owned()),
                limit: Some(5),
                ..SearchQuery::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(searched.result.root, root);
    assert_eq!(searched.freshness, ResultFreshness::Fresh);
    assert!(searched.indexing.is_none());
    backend.close().await;
}

#[tokio::test]
async fn watcher_reports_active_and_pending_search_is_stale() {
    // Regression (#70): the actor never wired the watcher's pending
    // callback and never marked the watcher active, so status always
    // read `watcher_active: false` and a search inside the debounce
    // window (750 ms, before the flush enqueues a reconcile) read fresh
    // over unflushed edits.
    let backend = backend();
    let dir = fixture();
    let root = root(&dir);
    let submitted = backend
        .index(
            &root,
            IndexInput {
                rebuild: true,
                ..IndexInput::default()
            },
        )
        .await
        .unwrap();
    backend
        .scheduler()
        .wait(&submitted.job.id, None)
        .await
        .unwrap();
    let status = backend.index_status(&root).await.unwrap();
    assert!(status.watcher_active, "{status:?}");
    std::fs::write(
        dir.path().join("a.rs"),
        "fn alpha() {}\nfn alpha_pending() {}\n",
    )
    .unwrap();
    // Poll only inside the debounce window: after the flush, the
    // enqueued reconcile alone would already read as possibly stale.
    let deadline = std::time::Instant::now() + Duration::from_millis(500);
    let mut freshness = ResultFreshness::Fresh;
    while std::time::Instant::now() < deadline {
        let searched = backend
            .search(
                &root,
                SearchQuery {
                    query: Some("alpha function".to_owned()),
                    limit: Some(5),
                    ..SearchQuery::default()
                },
            )
            .await
            .unwrap();
        freshness = searched.freshness;
        if freshness == ResultFreshness::PossiblyStale {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(freshness, ResultFreshness::PossiblyStale);
    backend.close().await;
}

#[tokio::test]
async fn late_waiter_observes_terminal_state() {
    // Regression: tokio 1.53+ `watch::send` drops values without
    // receivers, so the scheduler must store snapshots
    // unconditionally. An empty reconcile finishes before `wait`
    // subscribes; the late waiter must still see terminal state
    // instead of hanging on a stale slot.
    let backend = backend();
    let dir = fixture();
    let root = root(&dir);
    let submitted = backend.index(&root, IndexInput::default()).await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if backend
                .scheduler()
                .get(&submitted.job.id)
                .is_some_and(|snapshot| snapshot.is_terminal())
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let terminal = tokio::time::timeout(
        Duration::from_secs(10),
        backend.scheduler().wait(&submitted.job.id, None),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(terminal.state, IndexJobState::Succeeded);
    backend.close().await;
}

#[tokio::test]
async fn wait_for_fresh_applies_completion_without_resubmit() {
    // Regression (#54): `wait_for_fresh` awaited `scheduler.wait` inside
    // the actor, so the run's `IndexFinished` sat unprocessed in the same
    // mailbox. The runtime still read dirty after the wait and the next
    // loop round submitted a duplicate job. The wait must drain queued
    // completions before re-checking, so one search settles fresh with
    // the same job id.
    let backend = backend();
    let dir = fixture();
    // `c.rs` is created before the first command spawns the actor (and
    // its live filesystem watcher). A post-spawn write would race a
    // `WatchBatch` followup job into the search window, breaking the
    // same-job-id invariant below under load. The explicit incremental
    // re-index of `c.rs` still exercises the wait path deterministically.
    std::fs::write(dir.path().join("c.rs"), "fn gamma() {}\n").unwrap();
    let root = root(&dir);
    let submitted = backend
        .index(
            &root,
            IndexInput {
                rebuild: true,
                ..IndexInput::default()
            },
        )
        .await
        .unwrap();
    backend
        .scheduler()
        .wait(&submitted.job.id, None)
        .await
        .unwrap();
    // Settle the actor-side apply (scheduler terminal precedes it).
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let status = backend.index_status(&root).await.unwrap();
            let settled = status.dirty_revision == status.indexed_revision
                && status
                    .job
                    .as_ref()
                    .is_some_and(IndexJobSnapshot::is_terminal);
            if settled {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let changed = dir.path().join("c.rs");
    let incremental = backend
        .index(
            &root,
            IndexInput {
                changed_paths: vec![changed],
                ..IndexInput::default()
            },
        )
        .await
        .unwrap();
    // Search while the incremental job is still in flight: the fresh-wait
    // must settle that same job, not submit another.
    let searched = backend
        .search(
            &root,
            SearchQuery {
                query: Some("gamma function".to_owned()),
                limit: Some(5),
                freshness: SearchFreshness::WaitForFresh,
                ..SearchQuery::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(searched.freshness, ResultFreshness::Fresh);
    let live = backend.scheduler().get_by_root(&root).unwrap();
    assert_eq!(live.id, incremental.job.id);
    backend.close().await;
}

#[tokio::test]
async fn search_without_index_reports_missing() {
    let backend = backend();
    let dir = tempfile::tempdir().unwrap();
    let error = backend
        .search(
            dir.path().to_str().unwrap(),
            SearchQuery {
                query: Some("x".to_owned()),
                ..SearchQuery::default()
            },
        )
        .await
        .unwrap_err();
    assert_eq!(error.code(), "INDEX_MISSING");
    backend.close().await;
}

#[tokio::test]
async fn rg_and_status_do_not_activate_actor() {
    // Regression (#68): `rg` and `index_status` are read-only; they must
    // not spawn a root actor (watcher + catch-up rebuild) as a side effect.
    let backend = backend();
    let dir = fixture();
    let root = root(&dir);
    let rg = backend
        .rg_search(
            &root,
            RgQuery {
                patterns: vec!["fn alpha".to_owned()],
                ..RgQuery::default()
            },
        )
        .await
        .unwrap();
    assert!(!rg.items.is_empty());
    assert_eq!(backend.server_status().runtimes, 0);
    // Never-indexed status still reports the missing index, without
    // activating an actor to do so.
    let error = backend.index_status(&root).await.unwrap_err();
    assert_eq!(
        error.code(),
        "ZVEC_GREP.ENGINE.CONTEXT.WORKSPACE_INDEX_NOT_FOUND"
    );
    assert_eq!(backend.server_status().runtimes, 0);
    backend.close().await;
}

#[cfg(unix)]
#[tokio::test]
async fn rg_on_read_only_root_succeeds_without_actor() {
    // Regression (#68): `rg` previously demanded a writable root via
    // `activate_for_index`, so read-only workspaces failed outright.
    use std::os::unix::fs::PermissionsExt;
    let backend = backend();
    let dir = fixture();
    let root = root(&dir);
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o555)).unwrap();
    let result = backend
        .rg_search(
            &root,
            RgQuery {
                patterns: vec!["fn alpha".to_owned()],
                ..RgQuery::default()
            },
        )
        .await;
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(!result.unwrap().items.is_empty());
    assert_eq!(backend.server_status().runtimes, 0);
    backend.close().await;
}

#[tokio::test]
async fn status_on_indexed_root_without_actor_reads_persisted_state() {
    // Regression (#68): status on an indexed root with no live actor reads
    // persisted state directly instead of activating (and rebuilding).
    let dir = fixture();
    let root = root(&dir);
    let first = backend();
    let submitted = first
        .index(
            &root,
            IndexInput {
                rebuild: true,
                ..IndexInput::default()
            },
        )
        .await
        .unwrap();
    first
        .scheduler()
        .wait(&submitted.job.id, None)
        .await
        .unwrap();
    first.close().await;
    let second = backend();
    let status = second.index_status(&root).await.unwrap();
    assert!(status.status.files_scanned >= 2, "{status:?}");
    assert!(!status.watcher_active);
    assert_eq!(second.server_status().runtimes, 0);
    second.close().await;
}

#[tokio::test]
async fn drop_during_running_job_waits_for_cancellation_and_drops() {
    // Regression (#72): `handle_drop` cancelled the job request-only and
    // immediately dropped storage, so the still-running blocking index
    // held the write lock and the drop failed BUSY — yet the actor
    // exited anyway, leaving the index on disk with no watcher. The drop
    // must await the cancelled job before deleting storage.
    let gate = Arc::new((
        std::sync::Mutex::new(GateState::default()),
        std::sync::Condvar::new(),
    ));
    let backend = DaemonBackend::new(DaemonBackendOptions {
        service: ServiceConfig {
            model_override: Some(Arc::new(GatedStubModel {
                inner: zg_core::models::stub::StubEmbeddingModel::new(16),
                gate: Arc::clone(&gate),
            })),
            ..ServiceConfig::default()
        },
        ..DaemonBackendOptions::default()
    });
    let dir = fixture();
    let root = root(&dir);
    let submitted = backend
        .index(
            &root,
            IndexInput {
                rebuild: true,
                ..IndexInput::default()
            },
        )
        .await
        .unwrap();
    // Wait until the job is inside `embed`: running, write lock held.
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if gate.0.lock().unwrap().entered {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let drop_backend = backend.clone();
    let drop_root = root.clone();
    let mut drop_task = tokio::spawn(async move { drop_backend.index_drop(&drop_root).await });
    // Give the drop its chance to (incorrectly) race the running job,
    // then let the embedding call return into the cancelled pipeline.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let raced = drop_task.is_finished();
    // Release before asserting: a panicking test must not leave the
    // gated blocking embed (and this test binary) parked forever.
    release_gate(&gate);
    assert!(!raced, "drop raced the running job");
    let dropped = tokio::time::timeout(Duration::from_secs(10), &mut drop_task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(dropped);
    let terminal = backend
        .scheduler()
        .wait(&submitted.job.id, None)
        .await
        .unwrap();
    assert_eq!(terminal.state, IndexJobState::Cancelled);
    assert_eq!(backend.server_status().runtimes, 0);
    let error = backend.index_status(&root).await.unwrap_err();
    assert_eq!(
        error.code(),
        "ZVEC_GREP.ENGINE.CONTEXT.WORKSPACE_INDEX_NOT_FOUND"
    );
    backend.close().await;
    // Second phase (same test slot: two extra live backend actors in
    // parallel measurably destabilize the lock-retry-sensitive backend
    // suite, so the phases run sequentially here instead).
    #[cfg(unix)]
    assert_failed_drop_keeps_actor_alive().await;
}

#[cfg(unix)]
async fn assert_failed_drop_keeps_actor_alive() {
    // Regression (#72): a failed drop returned from `handle` as actor
    // exit regardless of the error, stranding the on-disk index with no
    // watcher. The actor must stay alive on failure and a retry must
    // succeed once the failure cause is gone.
    use std::os::unix::fs::PermissionsExt;
    let backend = backend();
    let dir = fixture();
    let root = root(&dir);
    let submitted = backend
        .index(
            &root,
            IndexInput {
                rebuild: true,
                ..IndexInput::default()
            },
        )
        .await
        .unwrap();
    backend
        .scheduler()
        .wait(&submitted.job.id, None)
        .await
        .unwrap();
    assert_eq!(backend.server_status().runtimes, 1);
    let home = dir.path().join(".zvec-grep-rs");
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o555)).unwrap();
    let result = backend.index_drop(&root).await;
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(result.is_err(), "read-only state dir must fail the drop");
    // Actor survived: still registered and still answering status.
    // Poll: the chmod can trigger a watcher reconcile whose writer
    // legitimately BUSYs a read open for a short window.
    assert_eq!(backend.server_status().runtimes, 1);
    let status = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(status) = backend.index_status(&root).await {
                break status;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    assert!(status.status.files_scanned >= 2, "{status:?}");
    // Retry succeeds and only then does the actor exit. Poll: a
    // transient read open (watcher roots refresh) can legitimately
    // BUSY one attempt; the actor must survive those too.
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if backend.index_drop(&root).await.unwrap_or(false) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(backend.server_status().runtimes, 0);
    backend.close().await;
}
