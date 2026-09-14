//! Search-path resolution and result display paths.
//!
//! Requested search paths are split into existing vs missing (diagnostics
//! echo the caller's original spelling), and every match is reported as an
//! absolute + root-relative display pair, mirroring `normalizeResultPath`
//! (`relative(root, abs) || "."`).

use std::fs;
use std::path::{Path, PathBuf};

pub(crate) struct CheckedSearchPaths {
    pub(crate) existing: Vec<String>,
    pub(crate) searched: Vec<String>,
    pub(crate) missing: Vec<String>,
}

/// Splits requested search paths into existing vs missing, mirroring
/// `checkSearchPaths`. Kept original strings so diagnostics echo the caller's
/// spelling.
pub(crate) fn check_search_paths(root: &Path, paths: &[String]) -> CheckedSearchPaths {
    if paths.is_empty() {
        return CheckedSearchPaths {
            existing: Vec::new(),
            searched: Vec::new(),
            missing: Vec::new(),
        };
    }
    let mut existing = Vec::new();
    let mut missing = Vec::new();
    for path in paths {
        if fs::symlink_metadata(resolve_search_path(root, path)).is_ok() {
            existing.push(path.clone());
        } else {
            missing.push(path.clone());
        }
    }
    CheckedSearchPaths {
        searched: existing.clone(),
        existing,
        missing,
    }
}

pub(crate) fn resolve_search_path(root: &Path, path: &str) -> PathBuf {
    let candidate = Path::new(path);
    if candidate.is_absolute() {
        crate::paths::normalize_path(candidate)
    } else {
        crate::paths::normalize_path(&root.join(candidate))
    }
}

pub(crate) fn absolute_normalized(path: &Path) -> PathBuf {
    if path.is_absolute() {
        crate::paths::normalize_path(path)
    } else {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        crate::paths::normalize_path(&cwd.join(path))
    }
}

/// Absolute + root-relative display paths for a match, mirroring
/// `normalizeResultPath` (`relative(root, abs) || "."`).
pub(crate) fn display_paths(root: &Path, path: &Path) -> (String, String) {
    let absolute = crate::paths::normalize_path(path);
    let absolute_display = crate::paths::to_display_path(&absolute);
    let relative = relative_display(root, &absolute);
    (relative, absolute_display)
}

fn relative_display(root: &Path, absolute: &Path) -> String {
    if let Ok(stripped) = absolute.strip_prefix(root) {
        let display = crate::paths::to_display_path(stripped);
        return if display.is_empty() {
            ".".to_owned()
        } else {
            display
        };
    }
    // Outside the root: walk up with `..`, like `path.relative`.
    let mut root_rest = root.components().peekable();
    let mut abs_rest = absolute.components().peekable();
    while root_rest.peek() == abs_rest.peek() {
        if root_rest.peek().is_none() {
            break;
        }
        root_rest.next();
        abs_rest.next();
    }
    let mut parts: Vec<String> = root_rest.map(|_| "..".to_owned()).collect();
    for component in abs_rest {
        parts.push(component.as_os_str().to_string_lossy().replace('\\', "/"));
    }
    if parts.is_empty() {
        ".".to_owned()
    } else {
        parts.join("/")
    }
}
