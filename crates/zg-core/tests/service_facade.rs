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

fn nested_workspace() -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    std::fs::write(dir.path().join("top.txt"), "top-level content\n").expect("write top");
    std::fs::create_dir_all(dir.path().join("repo-a/.git")).expect("nested git dir");
    std::fs::write(dir.path().join("repo-a/.git/HEAD"), "ref\n").expect("marker");
    std::fs::write(
        dir.path().join("repo-a/nested-one.txt"),
        "zephyrnestedone lives in a nested repository\n",
    )
    .expect("write nested");
    dir
}

fn fts_search(
    service: &ZvecGrepService,
    dir: &TempDir,
    term: &str,
) -> zg_core::service::types::ZvecGrepContextResult {
    service
        .context(&ZvecGrepContextOptions {
            root: Some(dir.path()),
            fts: vec![term.to_owned()],
            auto_update: false,
            ..ZvecGrepContextOptions::default()
        })
        .expect("context")
}

fn manifest_home(dir: &TempDir) -> std::path::PathBuf {
    dir.path().join(".zvec-grep")
}

#[test]
fn nested_git_opt_in_persists_across_incremental_reload() {
    use zg_core::manifest::read_workspace_manifest;

    let dir = nested_workspace();
    let service = stub_service(&dir);
    service
        .ensure_index(&ZvecGrepIndexOptions {
            root: Some(dir.path()),
            include_nested_git: Some(true),
            ..ZvecGrepIndexOptions::default()
        })
        .expect("index");
    let found = fts_search(&service, &dir, "zephyrnestedone");
    assert!(
        found
            .items
            .iter()
            .any(|item| item.file.relative_path.ends_with("repo-a/nested-one.txt")),
        "{}",
        serde_json::to_string_pretty(&found.items).expect("json")
    );
    drop(service);

    let service = stub_service(&dir);
    let second = dir.path().join("repo-a/nested-two.txt");
    std::fs::write(
        &second,
        "zephyrnestedtwo lives in the same nested repository\n",
    )
    .expect("write second");
    service
        .ensure_index(&ZvecGrepIndexOptions {
            root: Some(dir.path()),
            changed_paths: vec![second],
            ..ZvecGrepIndexOptions::default()
        })
        .expect("incremental");
    let found = fts_search(&service, &dir, "zephyrnestedtwo");
    assert!(
        found
            .items
            .iter()
            .any(|item| item.file.relative_path.ends_with("repo-a/nested-two.txt")),
        "{}",
        serde_json::to_string_pretty(&found.items).expect("json")
    );
    let info = service.workspace_info(Some(dir.path())).expect("info");
    let workspace = info.workspace_index.expect("workspace");
    assert_eq!(workspace.root_paths.len(), 1);
    assert_eq!(
        workspace.root_paths[0].include_nested_git,
        Some(true),
        "{:?}",
        workspace.root_paths[0]
    );
    let manifest = read_workspace_manifest(&manifest_home(&dir))
        .expect("read manifest")
        .expect("manifest exists");
    assert_eq!(manifest.info.root_paths[0].include_nested_git, Some(true));
}

#[test]
fn nested_git_root_spec_constructors_carry_policy() {
    use zg_core::service::types::RootPathSpec;

    let dir = nested_workspace();
    let service = stub_service(&dir);
    service
        .ensure_index(&ZvecGrepIndexOptions {
            root: Some(dir.path()),
            root_paths: vec![RootPathSpec::Path(".")],
            include_nested_git: Some(true),
            ..ZvecGrepIndexOptions::default()
        })
        .expect("path spec index");
    let found = fts_search(&service, &dir, "zephyrnestedone");
    assert!(
        found
            .items
            .iter()
            .any(|item| item.file.relative_path.ends_with("repo-a/nested-one.txt")),
        "{}",
        serde_json::to_string_pretty(&found.items).expect("json")
    );

    let dir = nested_workspace();
    let service = stub_service(&dir);
    let root = zg_core::types::RootPath {
        absolute_path: dir.path().to_string_lossy().into_owned(),
        recursive: true,
        include: Vec::new(),
        exclude: Vec::new(),
        globs: Vec::new(),
        insensitive_globs: Vec::new(),
        file_types: Vec::new(),
        excluded_file_types: Vec::new(),
        hidden: None,
        no_ignore: None,
        ignore_files: Vec::new(),
        max_depth: None,
        max_file_size_bytes: None,
        follow: None,
        include_nested_git: Some(true),
    };
    service
        .ensure_index(&ZvecGrepIndexOptions {
            root: Some(dir.path()),
            root_paths: vec![RootPathSpec::Full(Box::new(root))],
            ..ZvecGrepIndexOptions::default()
        })
        .expect("full spec index");
    let found = fts_search(&service, &dir, "zephyrnestedone");
    assert!(
        found
            .items
            .iter()
            .any(|item| item.file.relative_path.ends_with("repo-a/nested-one.txt")),
        "{}",
        serde_json::to_string_pretty(&found.items).expect("json")
    );
}

#[test]
fn nested_git_reset_paths_removes_policy_from_index() {
    let dir = nested_workspace();
    let service = stub_service(&dir);
    service
        .ensure_index(&ZvecGrepIndexOptions {
            root: Some(dir.path()),
            include_nested_git: Some(true),
            ..ZvecGrepIndexOptions::default()
        })
        .expect("index");
    assert!(
        !fts_search(&service, &dir, "zephyrnestedone")
            .items
            .is_empty()
    );
    service
        .ensure_index(&ZvecGrepIndexOptions {
            root: Some(dir.path()),
            reset_paths: true,
            ..ZvecGrepIndexOptions::default()
        })
        .expect("reset");
    assert!(
        fts_search(&service, &dir, "zephyrnestedone")
            .items
            .is_empty()
    );
    let info = service.workspace_info(Some(dir.path())).expect("info");
    let workspace = info.workspace_index.expect("workspace");
    assert_eq!(workspace.root_paths[0].include_nested_git, None);
}

#[test]
fn nested_git_manifest_validation() {
    use zg_core::manifest::read_workspace_manifest;

    let dir = nested_workspace();
    let service = stub_service(&dir);
    service
        .ensure_index(&ZvecGrepIndexOptions {
            root: Some(dir.path()),
            include_nested_git: Some(true),
            ..ZvecGrepIndexOptions::default()
        })
        .expect("index");
    let home = manifest_home(&dir);
    let manifest = read_workspace_manifest(&home)
        .expect("read")
        .expect("exists");
    let mut value = serde_json::to_value(&manifest).expect("json");

    let scratch = TempDir::new().expect("tempdir");
    let scratch_home = scratch.path().join(".zvec-grep");
    std::fs::create_dir_all(&scratch_home).expect("mkdir");
    let write_manifest = |value: &serde_json::Value| {
        std::fs::write(
            scratch_home.join("manifest.json"),
            serde_json::to_string_pretty(value).expect("stringify"),
        )
        .expect("write scratch manifest");
    };

    let mut legacy = value.clone();
    legacy["rootPaths"][0]
        .as_object_mut()
        .expect("root")
        .remove("includeNestedGit");
    write_manifest(&legacy);
    let legacy = read_workspace_manifest(&scratch_home).expect("read legacy");
    assert_eq!(
        legacy.expect("legacy exists").info.root_paths[0].include_nested_git,
        None
    );

    let mut explicit_false = value.clone();
    explicit_false["rootPaths"][0]["includeNestedGit"] = serde_json::json!(false);
    write_manifest(&explicit_false);
    let explicit_false = read_workspace_manifest(&scratch_home).expect("read false");
    assert_eq!(
        explicit_false.expect("false exists").info.root_paths[0].include_nested_git,
        Some(false)
    );

    value["rootPaths"][0]["includeNestedGit"] = serde_json::json!("true");
    write_manifest(&value);
    let error = read_workspace_manifest(&scratch_home).expect_err("malformed must fail");
    assert_eq!(error.code(), &zg_core::error::codes::manifest_invalid());
}
