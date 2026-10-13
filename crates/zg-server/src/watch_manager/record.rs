//! Synchronous single-event recording and fail-open path filtering.
//!
//! Used by `WatchManager::inject_event`/tests: filters against the cached
//! roots snapshot, so the hot path never calls the blocking
//! [`RootPathsSource`] per event.

use std::path::Path;
use std::sync::Arc;

use zg_core::pipeline::indexing::scanner::{PathKind, path_can_affect_index};
use zg_core::types::RootPath;

use crate::change_set::ChangeKind;
use crate::errors::DaemonError;

use super::{RootPathsSource, Shared, set_pending};

/// Classifies one raw event and folds it into the pending set. Synchronous
/// (used by `inject_event`/tests): filters against the cached roots
/// snapshot, so it never calls the blocking [`RootPathsSource`] per event.
///
/// # Errors
///
/// Returns [`DaemonError::RootNotAbsolute`] when the path is relative, or
/// propagates the `ChangeSet` budget failure when the pending set overflows.
pub(super) fn record_raw(
    shared: &Arc<Shared>,
    path: &Path,
    kind: Option<ChangeKind>,
    is_dir_hint: Option<bool>,
) -> Result<(), DaemonError> {
    let absolute = if path.is_absolute() {
        path.to_string_lossy().into_owned()
    } else {
        return Err(DaemonError::RootNotAbsolute {
            root: path.to_string_lossy().into_owned(),
        });
    };
    // Never index internals.
    if is_internal_path(&shared.root, &absolute) {
        return Ok(());
    }
    if is_workspace_signal(&absolute) {
        refresh_roots_blocking(shared);
    }
    let metadata = std::fs::symlink_metadata(&absolute).ok();
    let is_directory =
        is_dir_hint.unwrap_or_else(|| metadata.as_ref().is_some_and(std::fs::Metadata::is_dir));
    let kind = kind.unwrap_or_else(|| {
        if metadata.is_some() {
            ChangeKind::Changed
        } else {
            ChangeKind::Deleted
        }
    });
    if !should_track_cached(shared, &absolute, is_directory) {
        return Ok(());
    }
    let became_pending = {
        let mut changes = shared
            .changes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let was_empty = changes.is_empty();
        changes.add(&absolute, kind, is_directory)?;
        was_empty && !changes.is_empty()
    };
    if became_pending {
        set_pending(shared, true);
    }
    shared.poke();
    Ok(())
}

/// True for paths under `.git` / `.zvec-grep-rs` (own state) or the legacy
/// upstream `.zvec-grep`: never indexed. Pure string prefix check, safe on
/// any thread.
pub(super) fn is_internal_path(root: &str, absolute: &str) -> bool {
    if let Ok(relative) = Path::new(absolute).strip_prefix(Path::new(root))
        && relative.components().any(|component| {
            let name = component.as_os_str();
            name == ".git"
                || name == zg_core::paths::STATE_DIR_NAME
                || name == zg_core::paths::LEGACY_STATE_DIR_NAME
        })
    {
        return true;
    }
    false
}

/// True for events that may change index scoping (ignore rules, workspace
/// manifest): these refresh the cached roots snapshot instead of using it.
fn is_workspace_signal(absolute: &str) -> bool {
    matches!(
        Path::new(absolute)
            .file_name()
            .and_then(|name| name.to_str()),
        Some(".gitignore" | "manifest.json")
    )
}

/// Workspace-signal check on a pre-`String` path (blocking thread side).
pub(super) fn is_workspace_signal_lossy(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|name| name.to_str()),
        Some(".gitignore" | "manifest.json")
    )
}

/// Fail-open filtering against the cached roots snapshot: `.gitignore`
/// always tracks; unreadable rules or a failing predicate track rather
/// than risk a missed change. Never calls the source: the hot path stays
/// off blocking I/O.
fn should_track_cached(shared: &Shared, absolute: &str, is_directory: bool) -> bool {
    if Path::new(absolute)
        .file_name()
        .and_then(|name| name.to_str())
        == Some(".gitignore")
    {
        return true;
    }
    if shared.get_root_paths.is_none() {
        return true;
    }
    let roots = shared
        .roots_cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let kind = if is_directory {
        PathKind::Dir
    } else {
        PathKind::File
    };
    path_can_affect_index(&roots, absolute, kind).unwrap_or(true)
}

/// [`should_track_cached`] without the cache lock: the blocking batch owns
/// a cloned snapshot, so per-event filtering borrows it.
pub(super) fn should_track_roots(
    absolute: &str,
    is_directory: bool,
    has_source: bool,
    roots: &[RootPath],
) -> bool {
    if Path::new(absolute)
        .file_name()
        .and_then(|name| name.to_str())
        == Some(".gitignore")
    {
        return true;
    }
    if !has_source {
        return true;
    }
    let kind = if is_directory {
        PathKind::Dir
    } else {
        PathKind::File
    };
    path_can_affect_index(roots, absolute, kind).unwrap_or(true)
}

/// Calls the roots source fail-open: a panicking source keeps the previous
/// snapshot (`None` = keep, not clear) rather than risk a missed change.
pub(super) fn load_roots(source: Option<&RootPathsSource>) -> Option<Vec<RootPath>> {
    let source = source?;
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| source())).ok()
}

/// Re-snapshots [`Shared::roots_cache`] and clears the stale flag. Blocking
/// (reads the manifest): runs on a `spawn_blocking` thread, at construction,
/// or via `WatchManager::refresh_roots` — never inline on the async loop.
pub(super) fn refresh_roots_blocking(shared: &Shared) {
    if let Some(roots) = load_roots(shared.get_root_paths.as_ref()) {
        *shared
            .roots_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = roots;
    }
    shared
        .roots_stale
        .store(false, std::sync::atomic::Ordering::SeqCst);
}
