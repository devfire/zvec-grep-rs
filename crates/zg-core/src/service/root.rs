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

/// Environment variable selecting an explicit index storage directory,
/// mirroring `$ZVEC_GREP_HOME` for global state. An explicit
/// `--index-location` CLI value takes precedence when both are set.
pub const INDEX_LOCATION_ENV_VAR: &str = "ZVEC_GREP_INDEX_LOCATION";

/// Explicit index storage directory from the environment, when set to a
/// non-empty value. Returns `None` when unset or empty.
pub fn index_location_from_env() -> Option<PathBuf> {
    std::env::var_os(INDEX_LOCATION_ENV_VAR)
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
}

/// Full locations for `root` (mirrors `workspaceIndexLocation`).
///
/// # Errors
///
/// Returns `WORKSPACE.ROOT_UNAVAILABLE` when the root cannot be resolved.
pub fn workspace_index_location(root: &str) -> EngineResult<WorkspaceIndexLocation> {
    workspace_index_location_with_override(root, None)
}

/// Full locations for `root`, storing the manifest and index storage under
/// `index_location` instead of `<root>/.zvec-grep`.
///
/// Use this for read-only checkouts (e.g. read-only NFS mounts): the source
/// `root` is only read, while all writes (manifest, `files.json`,
/// `index.zvec`) target the writable `index_location`. Both absolute and
/// relative paths are accepted; relative paths resolve against the current
/// directory. An existing directory is canonicalized (symlinks resolved),
/// exactly like the default home.
///
/// # Errors
///
/// Returns `WORKSPACE.INVALID_INDEX_LOCATION` when `index_location` is an
/// empty path, and `WORKSPACE.ROOT_UNAVAILABLE` when `root` cannot be
/// resolved to an absolute directory.
pub fn workspace_index_location_with_override(
    root: &str,
    index_location: Option<&Path>,
) -> EngineResult<WorkspaceIndexLocation> {
    let resolved = resolve_zvec_grep_root(Some(root))?;
    let Some(explicit) = index_location else {
        return Ok(location_in_default_home(&resolved));
    };
    if explicit.as_os_str().is_empty() {
        return Err(crate::error::EngineError::new(
            crate::error::EngineErrorCode::WorkspaceInvalidIndexLocation,
            "index location must not be empty",
        ));
    }
    let requested_home =
        crate::storage::layout::normalize_absolute_path(explicit.to_string_lossy().as_ref());
    let home = match std::fs::canonicalize(&requested_home) {
        Ok(real) => to_display_path(&real),
        Err(_) => requested_home,
    };
    // Unlike the default home, an explicit location is not derived from the
    // source root, so the root stays the resolved source checkout.
    Ok(location_with_home(resolved, &home))
}

/// Default locations: home is `<root>/.zvec-grep`, root re-derives from the
/// canonicalized home so symlinked checkouts report stable paths.
fn location_in_default_home(resolved: &str) -> WorkspaceIndexLocation {
    let requested_home = workspace_home(resolved);
    let home = match std::fs::canonicalize(&requested_home) {
        Ok(real) => to_display_path(&real),
        Err(_) => requested_home,
    };
    let canonical_root = Path::new(&home)
        .parent()
        .map_or_else(|| resolved.to_owned(), to_display_path);
    location_with_home(canonical_root, &home)
}

/// Locations sharing one storage home directory.
fn location_with_home(root: String, home: &str) -> WorkspaceIndexLocation {
    let home_path = Path::new(home);
    WorkspaceIndexLocation {
        root,
        home: home.to_owned(),
        manifest_path: to_display_path(&workspace_manifest_path(home_path)),
        index_path: to_display_path(&workspace_index_path(home_path)),
    }
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

    #[test]
    fn default_location_stays_in_tree() {
        let location = workspace_index_location("/repo").expect("location");
        assert_eq!(location.root, "/repo");
        assert_eq!(location.home, "/repo/.zvec-grep");
        assert_eq!(location.manifest_path, "/repo/.zvec-grep/manifest.json");
        assert!(location.index_path.ends_with("/repo/.zvec-grep/index.zvec"));
    }

    #[test]
    fn override_moves_storage_out_of_tree() {
        let location = workspace_index_location_with_override(
            "/mnt/ro-repo",
            Some(Path::new("/tmp/writable/zvec-index")),
        )
        .expect("location");
        assert_eq!(location.root, "/mnt/ro-repo");
        assert_eq!(location.home, "/tmp/writable/zvec-index");
        assert_eq!(
            location.manifest_path,
            "/tmp/writable/zvec-index/manifest.json"
        );
        assert_eq!(location.index_path, "/tmp/writable/zvec-index/index.zvec");
    }

    #[test]
    fn override_none_matches_default() {
        let expected = workspace_index_location("/repo").expect("default");
        let actual = workspace_index_location_with_override("/repo", None).expect("override");
        assert_eq!(expected, actual);
    }

    #[test]
    fn override_rejects_empty_path() {
        let error = workspace_index_location_with_override("/repo", Some(Path::new("")))
            .expect_err("empty index location must fail");
        assert_eq!(error.code().suffix(), "WORKSPACE.INVALID_INDEX_LOCATION");
    }

    #[test]
    fn override_canonicalizes_existing_directory() {
        let dir = std::env::temp_dir();
        let location =
            workspace_index_location_with_override("/repo", Some(&dir)).expect("location");
        let canonical = std::fs::canonicalize(&dir).expect("canonicalize");
        assert_eq!(location.home, to_display_path(&canonical));
        assert_eq!(location.root, "/repo");
    }
}
