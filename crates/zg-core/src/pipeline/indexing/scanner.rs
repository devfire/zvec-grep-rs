//! Workspace file scanner: directory walk, ignore rules, file typing.
//!
//! Port of `engine/pipeline/indexing/scanner/index.ts`. Synchronous
//! (`std::fs`) where the TS original is async; cancellation flows through an
//! optional atomic flag instead of `AbortSignal`.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use crate::error::{EngineError, EngineErrorCode, EngineResult};
use crate::file_size_policy::resolve_max_file_size_bytes;
use crate::file_type::detect_file_type;
use crate::paths::{is_path_inside, normalize_path, to_display_path};
use crate::types::{
    FileInfo, FileScanDiagnostics, RootPath, SkippedFile, SkippedFileReason, UnixMillis,
};
use crate::utils::file_selection::{FileSelection, OrderedGlobs, resolve_file_types};
use crate::utils::glob::{
    normalize_path_pattern, path_pattern_matches, path_pattern_might_match_descendant,
};
use crate::utils::hash::sha256_text;

use super::root_paths::{
    matches_root_exclude_patterns, matches_root_patterns, normalize_root_path, validate_root_paths,
};

const BINARY_SNIFF_BYTES: usize = 8192;
const BINARY_CONTROL_CHAR_RATIO: f64 = 0.3;
const MAX_GITIGNORE_CACHE_ENTRIES: usize = 4_096;
const MAX_SKIPPED_FILE_SAMPLES: usize = 20;

const DEFAULT_IGNORED_DIRECTORY_NAMES: &[&str] = &[
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

const DEFAULT_IGNORED_FILE_PATTERNS: &[&str] = &[
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

const HARD_SKIP_HIDDEN_NAMES: &[&str] = &[".git", ".zvec-grep"];

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
#[derive(Debug, Clone, Default)]
pub struct CancelFlag(pub std::sync::Arc<AtomicBool>);

impl CancelFlag {
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

fn throw_if_cancelled(cancel: Option<&CancelFlag>) -> EngineResult<()> {
    if cancel.is_some_and(CancelFlag::is_cancelled) {
        return Err(EngineError::new(
            EngineErrorCode::new("INDEXING.CANCELLED"),
            "indexing was cancelled",
        ));
    }
    Ok(())
}

#[derive(Debug, Clone)]
struct IgnoreRule {
    base_path: String,
    pattern: String,
    negated: bool,
    directory_only: bool,
    anchored: bool,
    has_slash: bool,
}

#[derive(Debug, Clone, Copy)]
struct IgnoreMatch<'a> {
    ignored: bool,
    matched_rule: Option<&'a IgnoreRule>,
}

struct GitIgnoreCache {
    entries: HashMap<String, (String, Vec<IgnoreRule>)>,
    order: VecDeque<String>,
}

static GITIGNORE_CACHE: OnceLock<Mutex<GitIgnoreCache>> = OnceLock::new();

fn gitignore_cache() -> &'static Mutex<GitIgnoreCache> {
    GITIGNORE_CACHE.get_or_init(|| {
        Mutex::new(GitIgnoreCache {
            entries: HashMap::new(),
            order: VecDeque::new(),
        })
    })
}

fn default_ignore_rules() -> Vec<IgnoreRule> {
    let mut rules = Vec::with_capacity(
        DEFAULT_IGNORED_DIRECTORY_NAMES.len() + DEFAULT_IGNORED_FILE_PATTERNS.len(),
    );
    for name in DEFAULT_IGNORED_DIRECTORY_NAMES {
        rules.push(IgnoreRule {
            base_path: String::new(),
            pattern: (*name).to_owned(),
            negated: false,
            directory_only: true,
            anchored: false,
            has_slash: false,
        });
    }
    for pattern in DEFAULT_IGNORED_FILE_PATTERNS {
        rules.push(IgnoreRule {
            base_path: String::new(),
            pattern: (*pattern).to_owned(),
            negated: false,
            directory_only: false,
            anchored: false,
            has_slash: pattern.contains('/'),
        });
    }
    rules
}

/// Scans every configured root (mirrors `scanRootPaths`).
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
        let target = std::fs::symlink_metadata(absolute_path).ok();
        let followed = match (&target, root.follow.unwrap_or(false)) {
            (Some(meta), true) if meta.file_type().is_symlink() => {
                std::fs::metadata(absolute_path).ok()
            }
            _ => target,
        };
        let Some(meta) = followed else { continue };
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
        let target = std::fs::symlink_metadata(absolute_path).ok();
        let followed = match (&target, root.follow.unwrap_or(false)) {
            (Some(meta), true) if meta.file_type().is_symlink() => {
                std::fs::metadata(absolute_path).ok()
            }
            _ => target,
        };
        let Some(meta) = followed else { continue };
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

fn known_files_by_path(files: &[FileInfo]) -> HashMap<String, &FileInfo> {
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

fn matching_root_paths(root_paths: &[RootPath], absolute_path: &str) -> Vec<RootPath> {
    let Ok(validated) = validate_root_paths(root_paths) else {
        return Vec::new();
    };
    validated
        .into_iter()
        .filter(|root| !path_outside_root(&normalize_root_path(root).absolute_path, absolute_path))
        .collect()
}

fn path_outside_root(root: &str, path: &str) -> bool {
    if path == root {
        return false;
    }
    !is_path_inside(Path::new(root), Path::new(path))
}

fn strip_root_prefix(root: &str, path: &str) -> String {
    if path == root {
        return String::new();
    }
    display_relative(root, path)
}

fn display_relative(root: &str, path: &str) -> String {
    match Path::new(path).strip_prefix(Path::new(root)) {
        Ok(relative) => to_display_path(relative),
        Err(_) => to_display_path(Path::new(path)),
    }
}

fn parent_display(path: &str) -> String {
    Path::new(path)
        .parent()
        .map(to_display_path)
        .unwrap_or_default()
}

fn file_name_of(path: &str) -> String {
    Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_owned()
}

fn real_path_of(path: &str) -> String {
    std::fs::canonicalize(path)
        .map(|real| to_display_path(&real))
        .unwrap_or_else(|_| path.to_owned())
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
        if selection.matches(&relative_path) {
            if let Some(file) = read_file_info(
                workspace_index_id,
                root,
                &root.absolute_path,
                diagnostics,
                known_files,
            )? {
                files.push(file);
            }
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

fn ignore_rules_for_directory(root: &RootPath, directory: &str) -> EngineResult<Vec<IgnoreRule>> {
    if path_outside_root(&root.absolute_path, directory) && directory != root.absolute_path {
        return Ok(default_ignore_rules());
    }
    let mut rules = if root.no_ignore.unwrap_or(false) {
        Vec::new()
    } else {
        default_ignore_rules()
    };
    rules.extend(read_configured_ignore_rules(root)?);
    if !root.no_ignore.unwrap_or(false) {
        rules.extend(read_gitignore_rules(root, &root.absolute_path)?);
    }
    let path_from_root = strip_root_prefix(&root.absolute_path, directory);
    if path_from_root.is_empty() {
        return Ok(rules);
    }
    let mut current = PathBuf::from(&root.absolute_path);
    for segment in path_from_root.split('/').filter(|s| !s.is_empty()) {
        current.push(segment);
        if !root.no_ignore.unwrap_or(false) {
            rules.extend(read_gitignore_rules(root, &to_display_path(&current))?);
        }
    }
    Ok(rules)
}

fn path_can_be_scanned(
    root: &RootPath,
    relative_path: &str,
    name: &str,
    is_directory: bool,
    ignore_rules: &[IgnoreRule],
) -> bool {
    if relative_path
        .split('/')
        .any(|segment| HARD_SKIP_HIDDEN_NAMES.contains(&segment))
    {
        return false;
    }
    let ignore_match = match_ignore_rules(relative_path, is_directory, ignore_rules);
    if ignore_match.ignored && !ignored_path_explicitly_included(relative_path, root, ignore_match)
    {
        return false;
    }
    if matches_root_exclude_patterns(relative_path, root) {
        return false;
    }
    if is_directory {
        return !should_skip_hidden_directory(name, relative_path, root);
    }
    !should_skip_hidden_file(name, relative_path, root)
        && matches_root_patterns(relative_path, root)
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

fn read_gitignore_rules(root: &RootPath, current_path: &str) -> EngineResult<Vec<IgnoreRule>> {
    let ignore_path = Path::new(current_path).join(".gitignore");
    let content = match std::fs::read_to_string(&ignore_path) {
        Ok(content) => content,
        Err(_) => {
            if let Ok(mut cache) = gitignore_cache().lock() {
                cache.entries.remove(&to_display_path(&ignore_path));
            }
            return Ok(Vec::new());
        }
    };
    let base_path = display_relative(&root.absolute_path, current_path);
    let cache_key = format!("{}\0{base_path}", to_display_path(&ignore_path));
    if let Ok(cache) = gitignore_cache().lock() {
        if let Some((cached_content, rules)) = cache.entries.get(&cache_key) {
            if cached_content == &content {
                return Ok(rules.clone());
            }
        }
    }
    let rules = parse_gitignore_rules(&content, &base_path);
    if let Ok(mut cache) = gitignore_cache().lock() {
        if cache.entries.len() > MAX_GITIGNORE_CACHE_ENTRIES {
            if let Some(oldest) = cache.order.pop_front() {
                cache.entries.remove(&oldest);
            }
        }
        cache.order.push_back(cache_key.clone());
        cache.entries.insert(cache_key, (content, rules.clone()));
    }
    Ok(rules)
}

fn read_configured_ignore_rules(root: &RootPath) -> EngineResult<Vec<IgnoreRule>> {
    let mut rules = Vec::new();
    for path in &root.ignore_files {
        let absolute = if Path::new(path).is_absolute() {
            PathBuf::from(path)
        } else {
            Path::new(&root.absolute_path).join(path)
        };
        let content = std::fs::read_to_string(&absolute).map_err(|err| {
            EngineError::new(
                EngineErrorCode::new("SCANNER.CONFIGURED_IGNORE_READ_FAILED"),
                "workspace index ignore file could not be read",
            )
            .with_context(format!("path={} detail={err}", absolute.display()))
        })?;
        rules.extend(parse_gitignore_rules(&content, ""));
    }
    Ok(rules)
}

fn parse_gitignore_rules(content: &str, base_path: &str) -> Vec<IgnoreRule> {
    content
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .filter_map(|line| parse_gitignore_rule(line, base_path))
        .collect()
}

fn parse_gitignore_rule(line: &str, base_path: &str) -> Option<IgnoreRule> {
    let mut pattern = line.trim().to_owned();
    if pattern.is_empty() || pattern.starts_with('#') {
        return None;
    }
    let mut negated = false;
    if pattern.starts_with("\\#") || pattern.starts_with("\\!") {
        pattern = pattern[1..].to_owned();
    } else if let Some(rest) = pattern.strip_prefix('!') {
        negated = true;
        pattern = rest.trim().to_owned();
    }
    if pattern.is_empty() {
        return None;
    }
    let directory_only = pattern.ends_with('/');
    if directory_only {
        pattern = pattern.trim_end_matches('/').to_owned();
    }
    let anchored = pattern.starts_with('/');
    if anchored {
        pattern = pattern.trim_start_matches('/').to_owned();
    }
    let pattern = normalize_path_pattern(&pattern);
    if pattern.is_empty() {
        return None;
    }
    let has_slash = pattern.contains('/');
    Some(IgnoreRule {
        base_path: base_path.to_owned(),
        pattern,
        negated,
        directory_only,
        anchored,
        has_slash,
    })
}

fn match_ignore_rules<'a>(
    relative_path: &str,
    is_directory: bool,
    rules: &'a [IgnoreRule],
) -> IgnoreMatch<'a> {
    let mut ignored = false;
    let mut matched_rule = None;
    for rule in rules {
        if !ignore_rule_matches(rule, relative_path, is_directory) {
            continue;
        }
        ignored = !rule.negated;
        matched_rule = Some(rule);
    }
    IgnoreMatch {
        ignored,
        matched_rule,
    }
}

fn ignored_path_explicitly_included(
    relative_path: &str,
    root: &RootPath,
    ignore_match: IgnoreMatch<'_>,
) -> bool {
    let Some(rule) = ignore_match.matched_rule else {
        return false;
    };
    if !ignore_match.ignored || root.include.is_empty() {
        return false;
    }
    root.include
        .iter()
        .any(|pattern| include_pattern_names_ignored_path(pattern, relative_path, &rule.pattern))
}

fn include_pattern_names_ignored_path(
    include_pattern: &str,
    relative_path: &str,
    ignored_pattern: &str,
) -> bool {
    let normalized_include = normalize_path_pattern(include_pattern);
    let normalized_relative = normalize_path_pattern(relative_path);
    let ignored_segments: Vec<&str> = ignored_pattern.split('/').collect();
    if !path_pattern_might_match_descendant(&normalized_include, &normalized_relative)
        && !path_pattern_matches(&normalized_include, &normalized_relative)
    {
        return false;
    }
    normalized_include.split('/').any(|segment| {
        ignored_segments
            .iter()
            .any(|ignored| include_segment_names_ignored_path(segment, ignored))
    })
}

fn include_segment_names_ignored_path(segment: &str, ignored_segment: &str) -> bool {
    if matches!(segment, "*" | "**" | "?") {
        return false;
    }
    segment_matches(segment, ignored_segment) || segment_matches(ignored_segment, segment)
}

fn ignore_rule_matches(rule: &IgnoreRule, relative_path: &str, is_directory: bool) -> bool {
    let path = match relative_to_ignore_rule_base(relative_path, &rule.base_path) {
        Some(path) if !path.is_empty() => path,
        _ => return false,
    };
    if rule.directory_only {
        if rule.anchored || rule.has_slash {
            return path_pattern_matches(&rule.pattern, &path);
        }
        return path_contains_matching_segment(&path, &rule.pattern);
    }
    if rule.anchored || rule.has_slash {
        return path_pattern_matches(&rule.pattern, &path);
    }
    if is_directory && path_contains_matching_segment(&path, &rule.pattern) {
        return true;
    }
    segment_matches(&rule.pattern, &file_name_of(&path))
}

fn relative_to_ignore_rule_base(relative_path: &str, base_path: &str) -> Option<String> {
    if base_path.is_empty() {
        return Some(relative_path.to_owned());
    }
    if relative_path == base_path {
        return Some(String::new());
    }
    relative_path
        .strip_prefix(&format!("{base_path}/"))
        .map(str::to_owned)
}

fn path_contains_matching_segment(path: &str, pattern: &str) -> bool {
    path.split('/')
        .any(|segment| segment_matches(pattern, segment))
}

fn segment_matches(pattern: &str, segment: &str) -> bool {
    path_pattern_matches(pattern, segment)
}

fn is_hidden_name(name: &str) -> bool {
    name.starts_with('.') && name != "." && name != ".."
}

fn should_skip_hidden_directory(name: &str, relative_path: &str, root: &RootPath) -> bool {
    if !is_hidden_name(name) || root.hidden.unwrap_or(false) {
        return false;
    }
    !has_include_descendant(relative_path, &root.include)
}

fn should_skip_hidden_file(name: &str, relative_path: &str, root: &RootPath) -> bool {
    is_hidden_name(name)
        && !root.hidden.unwrap_or(false)
        && !has_explicit_hidden_file_include(relative_path, &root.include)
}

fn has_include_descendant(relative_path: &str, include: &[String]) -> bool {
    include.iter().any(|pattern| {
        include_pattern_declares_hidden_directory(pattern, relative_path)
            && path_pattern_might_match_descendant(pattern, relative_path)
    })
}

fn has_explicit_hidden_file_include(relative_path: &str, include: &[String]) -> bool {
    include.iter().any(|pattern| {
        include_pattern_declares_hidden_directory(pattern, relative_path)
            && path_pattern_matches(pattern, relative_path)
    })
}

fn include_pattern_declares_hidden_directory(pattern: &str, relative_path: &str) -> bool {
    let name = file_name_of(relative_path);
    if !is_hidden_name(&name) {
        return true;
    }
    normalize_path_pattern(pattern)
        .split('/')
        .any(|segment| hidden_pattern_segment_matches(segment, &name))
}

fn hidden_pattern_segment_matches(pattern_segment: &str, name: &str) -> bool {
    if !pattern_segment.starts_with('.') {
        return false;
    }
    if !pattern_segment.contains(['*', '?']) {
        return pattern_segment == name;
    }
    segment_glob_matches(pattern_segment, name)
}

/// Single-segment glob (`*` any run, `?` one char, no `/` crossing).
fn segment_glob_matches(pattern: &str, name: &str) -> bool {
    fn go(p: &[u8], n: &[u8]) -> bool {
        if p.is_empty() {
            return n.is_empty();
        }
        match p[0] {
            b'*' => {
                let mut rest = &p[1..];
                while rest.first() == Some(&b'*') {
                    rest = &rest[1..];
                }
                for split in 0..=n.len() {
                    if go(rest, &n[split..]) {
                        return true;
                    }
                }
                false
            }
            b'?' => !n.is_empty() && go(&p[1..], &n[1..]),
            b'\\' if p.len() > 1 => n.first() == Some(&p[1]) && go(&p[2..], &n[1..]),
            c => n.first() == Some(&c) && go(&p[1..], &n[1..]),
        }
    }
    go(pattern.as_bytes(), name.as_bytes())
}

fn read_file_info(
    workspace_index_id: &str,
    root: &RootPath,
    absolute_path: &str,
    diagnostics: &mut FileScanDiagnostics,
    known_files: &HashMap<String, &FileInfo>,
) -> EngineResult<Option<FileInfo>> {
    let Ok(info) = std::fs::metadata(absolute_path) else {
        return Ok(None);
    };
    if !info.is_file() {
        return Ok(None);
    }
    let relative_path = {
        let display = display_relative(&root.absolute_path, absolute_path);
        if display.is_empty() {
            file_name_of(absolute_path)
        } else {
            display
        }
    };
    if info.len() == 0 {
        record_skipped_file(
            diagnostics,
            absolute_path,
            &relative_path,
            SkippedFileReason::Empty,
            Some(0),
            None,
        );
        return Ok(None);
    }
    let Some(detected) = detect_file_type(Path::new(absolute_path)) else {
        record_skipped_file(
            diagnostics,
            absolute_path,
            &relative_path,
            SkippedFileReason::Unsupported,
            Some(info.len()),
            None,
        );
        return Ok(None);
    };
    let max_file_size = resolve_max_file_size_bytes(detected.kind, root.max_file_size_bytes);
    if info.len() > max_file_size {
        record_skipped_file(
            diagnostics,
            absolute_path,
            &relative_path,
            SkippedFileReason::TooLarge,
            Some(info.len()),
            Some(max_file_size),
        );
        return Ok(None);
    }
    let last_modified_time = info
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0);
    let known = known_files.get(&to_display_path(&normalize_path(Path::new(absolute_path))));
    if known.is_some_and(|known| {
        known.content_hash.is_some()
            && known.size_bytes == info.len()
            && known.last_modified_time.0 == last_modified_time
    }) {
        return Ok(Some(FileInfo {
            id: make_file_id(workspace_index_id, absolute_path),
            absolute_path: absolute_path.to_owned(),
            relative_path,
            root_path: root.absolute_path.clone(),
            size_bytes: info.len(),
            last_modified_time: UnixMillis(last_modified_time),
            content_hash: None,
            kind: detected.kind,
            format: detected.format,
            index_status: None,
        }));
    }
    if detected.kind != crate::types::FileKind::Image && is_likely_binary_file(absolute_path) {
        record_skipped_file(
            diagnostics,
            absolute_path,
            &relative_path,
            SkippedFileReason::Binary,
            Some(info.len()),
            None,
        );
        return Ok(None);
    }
    Ok(Some(FileInfo {
        id: make_file_id(workspace_index_id, absolute_path),
        absolute_path: absolute_path.to_owned(),
        relative_path,
        root_path: root.absolute_path.clone(),
        size_bytes: info.len(),
        last_modified_time: UnixMillis(last_modified_time),
        content_hash: None,
        kind: detected.kind,
        format: detected.format,
        index_status: None,
    }))
}

fn record_skipped_file(
    diagnostics: &mut FileScanDiagnostics,
    absolute_path: &str,
    relative_path: &str,
    reason: SkippedFileReason,
    size_bytes: Option<u64>,
    limit_bytes: Option<u64>,
) {
    diagnostics.skipped_files += 1;
    *diagnostics.skipped_by_reason.entry(reason).or_insert(0) += 1;
    if diagnostics.skipped_samples.len() < MAX_SKIPPED_FILE_SAMPLES {
        diagnostics.skipped_samples.push(SkippedFile {
            absolute_path: absolute_path.to_owned(),
            relative_path: relative_path.to_owned(),
            reason,
            size_bytes,
            limit_bytes,
        });
    }
}

/// Empty diagnostics accumulator (mirrors `createScanDiagnostics`).
pub fn create_scan_diagnostics() -> FileScanDiagnostics {
    FileScanDiagnostics {
        skipped_files: 0,
        skipped_by_reason: Default::default(),
        skipped_samples: Vec::new(),
    }
}

fn is_likely_binary_file(path: &str) -> bool {
    use std::io::Read as _;
    let Ok(mut handle) = std::fs::File::open(path) else {
        return false;
    };
    let mut buffer = vec![0u8; BINARY_SNIFF_BYTES];
    let Ok(bytes_read) = handle.read(&mut buffer) else {
        return false;
    };
    if bytes_read == 0 {
        return false;
    }
    let mut suspicious = 0usize;
    for value in &buffer[..bytes_read] {
        if *value == 0 {
            return true;
        }
        if is_suspicious_control_byte(*value) {
            suspicious += 1;
        }
    }
    suspicious as f64 / bytes_read as f64 > BINARY_CONTROL_CHAR_RATIO
}

fn is_suspicious_control_byte(value: u8) -> bool {
    value < 32 && !matches!(value, 7 | 8 | 9 | 10 | 12 | 13 | 27)
}

fn make_file_id(workspace_index_id: &str, absolute_path: &str) -> crate::ids::FileId {
    crate::ids::FileId::from_raw(sha256_text(&format!(
        "{workspace_index_id}\0{}",
        to_display_path(&normalize_path(Path::new(absolute_path)))
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn write_tree(dir: &Path, files: &[(&str, &str)]) {
        for (name, content) in files {
            let path = dir.join(name);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("mkdir");
            }
            let mut file = std::fs::File::create(&path).expect("create");
            file.write_all(content.as_bytes()).expect("write");
        }
    }

    fn test_root(dir: &Path) -> RootPath {
        RootPath {
            absolute_path: to_display_path(dir),
            recursive: true,
            ..RootPath::default()
        }
    }

    #[test]
    fn scans_code_and_skips_target_dir() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_tree(
            dir.path(),
            &[
                ("src/main.rs", "fn main() {}\n"),
                ("target/debug/blob", "binary-ish content here\n"),
            ],
        );
        let options = ScanOptions::default();
        let result = scan_root_paths("idx", &[test_root(dir.path())], &options).expect("scan");
        let names: Vec<_> = result
            .files
            .iter()
            .map(|f| f.relative_path.clone())
            .collect();
        assert!(names.iter().any(|n| n == "src/main.rs"), "{names:?}");
        assert!(!names.iter().any(|n| n.starts_with("target/")), "{names:?}");
    }

    #[test]
    fn empty_files_are_skipped_with_reason() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_tree(dir.path(), &[("empty.rs", ""), ("ok.rs", "fn f() {}\n")]);
        let options = ScanOptions::default();
        let result = scan_root_paths("idx", &[test_root(dir.path())], &options).expect("scan");
        assert_eq!(result.files.len(), 1);
        assert_eq!(result.diagnostics.skipped_files, 1);
        assert_eq!(
            result
                .diagnostics
                .skipped_by_reason
                .get(&SkippedFileReason::Empty),
            Some(&1)
        );
    }
}
