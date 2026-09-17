//! Search-path resolution and result display paths.
//!
//! Requested search paths are split into existing vs missing (diagnostics
//! echo the caller's original spelling), and every match is reported as an
//! absolute + root-relative display pair, mirroring `normalizeResultPath`
//! (`relative(root, abs) || "."`).
//!
//! Resolution is containment-gated: every candidate is canonicalized through
//! its nearest existing ancestor (resolving symlinks) and denied unless the
//! target sits inside the canonical root. Absolute paths and `..` escapes
//! pointing outside the root are errors, never silently searched.

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{EngineError, EngineErrorCode, EngineResult};

pub(crate) struct CheckedSearchPaths {
    pub(crate) existing: Vec<String>,
    pub(crate) searched: Vec<String>,
    pub(crate) missing: Vec<String>,
}

/// Splits requested search paths into existing vs missing, mirroring
/// `checkSearchPaths`. Kept original strings so diagnostics echo the caller's
/// spelling. Paths resolving outside the root are denied with
/// `LEXICAL.SEARCH_FAILED` instead of being searched.
pub(crate) fn check_search_paths(
    root: &Path,
    paths: &[String],
) -> EngineResult<CheckedSearchPaths> {
    if paths.is_empty() {
        return Ok(CheckedSearchPaths {
            existing: Vec::new(),
            searched: Vec::new(),
            missing: Vec::new(),
        });
    }
    let mut existing = Vec::new();
    let mut missing = Vec::new();
    for path in paths {
        let resolved = resolve_search_path(root, path)?;
        // Follow symlinks before the existence check so dangling links read
        // as missing instead of searchable.
        if fs::metadata(&resolved).is_ok() {
            existing.push(path.clone());
        } else {
            missing.push(path.clone());
        }
    }
    Ok(CheckedSearchPaths {
        searched: existing.clone(),
        existing,
        missing,
    })
}

/// Resolves one requested search path against `root`, denying anything that
/// escapes it. Absolute candidates are checked as-is; relative ones resolve
/// under the root. Both are canonicalized through the nearest existing
/// ancestor (so symlinks resolve before the check) and gated with an
/// is-inside-root test against the canonical root.
pub(crate) fn resolve_search_path(root: &Path, path: &str) -> EngineResult<PathBuf> {
    let absolute_root = absolute_normalized(root);
    let candidate = Path::new(path);
    let joined = if candidate.is_absolute() {
        crate::paths::normalize_path(candidate)
    } else {
        crate::paths::normalize_path(&absolute_root.join(candidate))
    };
    let canonical_root = canonical_through_ancestors(&absolute_root);
    let canonical_candidate = canonical_through_ancestors(&joined);
    if !crate::paths::is_path_inside(&canonical_root, &canonical_candidate) {
        return Err(EngineError::new(
            EngineErrorCode::LexicalSearchFailed,
            format!("search path resolves outside root: {path}"),
        ));
    }
    Ok(joined)
}

/// Canonicalizes through the nearest existing ancestor, resolving symlinks
/// without requiring the full path to exist: missing trailing segments are
/// re-appended to the canonical ancestor. Falls back to the input when
/// nothing along the chain exists.
pub(crate) fn canonical_through_ancestors(path: &Path) -> PathBuf {
    let mut current = path.to_path_buf();
    let mut missing: Vec<std::ffi::OsString> = Vec::new();
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
                let Some(name) = current.file_name().map(|name| name.to_owned()) else {
                    return path.to_path_buf();
                };
                let Some(parent) = current.parent().map(Path::to_path_buf) else {
                    return path.to_path_buf();
                };
                if parent == current {
                    return path.to_path_buf();
                }
                missing.insert(0, name);
                current = parent;
            }
        }
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
