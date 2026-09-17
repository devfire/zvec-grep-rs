//! Workspace root locations: `.zvec-grep` home, manifest, index paths.
//!
//! Port of `engine/service/root.ts`: root resolution, index locations,
//! reset, and nearest-workspace discovery.

use std::path::{Path, PathBuf};

use crate::error::EngineResult;
use crate::manifest::{delete_workspace_manifest, workspace_manifest_path};
use crate::paths::to_display_path;
use crate::storage::layout::{
    delete_workspace_index_storage, has_workspace_index_storage,
    resolve_workspace_index_storage_paths, workspace_index_path,
};
use crate::utils::lock::{Guard, LockMode, LockOptions, acquire_read_write_lock};

/// Directory holding the manifest and index storage.
pub const ZVEC_GREP_DIR: &str = ".zvec-grep";

/// Resolved workspace locations (mirrors `WorkspaceIndexLocation`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceIndexLocation {
    pub root: String,
    pub home: String,
    pub manifest_path: String,
    pub index_path: String,
}

/// Absolute resolution of the workspace root (mirrors
/// `resolveZvecGrepRoot`).
///
/// # Errors
///
/// Returns `WORKSPACE.ROOT_UNAVAILABLE` when the current directory cannot be determined.
pub fn resolve_zvec_grep_root(root: Option<&str>) -> EngineResult<String> {
    let base = match root {
        Some(root) => PathBuf::from(root),
        None => std::env::current_dir().map_err(|err| {
            crate::error::EngineError::new(
                crate::error::EngineErrorCode::WorkspaceRootUnavailable,
                "workspace root directory is unavailable",
            )
            .with_context(format!("detail={err}"))
        })?,
    };
    let absolute = if base.is_absolute() {
        base
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(&base))
            .unwrap_or(base)
    };
    Ok(to_display_path(&crate::paths::normalize_path(&absolute)))
}

/// `.zvec-grep` home under `root` (mirrors `workspaceHome`).
#[must_use]
pub fn workspace_home(root: &str) -> String {
    to_display_path(&Path::new(root).join(ZVEC_GREP_DIR))
}

/// Full locations for `root` (mirrors `workspaceIndexLocation`).
///
/// # Errors
///
/// Returns `WORKSPACE.ROOT_UNAVAILABLE` when the root cannot be resolved.
pub fn workspace_index_location(root: &str) -> EngineResult<WorkspaceIndexLocation> {
    let resolved = resolve_zvec_grep_root(Some(root))?;
    let requested_home = workspace_home(&resolved);
    let home = match std::fs::canonicalize(&requested_home) {
        Ok(real) => to_display_path(&real),
        Err(_) => requested_home,
    };
    let canonical_root = Path::new(&home)
        .parent()
        .map(to_display_path)
        .unwrap_or_else(|| resolved.clone());
    let home_path = Path::new(&home);
    Ok(WorkspaceIndexLocation {
        root: canonical_root,
        home: home.clone(),
        manifest_path: to_display_path(&workspace_manifest_path(home_path)),
        index_path: to_display_path(&workspace_index_path(home_path)),
    })
}

/// Acquires the exclusive storage write lock for index maintenance (reset /
/// rebuild): the same lock [`crate::service::workspace_index::WorkspaceIndex::open`]
/// takes in write mode. A live reader/writer or concurrent rebuild holds it,
/// so maintenance fails with `LOCK.BUSY` instead of racing it. Hold the
/// returned guard across deletes and manifest writes; release before opening
/// replacement storage (which takes the same lock for the indexing run).
///
/// The lock path matches storage open exactly (resolved storage path plus
/// `LOCK`), so either side sees the other.
///
/// # Errors
///
/// Returns `LOCK.BUSY` when another owner holds the lock, or
/// `LOCK.UNAVAILABLE` when the lock directory cannot be created.
pub fn acquire_index_maintenance_guard(home: &Path, operation: &str) -> EngineResult<Guard> {
    let paths = resolve_workspace_index_storage_paths(home);
    acquire_read_write_lock(
        &paths.storage_path.join("LOCK"),
        LockMode::Write,
        &LockOptions::new(operation),
    )
}

/// Deletes the manifest and index storage (mirrors `resetWorkspaceIndex`).
///
/// # Errors
///
/// Returns `LOCK.BUSY` when a live reader/writer holds the storage lock, or
/// an error when the manifest or index storage cannot be deleted.
pub fn reset_workspace_index(location: &WorkspaceIndexLocation) -> EngineResult<()> {
    let home = Path::new(&location.home);
    // Exclusive guard across both deletes: a live indexer holds this lock via
    // storage open, so reset fails instead of unlinking data beneath it. A
    // bare existence check would time-of-check/to-time-of-use race; holding
    // the guard is the exclusion.
    let _guard = acquire_index_maintenance_guard(home, "index.reset")?;
    delete_workspace_manifest(home)?;
    delete_workspace_index_storage(home)?;
    Ok(())
}

/// Nearest ancestor (or self) whose manifest and storage both exist (mirrors
/// `findNearestWorkspaceIndex`).
#[must_use]
pub fn find_nearest_workspace_index(start: &str) -> Option<WorkspaceIndexLocation> {
    find_nearest_workspace_location(start, &has_workspace_index)
}

/// Nearest ancestor (or self) with a manifest (mirrors
/// `findNearestWorkspace`).
#[must_use]
pub fn find_nearest_workspace(start: &str) -> Option<WorkspaceIndexLocation> {
    find_nearest_workspace_location(start, &has_workspace_manifest)
}

fn find_nearest_workspace_location(
    start: &str,
    predicate: &dyn Fn(&WorkspaceIndexLocation) -> bool,
) -> Option<WorkspaceIndexLocation> {
    let mut current = resolve_zvec_grep_root(Some(start)).ok()?;
    loop {
        let location = workspace_index_location(&current).ok()?;
        if predicate(&location) {
            return Some(location);
        }
        let parent = Path::new(&current).parent().map(to_display_path)?;
        if parent == current {
            return None;
        }
        current = parent;
    }
}

/// True when a manifest exists (mirrors `hasWorkspaceManifest`).
#[must_use]
pub fn has_workspace_manifest(location: &WorkspaceIndexLocation) -> bool {
    Path::new(&location.manifest_path).is_file()
}

/// True when manifest and storage both exist (mirrors `hasWorkspaceIndex`).
#[must_use]
pub fn has_workspace_index(location: &WorkspaceIndexLocation) -> bool {
    has_workspace_manifest(location) && has_workspace_index_storage(Path::new(&location.home))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::FileId;
    use crate::storage::{StorageOptions, create_workspace_index_storage};
    use crate::types::{
        FileFormat, FileInfo, FileKind, SearchMetric, WorkspaceIndexEmbeddingSchema,
    };

    fn test_location(home: &Path) -> WorkspaceIndexLocation {
        WorkspaceIndexLocation {
            root: to_display_path(home.parent().expect("parent")),
            home: to_display_path(home),
            manifest_path: to_display_path(&home.join("manifest.json")),
            index_path: to_display_path(&home.join("index.zvec")),
        }
    }

    fn dummy_schema() -> WorkspaceIndexEmbeddingSchema {
        WorkspaceIndexEmbeddingSchema {
            provider: "test".to_owned(),
            model: "dummy".to_owned(),
            dimension: 4,
            metric: SearchMetric::Cosine,
        }
    }

    #[test]
    fn reset_fails_busy_under_live_writer() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let home = dir.path().join(".zvec-grep");
        std::fs::create_dir_all(&home).expect("mkdir");
        let location = test_location(&home);
        let schema = dummy_schema();
        let mut storage = create_workspace_index_storage(StorageOptions::ReadWrite {
            storage_path: &home,
            embedding: &schema,
        })
        .expect("open storage");
        // Seed persisted state: one file record (flush writes files.json) plus
        // a manifest, mirroring a live indexed workspace.
        let absolute_path = home.join("seed.txt").to_string_lossy().into_owned();
        std::fs::write(&absolute_path, "0123456789abcdef").expect("seed file");
        storage
            .replace_file(
                &FileInfo {
                    id: FileId::from_raw("seed".to_owned()),
                    absolute_path,
                    relative_path: "seed.txt".to_owned(),
                    root_path: home.to_string_lossy().into_owned(),
                    size_bytes: 16,
                    last_modified_time: crate::types::UnixMillis::from_millis(1_700_000_000_000),
                    content_hash: Some("hash-seed".to_owned()),
                    kind: FileKind::Text,
                    format: FileFormat::parse("text"),
                    index_status: None,
                },
                &[],
                None,
            )
            .expect("seed record");
        storage.flush().expect("flush meta");
        std::fs::write(home.join("manifest.json"), "{}").expect("seed manifest");
        assert!(has_workspace_index_storage(&home));

        let error =
            reset_workspace_index(&location).expect_err("reset under live writer must fail");
        assert_eq!(error.code().to_string(), "ZVEC_GREP.ENGINE.LOCK.BUSY");
        // Live data survives the refused reset.
        assert!(has_workspace_index_storage(&home));
        assert!(home.join("manifest.json").is_file());

        drop(storage);
        reset_workspace_index(&location).expect("reset after close");
        assert!(!has_workspace_index_storage(&home));
        assert!(!home.join("manifest.json").exists());
    }
}
