//! Workspace root locations: `.zvec-grep` home, manifest, index paths.
//!
//! Port of `engine/service/root.ts`: root resolution, index locations,
//! reset, and nearest-workspace discovery.

use std::path::{Path, PathBuf};

use crate::error::EngineResult;
use crate::manifest::{delete_workspace_manifest, workspace_manifest_path};
use crate::paths::to_display_path;
use crate::storage::layout::{
    delete_workspace_index_storage, has_workspace_index_storage, workspace_index_path,
};

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
pub fn resolve_zvec_grep_root(root: Option<&str>) -> EngineResult<String> {
    let base = match root {
        Some(root) => PathBuf::from(root),
        None => std::env::current_dir().map_err(|err| {
            crate::error::EngineError::new(
                crate::error::EngineErrorCode::from_static("WORKSPACE.ROOT_UNAVAILABLE"),
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
pub fn workspace_home(root: &str) -> String {
    to_display_path(&Path::new(root).join(ZVEC_GREP_DIR))
}

/// Full locations for `root` (mirrors `workspaceIndexLocation`).
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

/// Deletes the manifest and index storage (mirrors `resetWorkspaceIndex`).
pub fn reset_workspace_index(location: &WorkspaceIndexLocation) -> EngineResult<()> {
    let home = Path::new(&location.home);
    delete_workspace_manifest(home)?;
    delete_workspace_index_storage(home)?;
    Ok(())
}

/// Nearest ancestor (or self) whose manifest and storage both exist (mirrors
/// `findNearestWorkspaceIndex`).
pub fn find_nearest_workspace_index(start: &str) -> Option<WorkspaceIndexLocation> {
    find_nearest_workspace_location(start, &has_workspace_index)
}

/// Nearest ancestor (or self) with a manifest (mirrors
/// `findNearestWorkspace`).
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
pub fn has_workspace_manifest(location: &WorkspaceIndexLocation) -> bool {
    Path::new(&location.manifest_path).is_file()
}

/// True when manifest and storage both exist (mirrors `hasWorkspaceIndex`).
pub fn has_workspace_index(location: &WorkspaceIndexLocation) -> bool {
    has_workspace_manifest(location) && has_workspace_index_storage(Path::new(&location.home))
}
