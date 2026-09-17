//! Per-file discovery filters: hard-ignored directories, include/exclude
//! path patterns, ordered `--glob`/`--iglob` rules, ripgrep file-type
//! selection (via `utils::file_selection`), extra ignore files, and mtime
//! bounds.
//!
//! The glob filter is precompiled so the hot walk path never builds a regex
//! per file.

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::EngineResult;

/// Directories that are always excluded, mirroring
/// `HARD_IGNORED_HIDDEN_DIRECTORIES` in the TS implementation.
pub const HARD_IGNORED_DIRECTORIES: &[&str] = &[".git", ".zvec-grep"];

/// Expands one include/exclude path pattern the way `expandRipgrepPathGlob`
/// does: `./`-prefixed patterns are stripped, `**/`-prefixed and absolute
/// patterns stay as-is, everything else also matches at any depth.
fn expand_path_pattern(pattern: &str) -> Vec<String> {
    let mut normalized = pattern.trim().to_owned();
    while let Some(rest) = normalized.strip_prefix("./") {
        normalized = rest.to_owned();
    }
    if normalized.starts_with("**/") || Path::new(&normalized).is_absolute() {
        vec![normalized]
    } else {
        vec![normalized.clone(), format!("**/{normalized}")]
    }
}

pub(crate) fn expand_path_patterns(patterns: &[String]) -> Vec<String> {
    patterns
        .iter()
        .flat_map(|pattern| expand_path_pattern(pattern))
        .collect()
}

pub(crate) fn matches_any_path_pattern(
    patterns: &[String],
    relative: &str,
    absolute: &str,
) -> bool {
    patterns.iter().any(|pattern| {
        let candidate = if crate::utils::glob::is_absolute_path_pattern(pattern) {
            absolute
        } else {
            relative
        };
        crate::utils::glob::path_pattern_matches(pattern, candidate)
    })
}

struct GlobRule {
    matcher: crate::utils::glob::CompiledGlob,
    absolute: bool,
    negated: bool,
}

/// Ordered `--glob`/`--iglob` filter with last-match-wins semantics (mirrors
/// `OrderedGlobs` in `utils/file_selection.rs`). Precompiled so the hot path
/// never builds a regex per file.
pub(crate) struct GlobFilter {
    rules: Vec<GlobRule>,
    has_positive: bool,
}

impl GlobFilter {
    pub(crate) fn new(globs: &[String], insensitive_globs: &[String]) -> Self {
        let mut rules = Vec::with_capacity(globs.len() + insensitive_globs.len());
        let mut push = |raw: &str, case_insensitive: bool| {
            let trimmed = raw.trim();
            let (pattern, negated) = match trimmed.strip_prefix('!') {
                Some(rest) => (rest.trim(), true),
                None => (trimmed, false),
            };
            if !pattern.is_empty() {
                rules.push(GlobRule {
                    matcher: crate::utils::glob::CompiledGlob::new(pattern, case_insensitive),
                    absolute: crate::utils::glob::is_absolute_path_pattern(pattern),
                    negated,
                });
            }
        };
        for pattern in globs {
            push(pattern, false);
        }
        for pattern in insensitive_globs {
            push(pattern, true);
        }
        let has_positive = rules.iter().any(|rule| !rule.negated);
        Self {
            rules,
            has_positive,
        }
    }

    pub(crate) fn matches(&self, relative: &str, absolute: &str) -> bool {
        if self.rules.is_empty() {
            return true;
        }
        let mut included = !self.has_positive;
        for rule in &self.rules {
            let candidate = if rule.absolute { absolute } else { relative };
            if rule.matcher.matches(candidate) {
                included = !rule.negated;
            }
        }
        included
    }
}

pub(crate) fn build_ignore_matcher(
    root: &Path,
    ignore_files: &[PathBuf],
) -> EngineResult<Option<ignore::gitignore::Gitignore>> {
    use crate::error::{EngineError, EngineErrorCode};

    if ignore_files.is_empty() {
        return Ok(None);
    }
    let mut builder = ignore::gitignore::GitignoreBuilder::new(root);
    for file in ignore_files {
        if let Some(error) = builder.add(file) {
            return Err(EngineError::new(
                EngineErrorCode::LexicalIgnoreFileInvalid,
                format!("unable to read ignore file {}", file.display()),
            )
            .with_context(format!("error={error}")));
        }
    }
    builder.build().map(Some).map_err(|error| {
        EngineError::new(
            EngineErrorCode::LexicalIgnoreFileInvalid,
            "unable to compile ignore files",
        )
        .with_context(format!("error={error}"))
    })
}

pub(crate) fn is_hard_ignored_name(file_name: &std::ffi::OsStr) -> bool {
    file_name == ".git" || file_name == ".zvec-grep"
}

/// True when the candidate lives under a hard-ignored directory, mirroring
/// `--glob '!**/.git/**'` / `--glob '!**/.zvec-grep/**'`.
pub(crate) fn is_hard_ignored_path(root: &Path, path: &Path) -> bool {
    let relative = path.strip_prefix(root).unwrap_or(path);
    relative
        .components()
        .any(|component| component.as_os_str() == ".git" || component.as_os_str() == ".zvec-grep")
}

pub(crate) fn is_searchable_file(
    file_type: Option<fs::FileType>,
    path: &Path,
    follow: bool,
) -> bool {
    if file_type.is_some_and(|kind| kind.is_file()) {
        return true;
    }
    // Symlink-to-file when following links: the walk entry itself is not a
    // plain file, so resolve through metadata.
    follow && fs::metadata(path).is_ok_and(|metadata| metadata.is_file())
}

pub(crate) fn matches_modified_time(
    mtime_millis: Option<i64>,
    after: Option<i64>,
    before: Option<i64>,
) -> bool {
    if after.is_none() && before.is_none() {
        return true;
    }
    let Some(mtime) = mtime_millis else {
        return false;
    };
    if after.is_some_and(|bound| mtime < bound) {
        return false;
    }
    if before.is_some_and(|bound| mtime > bound) {
        return false;
    }
    true
}
