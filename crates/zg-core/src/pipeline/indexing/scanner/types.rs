//! Scanner core types: result, options, cancellation, shared constants.
//!
//! The path/query helpers at the bottom are shared by every scanner
//! submodule, so they live here (the common module) rather than in `walk`.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::error::{EngineError, EngineErrorCode, EngineResult};
use crate::paths::{is_path_inside, normalize_path, to_display_path};
use crate::pipeline::indexing::root_paths::{normalize_root_path, validate_root_paths};
use crate::types::{FileInfo, FileScanDiagnostics, RootPath};

pub(crate) const BINARY_SNIFF_BYTES: usize = 8192;
pub(crate) const BINARY_CONTROL_CHAR_RATIO: f64 = 0.3;
pub(crate) const MAX_GITIGNORE_CACHE_ENTRIES: usize = 4_096;
pub(crate) const MAX_SKIPPED_FILE_SAMPLES: usize = 20;

pub(crate) const DEFAULT_IGNORED_DIRECTORY_NAMES: &[&str] = &[
    "node_modules",
    "vendor",
    "thirdparty",
    "third_party",
    "external",
    "deps",
    "dist",
    "build",
    "out",
    "target",
    "coverage",
    "generated",
    "__pycache__",
    "venv",
    ".venv",
    "env",
    ".tox",
    ".eggs",
    "Pods",
    ".next",
    ".nuxt",
    ".svelte-kit",
    ".turbo",
    ".vite",
    ".parcel-cache",
    ".cache",
    ".gradle",
    ".pytest_cache",
    ".mypy_cache",
    ".ruff_cache",
    "tmp",
    "temp",
    "logs",
    "locale",
    "locales",
    "translations",
];

pub(crate) const DEFAULT_IGNORED_FILE_PATTERNS: &[&str] = &[
    "*.lock",
    "*.lockb",
    "*-lock.json",
    "*-lock.yaml",
    "npm-shrinkwrap.json",
    "go.sum",
    "*.resolved",
    "*.po",
    "*.pot",
    "*.map",
    "*.min.*",
    "*.bundle.*",
    "*.generated.*",
    "*.gen.*",
    "*.designer.*",
    "*.pb.*",
    "*_pb2.*",
    "*.g.*",
    "*.gif",
    "*.jpeg",
    "*.jpg",
    "*.png",
    "*.webp",
];

pub(crate) const HARD_SKIP_HIDDEN_NAMES: &[&str] = &[".git", ".zvec-grep"];

/// Files plus skip diagnostics from one scan (mirrors `ScanResult`).
#[derive(Debug, Clone, Default)]
pub struct ScanResult {
    pub files: Vec<FileInfo>,
    pub diagnostics: FileScanDiagnostics,
}

/// Scan inputs (mirrors `ScanOptions` without the abort signal).
#[derive(Debug, Clone, Default)]
pub struct ScanOptions {
    pub known_files: Vec<FileInfo>,
    pub cancel: Option<CancelFlag>,
}

/// Shared cancellation flag (replaces `AbortSignal` in sync code).
///
/// The inner [`std::sync::Arc`] is private: construct with [`CancelFlag::new`], trip it
/// with [`CancelFlag::cancel`], probe it with [`CancelFlag::is_cancelled`]
/// (M2). Clones share one flag, so one `cancel()` trips every holder.
#[derive(Debug, Clone, Default)]
pub struct CancelFlag(std::sync::Arc<AtomicBool>);

impl CancelFlag {
    /// A flag that starts un-cancelled.
    #[must_use]
    pub fn new() -> Self {
        Self(std::sync::Arc::new(AtomicBool::new(false)))
    }

    /// Trips the flag for every clone sharing it.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    /// True once [`CancelFlag::cancel`] ran on any clone.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

pub(crate) fn throw_if_cancelled(cancel: Option<&CancelFlag>) -> EngineResult<()> {
    if cancel.is_some_and(CancelFlag::is_cancelled) {
        return Err(EngineError::new(
            EngineErrorCode::from_static("INDEXING.CANCELLED"),
            "indexing was cancelled",
        ));
    }
    Ok(())
}

pub(crate) fn path_outside_root(root: &str, path: &str) -> bool {
    if path == root {
        return false;
    }
    !is_path_inside(Path::new(root), Path::new(path))
}

pub(crate) fn strip_root_prefix(root: &str, path: &str) -> String {
    if path == root {
        return String::new();
    }
    display_relative(root, path)
}

pub(crate) fn display_relative(root: &str, path: &str) -> String {
    match Path::new(path).strip_prefix(Path::new(root)) {
        Ok(relative) => to_display_path(relative),
        Err(_) => to_display_path(Path::new(path)),
    }
}

pub(crate) fn parent_display(path: &str) -> String {
    Path::new(path)
        .parent()
        .map(to_display_path)
        .unwrap_or_default()
}

pub(crate) fn file_name_of(path: &str) -> String {
    Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_owned()
}

pub(crate) fn known_files_by_path(files: &[FileInfo]) -> HashMap<String, &FileInfo> {
    files
        .iter()
        .map(|file| {
            (
                to_display_path(&normalize_path(Path::new(&file.absolute_path))),
                file,
            )
        })
        .collect()
}

pub(crate) fn matching_root_paths(root_paths: &[RootPath], absolute_path: &str) -> Vec<RootPath> {
    let Ok(validated) = validate_root_paths(root_paths) else {
        return Vec::new();
    };
    validated
        .into_iter()
        .filter(|root| !path_outside_root(&normalize_root_path(root).absolute_path, absolute_path))
        .collect()
}
