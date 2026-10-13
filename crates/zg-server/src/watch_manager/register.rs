//! Per-directory non-recursive watch registration (#71).
//!
//! `notify`'s recursive inotify watch registers every subdirectory,
//! including ignored trees such as `node_modules`/`target`, and can
//! exhaust `fs.inotify.max_user_watches`. Instead each directory that
//! may affect the index gets its own `NonRecursive` watch, pruned by
//! the same scanner predicate that filters events
//! ([`path_can_affect_index`]): ignored, hidden, excluded, nested-git,
//! depth-capped, and internal directories are never registered, so
//! they consume no watches and produce no events.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use notify::{RecursiveMode, Watcher};
use zg_core::pipeline::indexing::scanner::{PathKind, path_can_affect_index};
use zg_core::types::RootPath;

use super::Shared;
use super::record::is_internal_path;

fn lock<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Roots used for registration pruning. The cached index roots apply
/// once an index exists; before that (or without a roots source) a
/// synthetic default root over the workspace applies the scanner's
/// default ignore/hidden rules so ignored trees are pruned from the
/// start.
fn effective_roots(shared: &Shared) -> Vec<RootPath> {
    let cached = lock(&shared.roots_cache).clone();
    if shared.get_root_paths.is_some() && !cached.is_empty() {
        return cached;
    }
    vec![RootPath {
        absolute_path: shared.root.clone(),
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
        include_nested_git: None,
    }]
}

/// True when `dir` may contain indexable content under `roots`:
/// scanner-approved, or an ancestor of a configured index root (the
/// walk must descend through it to reach that root). Fail-open on
/// scanner errors, mirroring event filtering.
fn dir_may_be_watched(shared: &Shared, roots: &[RootPath], dir: &Path) -> bool {
    let display = dir.to_string_lossy().into_owned();
    if is_internal_path(&shared.root, &display) {
        return false;
    }
    if roots.iter().any(|root| {
        let root_path = Path::new(&root.absolute_path);
        root_path != dir && root_path.starts_with(dir)
    }) {
        return true;
    }
    path_can_affect_index(roots, &display, PathKind::Dir).unwrap_or(true)
}

/// Iterative walk collecting every watchable directory at or below
/// `start`. Symlinks are not followed (scanner parity without
/// `follow`, and no watch cycles). `start` itself is included only
/// when watchable, except the workspace root, which the caller
/// always watches so top-level ignore/manifest changes are seen.
fn collect_dirs(shared: &Shared, start: &Path, include_start: bool) -> Vec<PathBuf> {
    let roots = effective_roots(shared);
    if !include_start && !dir_may_be_watched(shared, &roots, start) {
        return Vec::new();
    }
    let mut collected = Vec::new();
    let mut stack = vec![start.to_path_buf()];
    while let Some(dir) = stack.pop() {
        collected.push(dir.clone());
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if !file_type.is_dir() {
                continue;
            }
            let child = entry.path();
            if dir_may_be_watched(shared, &roots, &child) {
                stack.push(child);
            }
        }
    }
    collected
}

/// Registers `dirs`, skipping already-watched ones. A failure on
/// `required` (the workspace root at startup) propagates; other
/// failures skip that directory (vanished mid-walk, watch quota),
/// leaving the rest of the tree watched.
fn watch_dirs(
    shared: &Shared,
    dirs: &[PathBuf],
    required: Option<&Path>,
) -> Result<(), notify::Error> {
    let mut watcher = lock(&shared.watcher);
    let Some(watcher) = watcher.as_mut() else {
        return Ok(());
    };
    let mut registered = lock(&shared.watched);
    for dir in dirs {
        if registered.contains(dir) {
            continue;
        }
        match watcher.watch(dir, RecursiveMode::NonRecursive) {
            Ok(()) => {
                registered.insert(dir.clone());
            }
            Err(error) => {
                if required == Some(dir.as_path()) {
                    return Err(error);
                }
            }
        }
    }
    Ok(())
}

/// Registers the whole workspace tree at startup.
///
/// # Errors
///
/// Returns the notify error when the workspace root itself cannot be
/// watched; subdirectory failures are skipped (see [`watch_dirs`]).
pub(super) fn register_initial(shared: &Shared) -> Result<(), notify::Error> {
    let root = PathBuf::from(&shared.root);
    let dirs = collect_dirs(shared, &root, true);
    watch_dirs(shared, &dirs, Some(&root))
}

/// Reconciles registrations with the current tree and ignore rules:
/// newly watchable directories are added, newly ignored or vanished
/// ones are unwatched. Runs on `.gitignore` changes and after the
/// index-roots snapshot refreshes.
pub(super) fn sync_watch_tree(shared: &Shared) {
    if lock(&shared.watcher).is_none() {
        return;
    }
    let root = PathBuf::from(&shared.root);
    let desired: HashSet<PathBuf> = collect_dirs(shared, &root, true).into_iter().collect();
    let stale: Vec<PathBuf> = lock(&shared.watched)
        .difference(&desired)
        .cloned()
        .collect();
    if !stale.is_empty() {
        let mut watcher = lock(&shared.watcher);
        let mut registered = lock(&shared.watched);
        for dir in &stale {
            if let Some(watcher) = watcher.as_mut() {
                // Deleted directories lose their inotify watch on their
                // own; an unwatch error there is expected, not fatal.
                let _ = watcher.unwatch(dir);
            }
            registered.remove(dir);
        }
    }
    let fresh: Vec<PathBuf> = desired
        .into_iter()
        .filter(|dir| !lock(&shared.watched).contains(dir))
        .collect();
    let _ = watch_dirs(shared, &fresh, None);
}

/// Registers a directory created or moved into the tree, plus any
/// subdirectories it already contains.
fn add_watch_tree(shared: &Shared, start: &Path) {
    let dirs = collect_dirs(shared, start, false);
    let _ = watch_dirs(shared, &dirs, None);
}

/// Unwatches `start` and every watched directory below it (deleted or
/// moved out of the tree; the watch would otherwise follow the inode
/// outside the workspace).
fn remove_watch_tree(shared: &Shared, start: &Path) {
    let mut watcher = lock(&shared.watcher);
    let mut registered = lock(&shared.watched);
    let doomed: Vec<PathBuf> = registered
        .iter()
        .filter(|dir| dir.as_path() == start || dir.starts_with(start))
        .cloned()
        .collect();
    for dir in &doomed {
        if let Some(watcher) = watcher.as_mut() {
            let _ = watcher.unwatch(dir);
        }
        registered.remove(dir);
    }
}

/// Registration upkeep for one classified event path: an existing
/// directory gains watches, a vanished path drops them, and a
/// `.gitignore` change reconciles the whole tree against the new
/// rules. Must run on the blocking classify thread, never in the
/// notify callback: the inotify backend delivers events on the same
/// thread that services `watch`/`unwatch`, so calling them from the
/// callback deadlocks the backend.
pub(super) fn reconcile_event_path(shared: &Shared, path: &Path) {
    if path.file_name().and_then(|name| name.to_str()) == Some(".gitignore") {
        sync_watch_tree(shared);
        return;
    }
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => add_watch_tree(shared, path),
        Ok(_) => {}
        Err(_) => remove_watch_tree(shared, path),
    }
}

/// Drops every registration (watcher close/drop).
pub(super) fn clear_watches(shared: &Shared) {
    *lock(&shared.watcher) = None;
    lock(&shared.watched).clear();
}
