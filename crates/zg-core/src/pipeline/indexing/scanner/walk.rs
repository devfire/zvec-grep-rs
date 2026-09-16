//! Root scanning entry points and recursive directory walk.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::error::EngineResult;
use crate::paths::to_display_path;
use crate::pipeline::indexing::root_paths::{
    matches_root_exclude_patterns, matches_root_patterns, normalize_root_path, validate_root_paths,
};
use crate::types::{FileInfo, FileScanDiagnostics, RootPath};
use crate::utils::file_selection::{FileSelection, OrderedGlobs, resolve_file_types};
use crate::utils::glob::{normalize_path_pattern, path_pattern_matches};

use super::file_info::{create_scan_diagnostics, read_file_info};
use super::hidden::{path_can_be_scanned, should_skip_hidden_directory, should_skip_hidden_file};
use super::ignore::{
    IgnoreRule, default_ignore_rules, ignore_rules_for_directory, ignored_path_explicitly_included,
    match_ignore_rules, read_configured_ignore_rules, read_gitignore_rules,
};
use super::types::{CancelFlag, PathKind, ScanOptions, ScanResult, throw_if_cancelled};
use super::types::{HARD_SKIP_HIDDEN_NAMES, display_relative, file_name_of, known_files_by_path};
use super::types::{matching_root_paths, parent_display, path_outside_root, strip_root_prefix};

/// Scans every configured root (mirrors `scanRootPaths`).
///
/// # Errors
///
/// Returns `SCANNER.OVERLAPPING_ROOT_PATHS` for overlapping roots,
/// `SCANNER.ROOT_PATH_STAT_FAILED` or `SCANNER.UNSUPPORTED_ROOT_PATH` for bad roots,
/// `INDEXING.CANCELLED` when cancelled, or per-root scan errors.
pub fn scan_root_paths(
    workspace_index_id: &str,
    root_paths: &[RootPath],
    options: &ScanOptions,
) -> EngineResult<ScanResult> {
    let validated = validate_root_paths(root_paths)?;
    let mut files = Vec::new();
    let mut diagnostics = create_scan_diagnostics();
    let known = known_files_by_path(&options.known_files);
    for root in &validated {
        throw_if_cancelled(options.cancel.as_ref())?;
        scan_root_path(
            workspace_index_id,
            root,
            &mut files,
            &mut diagnostics,
            options,
            &known,
        )?;
    }
    Ok(ScanResult { files, diagnostics })
}

/// Scans one file path under the matching roots (mirrors `scanFilePath`).
///
/// # Errors
///
/// Returns `INDEXING.CANCELLED` when cancelled, or an error when ignore rules, file
/// selection, or file reads fail.
pub fn scan_file_path(
    workspace_index_id: &str,
    root_paths: &[RootPath],
    absolute_path: &str,
    options: &ScanOptions,
) -> EngineResult<ScanResult> {
    let mut files = Vec::new();
    let mut diagnostics = create_scan_diagnostics();
    let known = known_files_by_path(&options.known_files);
    for configured in matching_root_paths(root_paths, absolute_path) {
        throw_if_cancelled(options.cancel.as_ref())?;
        let root = normalize_root_path(&configured);
        let Some(meta) = follow_symlink_target(&root, absolute_path) else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        if !root.recursive && parent_display(absolute_path) != root.absolute_path {
            continue;
        }
        let relative_path = display_relative(&root.absolute_path, absolute_path);
        let rules = ignore_rules_for_directory(&root, &parent_display(absolute_path))?;
        let selection = root_file_selection(&root)?;
        if !path_can_be_scanned(
            &root,
            &relative_path,
            &file_name_of(absolute_path),
            false,
            &rules,
        ) || !selection.matches(&relative_path)
            || has_excluded_nested_git_ancestor(&root, absolute_path, PathKind::File)?
        {
            continue;
        }
        if let Some(file) = read_file_info(
            workspace_index_id,
            &root,
            absolute_path,
            &mut diagnostics,
            &known,
        )? {
            files.push(file);
        }
    }
    Ok(ScanResult {
        files: dedupe_files(files),
        diagnostics,
    })
}

/// True when `absolute_path` could affect the index (mirrors
/// `pathCanAffectIndex`). `kind` picks directory-vs-file semantics for the
/// recursive gate, max-depth bounds, and the nested-git ancestor check.
///
/// # Errors
///
/// Returns an error when ignore rules or file selection fail.
pub fn path_can_affect_index(
    root_paths: &[RootPath],
    absolute_path: &str,
    kind: PathKind,
) -> EngineResult<bool> {
    let is_directory = matches!(kind, PathKind::Dir);
    for configured in root_paths {
        let root = normalize_root_path(configured);
        if path_outside_root(&root.absolute_path, absolute_path) {
            continue;
        }
        let path_from_root = strip_root_prefix(&root.absolute_path, absolute_path);
        if path_from_root.is_empty() {
            return Ok(true);
        }
        let depth = path_from_root.split('/').filter(|s| !s.is_empty()).count();
        if (!root.recursive
            && (is_directory || parent_display(absolute_path) != root.absolute_path))
            || root.max_depth.is_some_and(|max| {
                if is_directory {
                    depth as u32 >= max
                } else {
                    depth as u32 > max
                }
            })
        {
            continue;
        }
        let relative_path = to_display_path(Path::new(&path_from_root));
        let rules = ignore_rules_for_directory(&root, &parent_display(absolute_path))?;
        if !path_can_be_scanned(
            &root,
            &relative_path,
            &file_name_of(absolute_path),
            is_directory,
            &rules,
        ) || has_excluded_nested_git_ancestor(&root, absolute_path, kind)?
        {
            continue;
        }
        if is_directory {
            return Ok(true);
        }
        let selection = root_file_selection(&root)?;
        if selection.matches(&relative_path) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Scans one directory under the matching roots (mirrors
/// `scanDirectoryPath`).
///
/// # Errors
///
/// Returns `INDEXING.CANCELLED` when cancelled, or an error when directory scanning,
/// ignore rules, or file reads fail.
pub fn scan_directory_path(
    workspace_index_id: &str,
    root_paths: &[RootPath],
    absolute_path: &str,
    options: &ScanOptions,
) -> EngineResult<ScanResult> {
    let mut files = Vec::new();
    let mut diagnostics = create_scan_diagnostics();
    let known = known_files_by_path(&options.known_files);
    for configured in matching_root_paths(root_paths, absolute_path) {
        throw_if_cancelled(options.cancel.as_ref())?;
        let root = normalize_root_path(&configured);
        if !root.recursive && absolute_path != root.absolute_path {
            continue;
        }
        let Some(meta) = follow_symlink_target(&root, absolute_path) else {
            continue;
        };
        if !meta.is_dir() {
            continue;
        }
        let relative_path = display_relative(&root.absolute_path, absolute_path);
        let parent_rules = ignore_rules_for_directory(&root, &parent_display(absolute_path))?;
        let selection = root_file_selection(&root)?;
        if !relative_path.is_empty()
            && (!path_can_be_scanned(
                &root,
                &relative_path,
                &file_name_of(absolute_path),
                true,
                &parent_rules,
            ) || has_excluded_nested_git_ancestor(&root, absolute_path, PathKind::Dir)?)
        {
            continue;
        }
        let root_real = real_path_of(&root.absolute_path);
        let dir_real = real_path_of(absolute_path);
        let mut visited = HashSet::new();
        visited.insert(root_real);
        visited.insert(dir_real);
        let depth = relative_path.split('/').filter(|s| !s.is_empty()).count();
        walk(
            workspace_index_id,
            &root,
            absolute_path,
            &mut files,
            &mut diagnostics,
            &parent_rules,
            &selection,
            &mut visited,
            depth,
            options.cancel.as_ref(),
            &known,
        )?;
    }
    Ok(ScanResult {
        files: dedupe_files(files),
        diagnostics,
    })
}

fn real_path_of(path: &str) -> String {
    std::fs::canonicalize(path)
        .map(|real| to_display_path(&real))
        .unwrap_or_else(|_| path.to_owned())
}

/// Resolves `absolute_path` metadata honoring `root.follow`: a symlink is
/// followed to its target only when following is enabled, otherwise the
/// link itself is reported (or `None` when nothing exists).
fn follow_symlink_target(root: &RootPath, absolute_path: &str) -> Option<std::fs::Metadata> {
    let target = std::fs::symlink_metadata(absolute_path).ok();
    match (&target, root.follow.unwrap_or(false)) {
        (Some(meta), true) if meta.file_type().is_symlink() => {
            std::fs::metadata(absolute_path).ok()
        }
        _ => target,
    }
}

fn root_file_selection(root: &RootPath) -> EngineResult<FileSelection> {
    Ok(FileSelection {
        globs: OrderedGlobs::new(&root.globs, &root.insensitive_globs),
        types: resolve_file_types(&root.file_types, &root.excluded_file_types)?,
    })
}

#[allow(clippy::too_many_arguments)]
fn walk(
    workspace_index_id: &str,
    root: &RootPath,
    current_path: &str,
    files: &mut Vec<FileInfo>,
    diagnostics: &mut FileScanDiagnostics,
    parent_ignore_rules: &[IgnoreRule],
    selection: &FileSelection,
    visited_directories: &mut HashSet<String>,
    depth: usize,
    cancel: Option<&CancelFlag>,
    known_files: &HashMap<String, &FileInfo>,
) -> EngineResult<()> {
    throw_if_cancelled(cancel)?;
    let mut ignore_rules: Vec<IgnoreRule> = parent_ignore_rules.to_vec();
    if !root.no_ignore.unwrap_or(false) {
        ignore_rules.extend(read_gitignore_rules(root, current_path)?);
    }
    let Ok(entries) = std::fs::read_dir(current_path) else {
        return Ok(());
    };
    for entry in entries.flatten() {
        throw_if_cancelled(cancel)?;
        let absolute_path = to_display_path(&entry.path());
        let relative_path = display_relative(&root.absolute_path, &absolute_path);
        let file_type = entry.file_type().ok();
        let mut is_directory = file_type.is_some_and(|kind| kind.is_dir());
        let mut is_file = file_type.is_some_and(|kind| kind.is_file());
        if file_type.is_some_and(|kind| kind.is_symlink()) && root.follow.unwrap_or(false) {
            if let Ok(target) = std::fs::metadata(&absolute_path) {
                is_directory = target.is_dir();
                is_file = target.is_file();
            } else {
                is_directory = false;
                is_file = false;
            }
        }
        let name = file_name_of(&absolute_path);

        if is_directory {
            if !root.recursive {
                continue;
            }
            if root.max_depth.is_some_and(|max| depth + 1 >= max as usize) {
                continue;
            }
            let ignore_match = match_ignore_rules(&relative_path, true, &ignore_rules);
            if HARD_SKIP_HIDDEN_NAMES.contains(&name.as_str())
                || matches_root_exclude_patterns(&relative_path, root)
                || (ignore_match.ignored
                    && !ignored_path_explicitly_included(&relative_path, root, ignore_match))
                || should_skip_hidden_directory(&name, &relative_path, root)
            {
                continue;
            }
            if nested_git_repository_blocked(root, &absolute_path, &relative_path) {
                continue;
            }
            let real_directory = real_path_of(&absolute_path);
            if !visited_directories.insert(real_directory) {
                continue;
            }
            walk(
                workspace_index_id,
                root,
                &absolute_path,
                files,
                diagnostics,
                &ignore_rules,
                selection,
                visited_directories,
                depth + 1,
                cancel,
                known_files,
            )?;
            continue;
        }

        if !is_file {
            continue;
        }
        if root.max_depth.is_some_and(|max| depth + 1 > max as usize) {
            continue;
        }
        if HARD_SKIP_HIDDEN_NAMES.contains(&name.as_str()) {
            continue;
        }
        let ignore_match = match_ignore_rules(&relative_path, false, &ignore_rules);
        if ignore_match.ignored
            && !ignored_path_explicitly_included(&relative_path, root, ignore_match)
        {
            continue;
        }
        if should_skip_hidden_file(&name, &relative_path, root) {
            continue;
        }
        if !matches_root_patterns(&relative_path, root) {
            continue;
        }
        if !selection.matches(&relative_path) {
            continue;
        }
        throw_if_cancelled(cancel)?;
        if let Some(file) = read_file_info(
            workspace_index_id,
            root,
            &absolute_path,
            diagnostics,
            known_files,
        )? {
            files.push(file);
        }
    }
    Ok(())
}

fn scan_root_path(
    workspace_index_id: &str,
    root: &RootPath,
    files: &mut Vec<FileInfo>,
    diagnostics: &mut FileScanDiagnostics,
    options: &ScanOptions,
    known_files: &HashMap<String, &FileInfo>,
) -> EngineResult<()> {
    throw_if_cancelled(options.cancel.as_ref())?;
    let selection = root_file_selection(root)?;
    let Ok(info) = std::fs::metadata(&root.absolute_path) else {
        return Ok(());
    };
    if info.is_file() {
        let relative_path = file_name_of(&root.absolute_path);
        if selection.matches(&relative_path)
            && let Some(file) = read_file_info(
                workspace_index_id,
                root,
                &root.absolute_path,
                diagnostics,
                known_files,
            )?
        {
            files.push(file);
        }
        return Ok(());
    }
    if !info.is_dir() {
        return Ok(());
    }
    if HARD_SKIP_HIDDEN_NAMES.contains(&file_name_of(&root.absolute_path).as_str()) {
        return Ok(());
    }
    let mut base_rules = if root.no_ignore.unwrap_or(false) {
        Vec::new()
    } else {
        default_ignore_rules()
    };
    base_rules.extend(read_configured_ignore_rules(root)?);
    let mut visited = HashSet::new();
    visited.insert(real_path_of(&root.absolute_path));
    walk(
        workspace_index_id,
        root,
        &root.absolute_path,
        files,
        diagnostics,
        &base_rules,
        &selection,
        &mut visited,
        0,
        options.cancel.as_ref(),
        known_files,
    )
}
fn has_excluded_nested_git_ancestor(
    root: &RootPath,
    absolute_path: &str,
    kind: PathKind,
) -> EngineResult<bool> {
    let path_from_root = strip_root_prefix(&root.absolute_path, absolute_path);
    let segments: Vec<&str> = path_from_root
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    let directories = match kind {
        PathKind::Dir => segments.as_slice(),
        PathKind::File => segments
            .get(..segments.len().saturating_sub(1))
            .unwrap_or(&[]),
    };
    let mut current = PathBuf::from(&root.absolute_path);
    for segment in directories {
        current.push(segment);
        let current_display = to_display_path(&current);
        let relative_directory = display_relative(&root.absolute_path, &current_display);
        if nested_git_repository_blocked(root, &current_display, &relative_directory) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn is_nested_git_repository_directory(absolute_path: &str) -> bool {
    let marker = Path::new(absolute_path).join(".git");
    std::fs::symlink_metadata(&marker)
        .map(|meta| meta.is_file() || meta.is_dir())
        .unwrap_or(false)
}

fn nested_git_repository_explicitly_included(relative_path: &str, root: &RootPath) -> bool {
    if root.include.is_empty() {
        return false;
    }
    let normalized = normalize_path_pattern(relative_path);
    root.include.iter().any(|pattern| {
        let normalized_pattern = normalize_path_pattern(pattern);
        path_pattern_matches(&normalized_pattern, &normalized)
            || normalized_pattern.starts_with(&format!("{normalized}/"))
    })
}

/// Shared nested-git exclusion gate: a directory is blocked when the root
/// does not traverse nested repositories (`None`/`Some(false)`), the
/// directory is itself a git repository, and no root include pattern
/// explicitly covers it.
fn nested_git_repository_blocked(
    root: &RootPath,
    absolute_directory: &str,
    relative_directory: &str,
) -> bool {
    !root.traverses_nested_git()
        && is_nested_git_repository_directory(absolute_directory)
        && !nested_git_repository_explicitly_included(relative_directory, root)
}

fn dedupe_files(files: Vec<FileInfo>) -> Vec<FileInfo> {
    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(files.len());
    for file in files {
        if seen.insert(file.id.clone()) {
            out.push(file);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::Path;

    use super::{path_can_affect_index, scan_directory_path, scan_file_path, scan_root_paths};
    use crate::pipeline::indexing::scanner::types::{PathKind, ScanOptions};
    use crate::types::RootPath;

    fn write(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent");
        }
        std::fs::write(path, contents).expect("write fixture");
    }

    fn base_root(dir: &tempfile::TempDir, include_nested_git: Option<bool>) -> RootPath {
        RootPath {
            absolute_path: crate::paths::to_display_path(dir.path()),
            recursive: true,
            include: Vec::new(),
            exclude: Vec::new(),
            globs: Vec::new(),
            insensitive_globs: Vec::new(),
            file_types: Vec::new(),
            excluded_file_types: Vec::new(),
            hidden: None,
            no_ignore: None,
            ignore_files: Vec::new(),
            max_depth: None,
            max_file_size_bytes: None,
            follow: None,
            include_nested_git,
        }
    }

    fn nested_fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        write(&dir.path().join("top.txt"), "top\n");
        write(&dir.path().join("repo-a/.git/HEAD"), "ref\n");
        write(&dir.path().join("repo-a/a.txt"), "a\n");
        write(
            &dir.path().join("repo-a/deeper/.git"),
            "gitdir: elsewhere\n",
        );
        write(&dir.path().join("repo-a/deeper/deep.txt"), "deep\n");
        write(&dir.path().join("repo-b/.git"), "gitdir: elsewhere\n");
        write(&dir.path().join("repo-b/b.txt"), "b\n");
        dir
    }

    fn relative_set(dir: &tempfile::TempDir, absolute_paths: &[String]) -> BTreeSet<String> {
        let root = crate::paths::to_display_path(dir.path());
        absolute_paths
            .iter()
            .map(|absolute| {
                Path::new(absolute)
                    .strip_prefix(Path::new(&root))
                    .map(crate::paths::to_display_path)
                    .expect("under root")
            })
            .collect()
    }

    fn full_scan_relative(dir: &tempfile::TempDir, root: &RootPath) -> BTreeSet<String> {
        let result = scan_root_paths(
            "test-index",
            std::slice::from_ref(root),
            &ScanOptions::default(),
        )
        .expect("scan");
        relative_set(
            dir,
            &result
                .files
                .iter()
                .map(|file| file.absolute_path.clone())
                .collect::<Vec<_>>(),
        )
    }

    #[test]
    fn nested_repositories_excluded_by_default() {
        let dir = nested_fixture();
        for policy in [None, Some(false)] {
            let root = base_root(&dir, policy);
            assert_eq!(
                full_scan_relative(&dir, &root),
                BTreeSet::from(["top.txt".to_owned()])
            );
        }
    }

    #[test]
    fn nested_repositories_included_when_opted_in() {
        let dir = nested_fixture();
        let root = base_root(&dir, Some(true));
        assert_eq!(
            full_scan_relative(&dir, &root),
            BTreeSet::from([
                "top.txt".to_owned(),
                "repo-a/a.txt".to_owned(),
                "repo-a/deeper/deep.txt".to_owned(),
                "repo-b/b.txt".to_owned(),
            ])
        );

        let root_display = crate::paths::to_display_path(dir.path());
        let deep = format!("{root_display}/repo-a/deeper/deep.txt");
        let repo_a = format!("{root_display}/repo-a");
        let roots = vec![root.clone()];
        let file_scan = scan_file_path("test-index", &roots, &deep, &ScanOptions::default())
            .expect("file scan");
        assert_eq!(file_scan.files.len(), 1);
        let dir_scan = scan_directory_path("test-index", &roots, &repo_a, &ScanOptions::default())
            .expect("dir scan");
        assert_eq!(dir_scan.files.len(), 2);
        assert!(
            path_can_affect_index(&roots, &deep, PathKind::File).expect("affect file"),
            "enabled policy tracks nested file"
        );
        assert!(
            path_can_affect_index(&roots, &repo_a, PathKind::Dir).expect("affect dir"),
            "enabled policy tracks nested repository directory"
        );

        let disabled = vec![base_root(&dir, Some(false))];
        assert!(
            scan_file_path("test-index", &disabled, &deep, &ScanOptions::default())
                .expect("file scan")
                .files
                .is_empty()
        );
        assert!(
            scan_directory_path("test-index", &disabled, &repo_a, &ScanOptions::default())
                .expect("dir scan")
                .files
                .is_empty()
        );
        assert!(
            !path_can_affect_index(&disabled, &deep, PathKind::File).expect("affect file"),
            "disabled policy ignores nested file"
        );
        assert!(
            !path_can_affect_index(&disabled, &repo_a, PathKind::Dir).expect("affect dir"),
            "disabled policy ignores nested repository directory"
        );

        let persisted: RootPath =
            serde_json::from_value(serde_json::to_value(&root).expect("serialize"))
                .expect("deserialize");
        assert_eq!(persisted.include_nested_git, Some(true));
        let persisted_scan =
            scan_file_path("test-index", &[persisted], &deep, &ScanOptions::default())
                .expect("persisted scan");
        assert_eq!(persisted_scan.files.len(), 1);
    }

    #[test]
    fn inclusion_keeps_ignore_and_hidden_filters() {
        let dir = nested_fixture();
        write(&dir.path().join(".gitignore"), "blocked/\n");
        write(&dir.path().join("blocked/blocked.txt"), "blocked\n");
        write(&dir.path().join("repo-a/.gitignore"), "ignored.txt\n");
        write(&dir.path().join("repo-a/ignored.txt"), "ignored\n");
        write(&dir.path().join("repo-a/.secret.txt"), "secret\n");
        write(
            &dir.path().join("repo-a/.zvec-grep/internal.txt"),
            "internal\n",
        );
        let scanned = full_scan_relative(&dir, &base_root(&dir, Some(true)));
        assert!(scanned.contains("top.txt"));
        assert!(scanned.contains("repo-a/a.txt"));
        assert!(!scanned.contains("blocked/blocked.txt"), "{scanned:?}");
        assert!(!scanned.contains("repo-a/ignored.txt"), "{scanned:?}");
        assert!(!scanned.contains("repo-a/.secret.txt"), "{scanned:?}");
        assert!(
            !scanned.contains("repo-a/.zvec-grep/internal.txt"),
            "{scanned:?}"
        );
    }

    #[test]
    fn hard_skips_survive_hidden_and_no_ignore() {
        let dir = nested_fixture();
        write(&dir.path().join(".gitignore"), "blocked/\n");
        write(&dir.path().join("blocked/blocked.txt"), "blocked\n");
        write(&dir.path().join("repo-a/.secret.txt"), "secret\n");
        let mut root = base_root(&dir, Some(true));
        root.hidden = Some(true);
        root.no_ignore = Some(true);
        let scanned = full_scan_relative(&dir, &root);
        assert!(scanned.contains("repo-a/.secret.txt"), "{scanned:?}");
        assert!(scanned.contains("blocked/blocked.txt"), "{scanned:?}");
        assert!(
            !scanned.iter().any(|path| path.contains(".git/")),
            "{scanned:?}"
        );
        assert!(
            !scanned.iter().any(|path| path.ends_with("/.git")),
            "{scanned:?}"
        );
        assert!(
            !scanned.iter().any(|path| path.contains(".zvec-grep")),
            "{scanned:?}"
        );
    }

    #[test]
    fn max_depth_still_bounds_included_repositories() {
        let dir = nested_fixture();
        let mut root = base_root(&dir, Some(true));
        root.max_depth = Some(1);
        assert_eq!(
            full_scan_relative(&dir, &root),
            BTreeSet::from(["top.txt".to_owned()])
        );
    }

    #[test]
    fn file_selection_still_applies_inside_included_repositories() {
        let dir = nested_fixture();
        write(&dir.path().join("repo-a/code.rs"), "fn main() {}\n");
        let mut root = base_root(&dir, Some(true));
        root.globs = vec!["**/*.txt".to_owned()];
        let scanned = full_scan_relative(&dir, &root);
        assert!(scanned.contains("repo-a/a.txt"), "{scanned:?}");
        assert!(!scanned.contains("repo-a/code.rs"), "{scanned:?}");
    }

    #[test]
    fn explicit_include_escape_still_works_when_disabled() {
        let dir = nested_fixture();
        let mut root = base_root(&dir, Some(false));
        root.include = vec!["repo-b/**".to_owned()];
        assert_eq!(
            full_scan_relative(&dir, &root),
            BTreeSet::from(["repo-b/b.txt".to_owned()])
        );
    }
}
