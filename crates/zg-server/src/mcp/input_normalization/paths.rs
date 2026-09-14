//! Root-scoped path resolution for managed-rg paths.

use std::path::{Path, PathBuf};

use crate::mcp::error::McpError;

/// Lexical path resolution against the root: rejects escapes (mirrors
/// `assertRootScopedPath` with the TS `{label}` messages).
pub(crate) fn assert_root_scoped(root: &str, path: &str) -> Result<(), McpError> {
    let resolved = join_root(root, path);
    if path_escapes(root, &resolved) {
        return Err(McpError::invalid_params(format!(
            "search path must stay within root: {path}"
        )));
    }
    let canonical = canonical_through_ancestors(&resolved);
    let canonical_root = canonical_through_ancestors(root);
    if path_escapes(
        &canonical_root.to_string_lossy(),
        &canonical.to_string_lossy(),
    ) {
        return Err(McpError::invalid_params(format!(
            "search path resolves outside root: {path}"
        )));
    }
    Ok(())
}

fn join_root(root: &str, path: &str) -> String {
    let joined = Path::new(root).join(path);
    normalize_lexical(&joined)
}

/// Lexical `..`/`.` normalization without touching the filesystem.
fn normalize_lexical(path: &Path) -> String {
    let mut parts: Vec<std::ffi::OsString> = Vec::new();
    for component in path.components() {
        use std::path::Component;
        match component {
            Component::ParentDir => {
                parts.pop();
            }
            Component::CurDir => {}
            other @ Component::Prefix(_)
            | other @ Component::RootDir
            | other @ Component::Normal(_) => parts.push(other.as_os_str().to_owned()),
        }
    }
    let mut normalized = PathBuf::new();
    normalized.extend(parts);
    normalized.to_string_lossy().into_owned()
}

fn path_escapes(root: &str, path: &str) -> bool {
    let root = root.trim_end_matches('/');
    if path == root {
        return false;
    }
    !path.starts_with('/') || !path.starts_with(&format!("{root}/"))
}

/// Canonicalizes through the nearest existing ancestor (mirrors
/// `resolveThroughExistingAncestor`).
fn canonical_through_ancestors(path: &str) -> PathBuf {
    let mut current = PathBuf::from(path);
    let mut missing = Vec::new();
    loop {
        match current.canonicalize() {
            Ok(canonical) => {
                let mut full = canonical;
                for segment in missing {
                    full.push(segment);
                }
                return full;
            }
            Err(_) => {
                let Some(parent) = current.parent().map(Path::to_path_buf) else {
                    return PathBuf::from(path);
                };
                let Some(name) = current.file_name().map(|name| name.to_owned()) else {
                    return PathBuf::from(path);
                };
                if parent == current {
                    return PathBuf::from(path);
                }
                missing.insert(0, name);
                current = parent;
            }
        }
    }
}
