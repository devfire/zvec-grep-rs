//! Gitignore-style rule loading, parsing, and matching.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use super::types::{DEFAULT_IGNORED_DIRECTORY_NAMES, DEFAULT_IGNORED_FILE_PATTERNS};
use super::types::{MAX_GITIGNORE_CACHE_ENTRIES, display_relative, file_name_of};
use super::types::{path_outside_root, strip_root_prefix};
use crate::error::{EngineError, EngineErrorCode, EngineResult};
use crate::paths::to_display_path;
use crate::types::RootPath;
use crate::utils::glob::{
    normalize_path_pattern, path_pattern_matches, path_pattern_might_match_descendant,
};

#[derive(Debug, Clone)]
pub(crate) struct IgnoreRule {
    pub(crate) base_path: String,
    pub(crate) pattern: String,
    pub(crate) negated: bool,
    pub(crate) directory_only: bool,
    pub(crate) anchored: bool,
    pub(crate) has_slash: bool,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct IgnoreMatch<'a> {
    pub(crate) ignored: bool,
    pub(crate) matched_rule: Option<&'a IgnoreRule>,
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

pub(crate) fn default_ignore_rules() -> Vec<IgnoreRule> {
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

pub(crate) fn ignore_rules_for_directory(
    root: &RootPath,
    directory: &str,
) -> EngineResult<Vec<IgnoreRule>> {
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

pub(crate) fn read_gitignore_rules(
    root: &RootPath,
    current_path: &str,
) -> EngineResult<Vec<IgnoreRule>> {
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
    if let Ok(cache) = gitignore_cache().lock()
        && let Some((cached_content, rules)) = cache.entries.get(&cache_key)
        && cached_content == &content
    {
        return Ok(rules.clone());
    }
    let rules = parse_gitignore_rules(&content, &base_path);
    if let Ok(mut cache) = gitignore_cache().lock()
        && cache.entries.len() > MAX_GITIGNORE_CACHE_ENTRIES
    {
        if let Some(oldest) = cache.order.pop_front() {
            cache.entries.remove(&oldest);
        }
        cache.order.push_back(cache_key.clone());
        cache.entries.insert(cache_key, (content, rules.clone()));
    }
    Ok(rules)
}

pub(crate) fn read_configured_ignore_rules(root: &RootPath) -> EngineResult<Vec<IgnoreRule>> {
    let mut rules = Vec::new();
    for path in &root.ignore_files {
        let absolute = if Path::new(path).is_absolute() {
            PathBuf::from(path)
        } else {
            Path::new(&root.absolute_path).join(path)
        };
        let content = std::fs::read_to_string(&absolute).map_err(|err| {
            EngineError::new(
                EngineErrorCode::ScannerConfiguredIgnoreReadFailed,
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

pub(crate) fn match_ignore_rules<'a>(
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

pub(crate) fn ignored_path_explicitly_included(
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

#[cfg(test)]
mod tests {
    use super::super::types::ScanOptions;
    use super::super::walk::scan_root_paths;
    use crate::paths::to_display_path;
    use crate::types::RootPath;
    use std::io::Write as _;
    use std::path::Path;

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
}
