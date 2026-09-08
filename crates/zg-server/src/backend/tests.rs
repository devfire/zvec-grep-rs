//! Backend round-trip tests: index → status → search over a fixture root.

use std::sync::Arc;
use std::time::Duration;

use super::*;
use zg_core::index_status::IndexJobState;

fn stub_service_config(dimension: usize) -> ServiceConfig {
    ServiceConfig {
        model_override: Some(Arc::new(zg_core::models::stub::StubEmbeddingModel::new(
            dimension,
        ))),
        ..ServiceConfig::default()
    }
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
    dir.path().to_string_lossy().into_owned()
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
