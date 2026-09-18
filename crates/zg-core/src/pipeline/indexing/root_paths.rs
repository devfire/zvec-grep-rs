//! Workspace root-path normalization, validation, and matching.
//!
//! Port of `engine/pipeline/indexing/root-paths.ts`: root normalization,
//! overlap validation (path + file-identity), and include/exclude matching.

use std::path::{Path, PathBuf};

use crate::error::{EngineError, EngineErrorCode, EngineResult};
use crate::paths::{is_path_inside, normalize_path, to_display_path};
use crate::types::RootPath;
use crate::utils::glob::{normalize_path_pattern, path_pattern_matches};

/// Normalizes one root path: absolute form, `recursive` defaulting to true
/// (mirrors `normalizeRootPath`; callers pass `recursive: true` explicitly,
/// this keeps the stored value as-is).
#[must_use]
pub fn normalize_root_path(root: &RootPath) -> RootPath {
    let mut normalized = root.clone();
    normalized.absolute_path = display_normalized(&root.absolute_path);
    normalized
}

/// Validates root paths and rejects overlapping scan domains (mirrors
/// `validateRootPaths`).
///
/// # Errors
///
/// Returns `SCANNER.ROOT_PATH_STAT_FAILED` when a root cannot be inspected,
/// `SCANNER.UNSUPPORTED_ROOT_PATH` when a root is neither a file nor a directory, or
/// `SCANNER.OVERLAPPING_ROOT_PATHS` when two roots overlap.
pub fn validate_root_paths(roots: &[RootPath]) -> EngineResult<Vec<RootPath>> {
    let normalized: Vec<RootPath> = roots.iter().map(normalize_root_path).collect();
    let mut domains = Vec::with_capacity(normalized.len());
    for root in &normalized {
        domains.push(root_path_to_scan_domain(root)?);
    }
    for left in 0..domains.len() {
        for right in (left + 1)..domains.len() {
            let (Some(left_domain), Some(right_domain)) = (domains.get(left), domains.get(right))
            else {
                continue;
            };
            if scan_domains_overlap(left_domain, right_domain) {
                return Err(EngineError::new(
                    EngineErrorCode::ScannerOverlappingRootPaths,
                    "workspace index root paths overlap",
                )
                .with_context(format!(
                    "left={} right={}",
                    left_domain.root.absolute_path, right_domain.root.absolute_path
                )));
            }
        }
    }
    Ok(normalized)
}

/// True when `absolute_path` belongs to `root` and passes its patterns
/// (mirrors `fileBelongsToRootPath`).
#[must_use]
pub fn file_belongs_to_root_path(absolute_path: &str, root: &RootPath) -> bool {
    if !is_path_inside(Path::new(&root.absolute_path), Path::new(absolute_path)) {
        return false;
    }
    let relative_path = display_relative(&root.absolute_path, absolute_path);
    matches_root_patterns(&relative_path, root)
}

/// Exclude-first, include-gated matching (mirrors `matchesRootPatterns`).
#[must_use]
pub fn matches_root_patterns(relative_path: &str, root: &RootPath) -> bool {
    if matches_any(relative_path, &root.exclude) {
        return false;
    }
    if root.include.is_empty() {
        return true;
    }
    matches_any(relative_path, &root.include)
}

/// True when any include pattern matches (mirrors
/// `matchesRootIncludePatterns`).
#[must_use]
pub fn matches_root_include_patterns(relative_path: &str, root: &RootPath) -> bool {
    matches_any(relative_path, &root.include)
}

/// True when any exclude pattern matches (mirrors
/// `matchesRootExcludePatterns`).
#[must_use]
pub fn matches_root_exclude_patterns(relative_path: &str, root: &RootPath) -> bool {
    matches_any(relative_path, &root.exclude)
}
fn matches_any(relative_path: &str, patterns: &[String]) -> bool {
    patterns
        .iter()
        .any(|pattern| path_pattern_matches(pattern, relative_path))
}

/// Per-scan compiled include/exclude patterns for one root (#35).
///
/// Built once per root per scan and shared by every per-file check, so the
/// hot path does matching only and never builds a regex per file. Semantics
/// mirror [`matches_root_patterns`]: exclude-first, include-gated.
#[derive(Debug, Clone)]
pub struct CompiledRootPatterns {
    include: Vec<crate::utils::glob::CompiledPathPattern>,
    exclude: Vec<crate::utils::glob::CompiledPathPattern>,
    has_include: bool,
}

impl CompiledRootPatterns {
    /// Compiles `root` include/exclude patterns once for repeated matching.
    #[must_use]
    pub fn new(root: &RootPath) -> Self {
        let compile = |patterns: &[String]| {
            patterns
                .iter()
                .map(|pattern| crate::utils::glob::CompiledPathPattern::new(pattern))
                .filter(|compiled| !compiled.is_never())
                .collect::<Vec<_>>()
        };
        let has_include = !root.include.is_empty();
        Self {
            include: compile(&root.include),
            exclude: compile(&root.exclude),
            has_include,
        }
    }

    /// Exclude-first, include-gated matching (compiled [`matches_root_patterns`]).
    #[must_use]
    pub fn matches(&self, relative_path: &str) -> bool {
        if self.matches_exclude(relative_path) {
            return false;
        }
        if !self.has_include {
            return true;
        }
        self.matches_include(relative_path)
    }

    /// True when any include pattern matches (compiled
    /// [`matches_root_include_patterns`]).
    #[must_use]
    pub fn matches_include(&self, relative_path: &str) -> bool {
        self.include
            .iter()
            .any(|pattern| pattern.matches(relative_path))
    }

    /// True when any exclude pattern matches (compiled
    /// [`matches_root_exclude_patterns`]).
    #[must_use]
    pub fn matches_exclude(&self, relative_path: &str) -> bool {
        self.exclude
            .iter()
            .any(|pattern| pattern.matches(relative_path))
    }

    /// True when an include pattern matches `relative_directory` or names a
    /// path underneath it (compiled nested-git explicit-include check: a
    /// repository directory stays visible when an include pattern covers it).
    #[must_use]
    pub fn include_covers_directory(&self, relative_directory: &str) -> bool {
        if !self.has_include {
            return false;
        }
        let normalized = normalize_path_pattern(relative_directory);
        self.include.iter().any(|pattern| {
            pattern.matches(relative_directory)
                || (!pattern.normalized_pattern().is_empty()
                    && pattern
                        .normalized_pattern()
                        .starts_with(&format!("{normalized}/")))
        })
    }
}

fn display_normalized(path: &str) -> String {
    to_display_path(&normalize_path(Path::new(path)))
}

fn display_relative(root: &str, path: &str) -> String {
    match Path::new(path).strip_prefix(Path::new(root)) {
        Ok(relative) => to_display_path(relative),
        Err(_) => to_display_path(Path::new(path)),
    }
}

enum ScanDomainKind {
    File,
    Directory,
}

struct RootScanDomain {
    root: RootPath,
    real_path: String,
    kind: ScanDomainKind,
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
}

fn root_path_to_scan_domain(root: &RootPath) -> EngineResult<RootScanDomain> {
    let path = Path::new(&root.absolute_path);
    let info = std::fs::metadata(path).map_err(|err| {
        EngineError::new(
            EngineErrorCode::ScannerRootPathStatFailed,
            "workspace index root path could not be inspected",
        )
        .with_context(format!("rootPath={} detail={err}", root.absolute_path))
    })?;
    if !info.is_file() && !info.is_dir() {
        return Err(EngineError::new(
            EngineErrorCode::ScannerUnsupportedRootPath,
            "workspace index root path must be a file or directory",
        )
        .with_context(format!("rootPath={}", root.absolute_path)));
    }
    let kind = if info.is_file() {
        ScanDomainKind::File
    } else {
        ScanDomainKind::Directory
    };
    let real_path = std::fs::canonicalize(path)
        .map(|real| to_display_path(&real))
        .unwrap_or_else(|_| root.absolute_path.clone());
    Ok(RootScanDomain {
        root: root.clone(),
        real_path,
        kind,
        #[cfg(unix)]
        dev: std::os::unix::fs::MetadataExt::dev(&info),
        #[cfg(unix)]
        ino: std::os::unix::fs::MetadataExt::ino(&info),
    })
}

fn same_file_identity(left: &RootScanDomain, right: &RootScanDomain) -> bool {
    if left.real_path == right.real_path {
        return true;
    }
    #[cfg(unix)]
    {
        left.dev == right.dev && left.ino != 0 && left.ino == right.ino
    }
    #[cfg(not(unix))]
    {
        false
    }
}

fn scan_domains_overlap(left: &RootScanDomain, right: &RootScanDomain) -> bool {
    if same_file_identity(left, right) {
        return true;
    }
    match (&left.kind, &right.kind) {
        (ScanDomainKind::File, ScanDomainKind::File) => false,
        (ScanDomainKind::Directory, ScanDomainKind::Directory) => {
            left.real_path == right.real_path
                || directory_covers_directory(left, &right.real_path)
                || directory_covers_directory(right, &left.real_path)
        }
        _ => {
            let (directory, file_path) = match left.kind {
                ScanDomainKind::Directory => (left, right.real_path.as_str()),
                ScanDomainKind::File => (right, left.real_path.as_str()),
            };
            directory_covers_file(directory, file_path)
        }
    }
}

fn directory_covers_directory(directory: &RootScanDomain, child: &str) -> bool {
    directory.root.recursive && is_path_inside(Path::new(&directory.real_path), Path::new(child))
}

fn directory_covers_file(directory: &RootScanDomain, file_path: &str) -> bool {
    if !is_path_inside(Path::new(&directory.real_path), Path::new(file_path)) {
        return false;
    }
    if directory.root.recursive {
        return true;
    }
    Path::new(file_path)
        .parent()
        .map(to_display_path)
        .as_deref()
        == Some(directory.real_path.as_str())
}

/// Parent directory display of `path`.
pub fn parent_display(path: &str) -> String {
    Path::new(path)
        .parent()
        .map(to_display_path)
        .unwrap_or_default()
}

#[must_use]
pub fn path_buf(path: &str) -> PathBuf {
    normalize_path(Path::new(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(path: &str) -> RootPath {
        RootPath {
            absolute_path: path.to_owned(),
            recursive: true,
            ..RootPath::default()
        }
    }

    #[test]
    fn include_gate_blocks_non_matching() {
        let root = RootPath {
            include: vec!["src/**".to_owned()],
            ..root("/repo")
        };
        assert!(matches_root_patterns("src/main.rs", &root));
        assert!(!matches_root_patterns("docs/readme.md", &root));
    }

    #[test]
    fn exclude_wins_over_include() {
        let root = RootPath {
            include: vec!["**".to_owned()],
            exclude: vec!["*.log".to_owned()],
            ..root("/repo")
        };
        assert!(!matches_root_patterns("debug.log", &root));
        assert!(matches_root_patterns("main.rs", &root));
    }
}
