//! Exhaustive in-process lexical search driver.
//!
//! Never spawns a subprocess: file discovery runs through the `ignore`
//! crate, matching through `grep-regex` + `grep-searcher`. Per-file IO
//! failures skip that file, mirroring ripgrep's tolerance.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::time::UNIX_EPOCH;

use grep_searcher::SearcherBuilder;

use crate::error::EngineResult;

use super::filter::{
    GlobFilter, HARD_IGNORED_DIRECTORIES, build_ignore_matcher, expand_path_patterns,
    is_hard_ignored_name, is_hard_ignored_path, is_searchable_file, matches_any_path_pattern,
    matches_modified_time,
};
use super::options::{
    LexicalBackend, LexicalDiagnostics, LexicalSearchOptions, LexicalSearchResult,
};
use super::patterns::{build_matcher, load_patterns};
use super::search_paths::{
    absolute_normalized, check_search_paths, display_paths, resolve_search_path,
};
use super::sink::{MatchSink, record_file};

/// Runs an exhaustive in-process lexical search.
///
/// Never spawns a subprocess. Returns an [`crate::error::EngineError`] only for invalid
/// search setup (no pattern, unreadable pattern/ignore file, unknown file
/// type, unusable matcher); per-file IO failures during the walk skip that
/// file, mirroring ripgrep's tolerance.
///
/// # Errors
///
/// Returns `LEXICAL.EMPTY_PATTERN` when no pattern is given, `LEXICAL.UNKNOWN_FILE_TYPE`
/// for unknown file types, `LEXICAL.PATTERN_FILE_UNREADABLE` or `LEXICAL.INVALID_PATTERN`
/// for bad patterns, `LEXICAL.IGNORE_FILE_INVALID` for bad ignore files, or
/// `LEXICAL.SEARCH_FAILED` when the walk itself fails.
pub fn run_lexical_search(options: &LexicalSearchOptions) -> EngineResult<LexicalSearchResult> {
    use crate::error::{EngineError, EngineErrorCode};

    let checked = check_search_paths(&options.root, &options.paths);
    let mut diagnostics = LexicalDiagnostics {
        backend: LexicalBackend::InProcess,
        command: "in-process".to_owned(),
        patterns: Vec::new(),
        ignored_directories: HARD_IGNORED_DIRECTORIES
            .iter()
            .map(|dir| (*dir).to_owned())
            .collect(),
        missing_paths: None,
        searched_paths: None,
        limit: options.limit,
        truncated: false,
    };

    // All requested paths missing: report diagnostics without searching,
    // mirroring the TS early return.
    if !options.paths.is_empty() && checked.existing.is_empty() {
        diagnostics.missing_paths = Some(checked.missing);
        diagnostics.searched_paths = Some(Vec::new());
        return Ok(LexicalSearchResult {
            items: Vec::new(),
            diagnostics,
        });
    }
    if !checked.missing.is_empty() {
        diagnostics.missing_paths = Some(checked.missing);
    }
    diagnostics.searched_paths = Some(checked.searched.clone());

    let all_patterns = load_patterns(&options.patterns, &options.pattern_files)?;
    if all_patterns.is_empty() {
        return Err(EngineError::new(
            EngineErrorCode::from_static("LEXICAL.EMPTY_PATTERN"),
            "at least one search pattern is required",
        ));
    }
    diagnostics.patterns = all_patterns.clone();

    let matcher = build_matcher(options, &all_patterns)?;
    let type_matcher = crate::utils::file_selection::resolve_file_types(
        &options.file_types,
        &options.excluded_file_types,
    )
    .map_err(|error| {
        EngineError::new(
            EngineErrorCode::from_static("LEXICAL.UNKNOWN_FILE_TYPE"),
            error.message().to_owned(),
        )
    })?;
    let glob_filter = GlobFilter::new(&options.globs, &options.insensitive_globs);
    let include_filters = expand_path_patterns(&options.include_paths);
    let exclude_filters = expand_path_patterns(&options.exclude_paths);
    let ignore_matcher = build_ignore_matcher(&options.root, &options.ignore_files)?;

    let root = absolute_normalized(&options.root);
    let searched_roots: Vec<PathBuf> = if checked.existing.is_empty() {
        vec![root.clone()]
    } else {
        checked
            .existing
            .iter()
            .map(|path| resolve_search_path(&options.root, path))
            .collect()
    };

    let Some(first) = searched_roots.first().cloned() else {
        return Ok(LexicalSearchResult {
            items: Vec::new(),
            diagnostics,
        });
    };
    let mut walk = ignore::WalkBuilder::new(&first);
    for extra in searched_roots.iter().skip(1) {
        walk.add(extra);
    }
    walk.hidden(!options.searches_hidden())
        .git_ignore(!options.no_ignore)
        .git_global(!options.no_ignore)
        .git_exclude(!options.no_ignore)
        .ignore(!options.no_ignore)
        .parents(!options.no_ignore)
        .max_depth(options.max_depth)
        .follow_links(options.follow)
        .filter_entry(|entry| {
            if entry.depth() == 0 {
                return true;
            }
            if entry.file_type().is_some_and(|kind| kind.is_dir()) {
                return !is_hard_ignored_name(entry.file_name());
            }
            true
        });

    let mut searcher = SearcherBuilder::new().line_number(true).build();
    let mut sink = MatchSink {
        items: Vec::new(),
        next_rank: 1,
        limit: options.limit,
        truncated: false,
        root: &root,
        options,
        matcher: &matcher,
        line_cache: HashMap::new(),
        per_file_counts: HashMap::new(),
    };

    for entry in walk.build() {
        let Ok(entry) = entry else { continue };
        if sink.truncated {
            break;
        }
        let path = entry.path();
        if !is_searchable_file(entry.file_type(), path, options.follow) {
            continue;
        }
        let metadata = entry.metadata().ok();
        let owned_metadata = match metadata {
            Some(metadata) => Some(metadata),
            None => fs::metadata(path).ok(),
        };
        let Some(file_len) = owned_metadata.as_ref().map(|m| m.len()) else {
            continue;
        };
        if options
            .max_file_size_bytes
            .is_some_and(|cap| file_len > cap)
        {
            continue;
        }
        if is_hard_ignored_path(&root, path) {
            continue;
        }
        let (relative, absolute) = display_paths(&root, path);
        if !options.include_paths.is_empty()
            && !matches_any_path_pattern(&include_filters, &relative, &absolute)
        {
            continue;
        }
        if matches_any_path_pattern(&exclude_filters, &relative, &absolute) {
            continue;
        }
        if !glob_filter.matches(&relative, &absolute) {
            continue;
        }
        if !type_matcher.matches(path) {
            continue;
        }
        if let Some(ignore) = ignore_matcher.as_ref()
            && ignore.matched(path, false).is_ignore()
        {
            continue;
        }
        let mtime = owned_metadata
            .as_ref()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64);
        if !matches_modified_time(mtime, options.modified_after, options.modified_before) {
            continue;
        }
        // Per-file IO failures skip the file, like ripgrep's tolerance for
        // unreadable files.
        record_file(path);
        let _ = searcher.search_path(&matcher, path, &mut sink);
    }

    if let Some(limit) = options.limit {
        sink.items.truncate(limit);
    }
    diagnostics.truncated = sink.truncated;
    Ok(LexicalSearchResult {
        items: sink.items,
        diagnostics,
    })
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use std::path::Path;

    use super::*;
    use std::io::Write as _;

    fn write_file(dir: &Path, name: &str, content: &str) -> PathBuf {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        let mut file = fs::File::create(&path).expect("create file");
        file.write_all(content.as_bytes()).expect("write file");
        path
    }

    fn options_in(root: &Path) -> LexicalSearchOptions {
        LexicalSearchOptions {
            root: root.to_owned(),
            ..LexicalSearchOptions::default()
        }
    }

    #[test]
    fn finds_literal_and_reports_diagnostics() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_file(dir.path(), "a.txt", "hello world\nsecond line\n");
        write_file(dir.path(), "sub/b.txt", "nothing here\nhello again\n");

        let mut options = options_in(dir.path());
        options.patterns = vec!["hello".to_owned()];
        let result = run_lexical_search(&options).expect("search");
        assert_eq!(result.items.len(), 2);
        assert_eq!(result.items[0].rank, 1);
        assert_eq!(result.items[0].matched_by.as_deref(), Some("lexical"));
        assert!(matches!(
            result.items[0].content,
            crate::types::Content::Text { .. }
        ));
        assert!(!result.diagnostics.truncated);
        assert_eq!(
            result.diagnostics.ignored_directories,
            vec![".git", ".zvec-grep"]
        );
    }

    #[test]
    fn limit_truncates_and_reports() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_file(dir.path(), "a.txt", "x\nx\nx\nx\n");

        let mut options = options_in(dir.path());
        options.patterns = vec!["x".to_owned()];
        options.limit = Some(2);
        let result = run_lexical_search(&options).expect("search");
        assert_eq!(result.items.len(), 2);
        assert!(result.diagnostics.truncated);
    }

    #[test]
    fn missing_paths_reported_without_searching() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_file(dir.path(), "a.txt", "hello\n");

        let mut options = options_in(dir.path());
        options.patterns = vec!["hello".to_owned()];
        options.paths = vec!["nope.txt".to_owned()];
        let result = run_lexical_search(&options).expect("search");
        assert!(result.items.is_empty());
        assert_eq!(
            result.diagnostics.missing_paths,
            Some(vec!["nope.txt".to_owned()])
        );
        assert_eq!(result.diagnostics.searched_paths, Some(Vec::new()));
    }

    #[test]
    fn hard_ignored_directories_are_skipped() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_file(dir.path(), ".git/hook.txt", "needle\n");
        write_file(dir.path(), "ok.txt", "needle\n");

        let mut options = options_in(dir.path());
        options.patterns = vec!["needle".to_owned()];
        let result = run_lexical_search(&options).expect("search");
        assert_eq!(result.items.len(), 1);
        assert!(result.items[0].file.relative_path.ends_with("ok.txt"));
    }

    #[test]
    fn include_and_type_filters_apply() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_file(dir.path(), "main.rs", "needle\n");
        write_file(dir.path(), "notes.txt", "needle\n");

        let mut options = options_in(dir.path());
        options.patterns = vec!["needle".to_owned()];
        options.file_types = vec!["rust".to_owned()];
        let result = run_lexical_search(&options).expect("search");
        assert_eq!(result.items.len(), 1);
        assert!(result.items[0].file.relative_path.ends_with("main.rs"));
    }

    #[test]
    fn context_expansion_sets_excerpt_range() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_file(dir.path(), "a.txt", "one\ntwo\nthree\nfour\n");

        let mut options = options_in(dir.path());
        options.patterns = vec!["three".to_owned()];
        options.before_context = 1;
        options.after_context = 1;
        let result = run_lexical_search(&options).expect("search");
        assert_eq!(result.items.len(), 1);
        assert!(result.items[0].excerpt_range.is_some());
        match &result.items[0].content {
            crate::types::Content::Text { text } => assert_eq!(text, "two\nthree\nfour"),
            _ => panic!("expected text content"),
        }
    }
}
