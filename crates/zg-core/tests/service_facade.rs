//! Phase-B facade round trip (requires `--features test-support`).
//!
//! Open / ensure-index / index-status / context / workspace-info /
//! read-session / drop-index against a `tempfile` fixture workspace using the
//! deterministic [`StubEmbeddingModel`](zg_core::models::stub::StubEmbeddingModel).
//! FTS routes carry the determinism (the stub's hash vectors rank
//! arbitrarily); the vector query only proves the search-plan path runs.

#![cfg(feature = "test-support")]
// Test targets exercise fallible fixtures directly: `unwrap`/`expect`/`panic!`
// refusal branches are the same class the crate roots allow under `cfg(test)`
// (integration tests are separate crates, so they carry their own allow).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![allow(clippy::indexing_slicing)]

use std::sync::Arc;

use tempfile::TempDir;
use zg_core::lexical::LexicalSearchOptions;
use zg_core::models::EmbeddingModel;
use zg_core::models::stub::StubEmbeddingModel;
use zg_core::service::facade::{CreateZvecGrepOptions, ZvecGrepService, create_zvec_grep};
use zg_core::service::types::{GroupRole, RgOptions, ZvecGrepContextOptions, ZvecGrepIndexOptions};

const STUB_DIMENSION: usize = 64;

fn fixture_workspace() -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    std::fs::write(
        dir.path().join("a.txt"),
        "hello world from the fixture workspace\n",
    )
    .expect("write a.txt");
    std::fs::write(
        dir.path().join("b.txt"),
        "goodbye moon unrelated content here\n",
    )
    .expect("write b.txt");
    dir
}

fn stub_service(dir: &TempDir) -> ZvecGrepService {
    let stub: Arc<dyn EmbeddingModel> = Arc::new(StubEmbeddingModel::new(STUB_DIMENSION));
    create_zvec_grep(CreateZvecGrepOptions {
        root: Some(dir.path().to_path_buf()),
        embedding_model: Some(stub),
        ..CreateZvecGrepOptions::default()
    })
}

fn index_options<'a>(dir: &'a TempDir) -> ZvecGrepIndexOptions<'a> {
    ZvecGrepIndexOptions {
        root: Some(dir.path()),
        ..ZvecGrepIndexOptions::default()
    }
}

fn search_options<'a>(dir: &'a TempDir) -> ZvecGrepContextOptions<'a> {
    ZvecGrepContextOptions {
        root: Some(dir.path()),
        query: Some("hello".to_owned()),
        fts: vec!["hello".to_owned()],
        auto_update: false,
        ..ZvecGrepContextOptions::default()
    }
}

#[test]
fn open_index_status_search_round_trip() {
    let dir = fixture_workspace();
    let service = stub_service(&dir);

    let location = service.open_workspace(None).expect("open");
    assert!(
        location.root.ends_with(
            dir.path()
                .file_name()
                .expect("name")
                .to_string_lossy()
                .as_ref()
        )
    );

    let result = service.ensure_index(&index_options(&dir)).expect("index");
    assert!(result.files_scanned >= 2, "{result:?}");

    let status = service.index_status(None).expect("status");
    let _ = status;

    let found = service.context(&search_options(&dir)).expect("context");
    assert_eq!(found.query, "hello");
    assert!(!found.items.is_empty(), "fts route must hit a.txt");
    assert!(
        found
            .items
            .iter()
            .any(|item| item.file.relative_path.ends_with("a.txt")),
        "{}",
        serde_json::to_string_pretty(&found.items).expect("json")
    );

    let info = service.workspace_info(None).expect("info");
    assert!(info.indexed);
    let embedding = info.embedding.expect("embedding");
    assert_eq!(embedding.dimension, STUB_DIMENSION);
    assert!(info.status.is_some());
}

#[test]
fn drop_index_reports_existence() {
    let dir = fixture_workspace();
    let service = stub_service(&dir);
    assert!(!service.drop_index(None).expect("empty drop"));
    service.ensure_index(&index_options(&dir)).expect("index");
    assert!(service.drop_index(None).expect("drop"));
    assert!(!service.drop_index(None).expect("second drop"));
}

#[test]
fn read_session_searches_then_fails_closed() {
    let dir = fixture_workspace();
    let service = stub_service(&dir);
    service.ensure_index(&index_options(&dir)).expect("index");

    let session = service.open_read_session(None).expect("session");
    let found = session
        .context(&search_options(&dir))
        .expect("session context");
    assert!(!found.items.is_empty());
    session.close();
}

#[test]
fn rg_search_finds_fixture_term() {
    let dir = fixture_workspace();
    let service = stub_service(&dir);
    let context = ZvecGrepContextOptions {
        root: Some(dir.path()),
        rg: Some(RgOptions {
            pattern: Some("goodbye".to_owned()),
            ..RgOptions::default()
        }),
        ..ZvecGrepContextOptions::default()
    };
    let options =
        LexicalSearchOptions::from_context(dir.path(), vec!["goodbye".to_owned()], &context);
    let result = service.rg_search(&options).expect("rg");
    assert!(!result.items.is_empty());
    assert!(
        result
            .items
            .iter()
            .any(|item| item.file.relative_path.ends_with("b.txt"))
    );
}

#[test]
fn empty_query_errors() {
    let dir = fixture_workspace();
    let service = stub_service(&dir);
    let err = service
        .context(&ZvecGrepContextOptions::default())
        .expect_err("empty query");
    assert_eq!(
        err.code().to_string(),
        "ZVEC_GREP.ENGINE.CONTEXT.EMPTY_QUERY"
    );
}

#[test]
fn bare_query_is_hybrid_without_fts_crutch() {
    let dir = fixture_workspace();
    let service = stub_service(&dir);
    service.ensure_index(&index_options(&dir)).expect("index");
    let found = service
        .context(&ZvecGrepContextOptions {
            root: Some(dir.path()),
            query: Some("hello".to_owned()),
            auto_update: false,
            ..ZvecGrepContextOptions::default()
        })
        .expect("context");
    assert_eq!(found.query, "hello");
    assert!(
        found
            .items
            .iter()
            .any(|item| item.file.relative_path.ends_with("a.txt")),
        "{}",
        serde_json::to_string_pretty(&found.items).expect("json")
    );
    assert!(
        found
            .items
            .iter()
            .any(|item| matches!(item.matched_by.as_deref(), Some("fts") | Some("fts+vector"))),
        "bare query must run the lexical leg"
    );
    let groups = found.group_results.expect("groups");
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].id, "Q1");
    assert_eq!(groups[0].role, Some(GroupRole::Primary));
    assert!(
        found
            .items
            .iter()
            .all(|item| item.query_groups.iter().any(|group| group.id == "Q1")),
        "every item links to its group"
    );
}

#[test]
fn fts_only_routes_request_succeeds() {
    let dir = fixture_workspace();
    let service = stub_service(&dir);
    service.ensure_index(&index_options(&dir)).expect("index");
    let found = service
        .context(&ZvecGrepContextOptions {
            root: Some(dir.path()),
            fts: vec!["hello".to_owned()],
            auto_update: false,
            ..ZvecGrepContextOptions::default()
        })
        .expect("routes-only context");
    assert_eq!(found.query, "hello");
    assert!(!found.items.is_empty());
    let groups = found.group_results.expect("groups");
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].role, Some(GroupRole::Supplemental));
}

#[test]
fn fuse_collapses_two_primary_queries() {
    let dir = fixture_workspace();
    let service = stub_service(&dir);
    service.ensure_index(&index_options(&dir)).expect("index");
    let options = |fuse: bool| ZvecGrepContextOptions {
        root: Some(dir.path()),
        queries: vec!["hello".to_owned(), "moon".to_owned()],
        fuse,
        auto_update: false,
        ..ZvecGrepContextOptions::default()
    };
    let split = service.context(&options(false)).expect("split");
    assert_eq!(split.query, "hello | moon");
    let split_groups = split.group_results.expect("groups");
    assert_eq!(split_groups.len(), 2);
    assert_eq!(split_groups[0].id, "Q1");
    assert_eq!(split_groups[1].id, "Q2");
    assert!(
        split_groups
            .iter()
            .all(|group| group.role == Some(GroupRole::Primary))
    );
    let fused = service.context(&options(true)).expect("fused");
    let fused_groups = fused.group_results.expect("groups");
    assert_eq!(fused_groups.len(), 1);
    assert_eq!(fused_groups[0].id, "Q1");
    assert_eq!(fused_groups[0].role, Some(GroupRole::Primary));
}
