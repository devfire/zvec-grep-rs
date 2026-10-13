use super::classify::classify_batch;
use super::*;
use std::sync::Mutex as StdMutex;

/// Batches delivered by the test manager.
type RecordedBatches = Arc<StdMutex<Vec<(ChangeSetSnapshot, WatchReason)>>>;

fn manager(root: &str) -> (WatchManager, RecordedBatches) {
    let batches: RecordedBatches = Arc::new(StdMutex::new(Vec::new()));
    let batches_clone = batches.clone();
    let manager = WatchManager::new(WatchManagerOptions {
        root: root.to_owned(),
        debounce: Some(Duration::from_millis(5)),
        max_wait: Some(Duration::from_millis(50)),
        max_changed_paths: None,
        on_changes: Arc::new(move |snapshot, reason| {
            batches_clone.lock().unwrap().push((snapshot, reason));
        }),
        get_root_paths: None,
        initial_roots: None,
        on_pending: None,
    });
    (manager, batches)
}

#[tokio::test]
async fn coalesces_events_into_one_batch() {
    let (manager, batches) = manager("/repo");
    manager
        .inject_event("/repo/a.rs", ChangeKind::Changed, false)
        .unwrap();
    manager
        .inject_event("/repo/b.rs", ChangeKind::Created, false)
        .unwrap();
    manager.flush_now();
    let batches = batches.lock().unwrap();
    assert_eq!(batches.len(), 1);
    let (snapshot, reason) = batches.iter().next().expect("one batch");
    assert_eq!(snapshot.touched_files, vec!["/repo/a.rs", "/repo/b.rs"]);
    assert_eq!(*reason, WatchReason::Watch);
}

#[tokio::test]
async fn internal_only_batches_skip_roots_refresh() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_clone = Arc::clone(&calls);
    let source: RootPathsSource = Arc::new(move || {
        calls_clone.fetch_add(1, Ordering::SeqCst);
        Vec::new()
    });
    let batches: RecordedBatches = Arc::new(StdMutex::new(Vec::new()));
    let batches_clone = Arc::clone(&batches);
    let manager = WatchManager::new(WatchManagerOptions {
        root: "/repo".to_owned(),
        debounce: Some(Duration::from_millis(5)),
        max_wait: Some(Duration::from_millis(50)),
        max_changed_paths: None,
        on_changes: Arc::new(move |snapshot, reason| {
            batches_clone.lock().unwrap().push((snapshot, reason));
        }),
        get_root_paths: Some(source),
        initial_roots: None,
        on_pending: None,
    });
    // Construction takes one blocking snapshot up front.
    let baseline = calls.load(Ordering::SeqCst);
    // Index internals — the per-run manifest rewrite, storage files,
    // lock files — classify to nothing and must not touch the roots
    // source: it is a blocking storage open whose read lock would race
    // index maintenance (drop/reset fail `LOCK.BUSY` under a live reader).
    let mut batch = HashMap::new();
    batch.insert(
        PathBuf::from("/repo/.zvec-grep-rs/manifest.json"),
        Some(ChangeKind::Changed),
    );
    batch.insert(
        PathBuf::from("/repo/.zvec-grep-rs/LOCK.readers/x/lock.json"),
        Some(ChangeKind::Created),
    );
    batch.insert(
        PathBuf::from("/repo/.zvec-grep/manifest.json"),
        Some(ChangeKind::Changed),
    );
    assert!(classify_batch(&manager.shared, &batch).is_empty());
    assert_eq!(calls.load(Ordering::SeqCst), baseline);
    // An external workspace signal still refreshes.
    let mut batch = HashMap::new();
    batch.insert(PathBuf::from("/repo/.gitignore"), Some(ChangeKind::Changed));
    assert_eq!(classify_batch(&manager.shared, &batch).len(), 1);
    assert_eq!(calls.load(Ordering::SeqCst), baseline + 1);
}

#[tokio::test]
async fn skips_index_internals() {
    let (manager, batches) = manager("/repo");
    manager
        .inject_event("/repo/.git/HEAD", ChangeKind::Changed, false)
        .unwrap();
    manager
        .inject_event("/repo/.zvec-grep-rs/files.json", ChangeKind::Changed, false)
        .unwrap();
    manager
        .inject_event("/repo/.zvec-grep/files.json", ChangeKind::Changed, false)
        .unwrap();
    manager.flush_now();
    assert!(batches.lock().unwrap().is_empty());
}

#[tokio::test]
async fn debounce_delivers_without_manual_flush() {
    let (manager, batches) = manager("/repo");
    manager
        .inject_event("/repo/a.rs", ChangeKind::Changed, false)
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(batches.lock().unwrap().len(), 1);
    drop(manager);
}

#[tokio::test]
async fn full_reconcile_flushes_as_reconcile() {
    let (manager, batches) = manager("/repo");
    manager.require_full_reconcile();
    manager.flush_now();
    let batches = batches.lock().unwrap();
    assert_eq!(batches.len(), 1);
    let (snapshot, reason) = batches.iter().next().expect("one batch");
    assert!(snapshot.force_full_reconcile);
    assert_eq!(*reason, WatchReason::Reconcile);
}

#[tokio::test]
async fn consecutive_flushes_deliver_only_new_changes() {
    let (manager, batches) = manager("/repo");
    manager
        .inject_event("/repo/a.rs", ChangeKind::Changed, false)
        .unwrap();
    manager.flush_now();
    manager
        .inject_event("/repo/b.rs", ChangeKind::Changed, false)
        .unwrap();
    manager.flush_now();
    let batches = batches.lock().unwrap();
    let touched = batches
        .iter()
        .map(|(snapshot, _)| snapshot.touched_files.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        touched,
        vec![vec!["/repo/a.rs".to_owned()], vec!["/repo/b.rs".to_owned()]]
    );
}

#[tokio::test]
async fn reconcile_flag_does_not_stick_across_batches() {
    let (manager, batches) = manager("/repo");
    manager.require_full_reconcile();
    manager.flush_now();
    manager
        .inject_event("/repo/a.rs", ChangeKind::Changed, false)
        .unwrap();
    manager.flush_now();
    let batches = batches.lock().unwrap();
    assert_eq!(batches.len(), 2);
    let (first, first_reason) = batches.first().expect("first batch");
    assert!(first.force_full_reconcile);
    assert_eq!(*first_reason, WatchReason::Reconcile);
    let (second, second_reason) = batches.get(1).expect("second batch");
    assert!(!second.force_full_reconcile);
    assert_eq!(*second_reason, WatchReason::Watch);
    assert_eq!(second.touched_files, vec!["/repo/a.rs".to_owned()]);
}

fn write_manifest_policy(home: &Path, root: &str, include_nested_git: Option<bool>) {
    use zg_core::config::EmbeddingRuntimeConfig;
    use zg_core::manifest::{
        CURRENT_MANIFEST_VERSION, WorkspaceManifest, write_workspace_manifest,
    };
    use zg_core::types::{UnixMillis, WorkspaceIndexInfo, WorkspaceIndexPolicy};

    std::fs::create_dir_all(home).expect("mkdir home");
    let manifest = WorkspaceManifest {
        info: WorkspaceIndexInfo {
            id: "watch-test".to_owned(),
            name: "watch-test".to_owned(),
            path: home.to_string_lossy().into_owned(),
            root_paths: vec![RootPath {
                absolute_path: root.to_owned(),
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
                include_nested_git,
            }],
            index_policy: Some(WorkspaceIndexPolicy::Enabled),
            embedding: Some(None),
            index_version: Some(zg_core::types::CURRENT_INDEX_VERSION),
            created_time: UnixMillis::now(),
            updated_time: UnixMillis::now(),
        },
        manifest_version: CURRENT_MANIFEST_VERSION,
        embedding_runtime: EmbeddingRuntimeConfig::default(),
    };
    write_workspace_manifest(home, &manifest).expect("write manifest");
}

#[tokio::test]
async fn nested_git_policy_gates_watcher_batches() {
    use zg_core::manifest::read_workspace_manifest;

    let dir = tempfile::tempdir().expect("tempdir");
    let root = zg_core::paths::to_display_path(dir.path());
    std::fs::create_dir_all(dir.path().join("repo-a/.git")).expect("mkdir");
    std::fs::write(dir.path().join("repo-a/.git/HEAD"), "ref\n").expect("marker");
    std::fs::write(dir.path().join("repo-a/a.txt"), "a\n").expect("nested file");
    let home = dir.path().join(zg_core::paths::STATE_DIR_NAME);
    write_manifest_policy(&home, &root, None);

    let home_clone = home.clone();
    let source: RootPathsSource = Arc::new(move || {
        read_workspace_manifest(&home_clone)
            .ok()
            .flatten()
            .map(|manifest| manifest.info.root_paths)
            .unwrap_or_default()
    });
    // One manager across phases: each flush drains, so every batch
    // holds only post-flush changes.
    let batches: RecordedBatches = Arc::new(StdMutex::new(Vec::new()));
    let batches_clone = batches.clone();
    let manager = WatchManager::new(WatchManagerOptions {
        root: root.clone(),
        debounce: Some(Duration::from_secs(3600)),
        max_wait: Some(Duration::from_secs(3600)),
        max_changed_paths: None,
        on_changes: Arc::new(move |snapshot, reason| {
            batches_clone.lock().unwrap().push((snapshot, reason));
        }),
        get_root_paths: Some(source),
        initial_roots: None,
        on_pending: None,
    });
    let touched_batches = || {
        batches
            .lock()
            .unwrap()
            .iter()
            .map(|(snapshot, _)| snapshot.touched_files.clone())
            .collect::<Vec<_>>()
    };

    let nested = format!("{root}/repo-a/a.txt");
    manager
        .inject_event(&nested, ChangeKind::Changed, false)
        .unwrap();
    manager.flush_now();
    assert!(
        touched_batches().iter().all(Vec::is_empty),
        "disabled policy tracks no nested change"
    );

    write_manifest_policy(&home, &root, Some(true));
    manager.refresh_roots();
    manager
        .inject_event(&nested, ChangeKind::Changed, false)
        .unwrap();
    manager.flush_now();
    let delivered = touched_batches();
    let latest = delivered.iter().last().expect("enabled phase flushes");
    assert_eq!(
        latest,
        &vec![nested.clone()],
        "enabled policy yields the nested touched path"
    );

    let batch_count = batches.lock().unwrap().len();
    manager
        .inject_event(
            &format!("{root}/repo-a/.git/HEAD"),
            ChangeKind::Changed,
            false,
        )
        .unwrap();
    manager
        .inject_event(
            &format!("{root}/.zvec-grep-rs/manifest.json"),
            ChangeKind::Changed,
            false,
        )
        .unwrap();
    manager
        .inject_event(
            &format!("{root}/.zvec-grep/manifest.json"),
            ChangeKind::Changed,
            false,
        )
        .unwrap();
    manager.flush_now();
    assert_eq!(
        batches.lock().unwrap().len(),
        batch_count,
        ".git, .zvec-grep-rs and legacy .zvec-grep changes stay untracked"
    );
}
