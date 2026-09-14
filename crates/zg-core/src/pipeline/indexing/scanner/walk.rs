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
use super::types::{CancelFlag, ScanOptions, ScanResult, throw_if_cancelled};
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
            || has_excluded_nested_git_ancestor(&root, absolute_path, false)?
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
/// `pathCanAffectIndex`).
///
/// # Errors
///
/// Returns an error when ignore rules or file selection fail.
pub fn path_can_affect_index(
    root_paths: &[RootPath],
    absolute_path: &str,
    is_directory: bool,
) -> EngineResult<bool> {
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
        ) || has_excluded_nested_git_ancestor(&root, absolute_path, is_directory)?
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
            ) || has_excluded_nested_git_ancestor(&root, absolute_path, true)?)
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
            if is_nested_git_repository_directory(&absolute_path)
                && !nested_git_repository_explicitly_included(&relative_path, root)
            {
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
    include_target: bool,
) -> EngineResult<bool> {
    let path_from_root = strip_root_prefix(&root.absolute_path, absolute_path);
    let segments: Vec<&str> = path_from_root
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    let directories = if include_target {
        segments.as_slice()
    } else {
        segments
            .get(..segments.len().saturating_sub(1))
            .unwrap_or(&[])
    };
    let mut current = PathBuf::from(&root.absolute_path);
    for segment in directories {
        current.push(segment);
        let current_display = to_display_path(&current);
        let relative_directory = display_relative(&root.absolute_path, &current_display);
        if is_nested_git_repository_directory(&current_display)
            && !nested_git_repository_explicitly_included(&relative_directory, root)
        {
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
