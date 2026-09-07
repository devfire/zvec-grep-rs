//! Workspace index storage path resolution, existence check, safe delete.
//!
//! Ports `engine/storage/layout.ts`. The entity vector collection keeps the
//! TypeScript on-disk name (`index.zvec`). File metadata lived in a second
//! zvec collection (`files.zvec`) in TypeScript; this port stores it as JSON
//! (`files.json`) instead — see [`crate::storage::zvec::store`]. Layout
//! helpers know both names so existence checks and deletes keep working
//! across indexes written by either generation.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use crate::error::{EngineError, EngineErrorCode, EngineResult};

/// Legacy TypeScript file-metadata zvec collection directory name.
pub const FILES_ZVEC_DIR: &str = "files.zvec";
/// File-metadata JSON store name used by this port.
pub const FILES_META_FILE: &str = "files.json";
/// Entity vector collection directory name (shared with TypeScript).
pub const ENTITIES_ZVEC_DIR: &str = "index.zvec";

/// Resolved on-disk locations for one workspace index storage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceIndexStoragePaths {
    pub storage_path: PathBuf,
    pub files_path: PathBuf,
    pub files_meta_path: PathBuf,
    pub index_path: PathBuf,
}

/// Resolves `storagePath`, `files.zvec`, `files.json`, and `index.zvec`.
pub fn resolve_workspace_index_storage_paths(storage_path: &Path) -> WorkspaceIndexStoragePaths {
    let resolved = PathBuf::from(normalize_absolute_path(
        storage_path.to_string_lossy().as_ref(),
    ));
    WorkspaceIndexStoragePaths {
        storage_path: resolved.clone(),
        files_path: resolved.join(FILES_ZVEC_DIR),
        files_meta_path: resolved.join(FILES_META_FILE),
        index_path: resolved.join(ENTITIES_ZVEC_DIR),
    }
}

/// True when the entity collection exists together with file metadata in
/// either the current (`files.json`) or legacy (`files.zvec`) location.
pub fn has_workspace_index_storage(storage_path: &Path) -> bool {
    let paths = resolve_workspace_index_storage_paths(storage_path);
    paths.index_path.exists() && (paths.files_meta_path.exists() || paths.files_path.exists())
}

/// Removes index data inside `storage_path`. Targets are fixed to direct
/// children of the storage directory; anything else is refused.
pub fn delete_workspace_index_storage(storage_path: &Path) -> EngineResult<()> {
    let paths = resolve_workspace_index_storage_paths(storage_path);
    for target in [&paths.files_path, &paths.files_meta_path, &paths.index_path] {
        let inside = target
            .parent()
            .is_some_and(|parent| parent == paths.storage_path);
        if !inside {
            return Err(EngineError::new(
                EngineErrorCode::from_static("STORAGE.INVALID_STORAGE_PATH"),
                "workspace index data must be inside its storage path",
            )
            .with_context(format!("path={}", target.display())));
        }
        // `files.json` is a file and `index.zvec` a directory:
        // `remove_dir_all` on a file fails with `ENOTDIR`, so branch on the
        // file kind first instead of only falling back on `NotFound`.
        let removal = if target.is_dir() {
            std::fs::remove_dir_all(target)
        } else {
            std::fs::remove_file(target)
        };
        removal
            .or_else(|error| {
                if error.kind() == std::io::ErrorKind::NotFound {
                    Ok(())
                } else {
                    Err(error)
                }
            })
            .map_err(|error| {
                EngineError::new(
                    EngineErrorCode::from_static("STORAGE.DELETE_FAILED"),
                    "failed to delete workspace index storage",
                )
                .with_context(format!("path={} error={error}", target.display()))
            })?;
    }
    Ok(())
}

/// Resolves the entity collection path for `storage_path`.
pub fn workspace_index_path(storage_path: &Path) -> PathBuf {
    resolve_workspace_index_storage_paths(storage_path).index_path
}

/// Makes `path` absolute and lexically normal without touching the file
/// system (mirrors `node:path.resolve`).
pub fn normalize_absolute_path(path: &str) -> String {
    let current = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let absolute = {
        let candidate = Path::new(path);
        if candidate.is_absolute() {
            candidate.to_path_buf()
        } else {
            current.join(candidate)
        }
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(Component::RootDir.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(part) => normalized.push(part),
        }
    }
    normalized.to_string_lossy().into_owned()
}

/// True when `path` equals one of `prefixes` or sits beneath one, walking
/// parent segments exactly like the TypeScript implementation.
pub fn path_has_prefix(path: &str, prefixes: &HashSet<String>) -> bool {
    let mut current = path;
    loop {
        if prefixes.contains(current) {
            return true;
        }
        let Some(pos) = current.rfind('/') else {
            return false;
        };
        if pos == 0 {
            return prefixes.contains("/");
        }
        current = &current[..pos];
    }
}
