//! Exhaustive lexical path: grep-crate powered text/regex search.
//!
//! In-process port of `engine/service/lexical.ts`. The TypeScript
//! implementation shells out to a ripgrep binary (`bundled-rg` with an `rg`
//! fallback); this port runs the same semantics inside the process with the
//! declared grep crates (`grep-regex` + `grep-searcher`) for matching and the
//! `ignore` crate for file discovery (hidden/ignore/max-depth/follow).
//!
//! Preserved TS semantics:
//! - multiple `--regexp` patterns (OR) plus `--file` pattern files,
//! - include/exclude path globs with `**/` expansion, `--glob`/`--iglob`
//!   filters, ripgrep file-type selection, hard-ignored `.git`/`.zvec-grep`,
//! - hidden/no-ignore/ignore-file/max-depth/max-filesize/follow options,
//! - `modifiedAfter`/`modifiedBefore` mtime post-filtering,
//! - `limit` + truncation reporting, missing-path diagnostics,
//! - before/after context expansion with `excerptRange`.

pub mod enrichment;

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use grep_matcher::Matcher as _;
use grep_regex::RegexMatcherBuilder;
use grep_searcher::{SearcherBuilder, Sink, SinkMatch};
use serde::{Deserialize, Serialize};

pub use enrichment::{
    STRUCTURE_ENRICH_FILE_LIMIT, StructureEnrichmentDiagnostics, StructureEnrichmentResult,
};

/// Directories that are always excluded, mirroring
/// `HARD_IGNORED_HIDDEN_DIRECTORIES` in the TS implementation.
pub const HARD_IGNORED_DIRECTORIES: &[&str] = &[".git", ".zvec-grep"];

/// Which backend produced a lexical result. The TS implementation reports
/// `bundled-rg` or `rg`; the Rust port always searches in-process.
///
/// Divergence: no subprocess is ever spawned, so there is no binary path or
/// raw argv to report. Diagnostics carry the effective search description
/// instead (see [`LexicalDiagnostics::patterns`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LexicalBackend {
    InProcess,
}

/// Full option set for one exhaustive lexical search.
///
/// Maps 1:1 onto `RgSearchOptions` in `engine/service/lexical.ts`:
/// `patterns`/`paths`/`limit`/`includePaths`/`excludePaths`/`globs`/
/// `insensitiveGlobs`/`fileTypes`/`excludedFileTypes`/`hidden`/`noIgnore`/
/// `ignoreFiles`/`maxDepth`/`maxFileSizeBytes`/`follow`/
/// `modifiedAfter`/`modifiedBefore`, plus the `ZvecGrepSearchOptions` text
/// flags (`fixedStrings`, `ignoreCase`, `wordRegexp`, `beforeContext`,
/// `afterContext`, `hidden`) and the per-file `maxCount` cap.
#[derive(Debug, Clone, Default)]
pub struct LexicalSearchOptions {
    /// Workspace root: search paths resolve against this and results are
    /// relativized against it.
    pub root: PathBuf,
    /// Regex (or literal, with `fixed_strings`) alternatives, OR-ed like
    /// repeated `--regexp` flags.
    pub patterns: Vec<String>,
    /// Pattern files (`--file`): every non-empty line is one more pattern.
    pub pattern_files: Vec<PathBuf>,
    /// Explicit search paths (files or directories), relative to `root` or
    /// absolute. Empty searches the whole root.
    pub paths: Vec<String>,
    /// Maximum items returned; one extra match is collected to report
    /// `truncated`, mirroring the TS kill-after-limit behavior.
    pub limit: Option<usize>,
    /// Whitelist path patterns (directory-prefix semantics).
    pub include_paths: Vec<String>,
    /// Blacklist path patterns (directory-prefix semantics).
    pub exclude_paths: Vec<String>,
    /// Case-sensitive glob filters (`--glob`, `!` negates, last match wins).
    pub globs: Vec<String>,
    /// Case-insensitive glob filters (`--iglob`).
    pub insensitive_globs: Vec<String>,
    /// Ripgrep file-type names to include (`--type`).
    pub file_types: Vec<String>,
    /// Ripgrep file-type names to exclude (`--type-not`).
    pub excluded_file_types: Vec<String>,
    /// Search hidden files (`--hidden`).
    pub hidden: bool,
    /// Ignore `.gitignore`/`.ignore`/`.rgignore` rules (`--no-ignore`).
    pub no_ignore: bool,
    /// Extra ignore files (`--ignore-file`).
    pub ignore_files: Vec<PathBuf>,
    /// Maximum directory depth below each searched path (`--max-depth`).
    pub max_depth: Option<usize>,
    /// Skip files larger than this (`--max-filesize`, exact bytes).
    pub max_file_size_bytes: Option<u64>,
    /// Follow symlinks (`--follow`).
    pub follow: bool,
    /// Only matches from files modified at/after this (unix millis).
    pub modified_after: Option<i64>,
    /// Only matches from files modified at/before this (unix millis).
    pub modified_before: Option<i64>,
    /// Treat patterns as literals (`--fixed-strings`).
    pub fixed_strings: bool,
    /// Case-insensitive matching (`--ignore-case`).
    pub ignore_case: bool,
    /// Case-insensitive when every pattern is lowercase (ripgrep smart case).
    pub smart_case: bool,
    /// Wrap the pattern with word boundaries (`--word-regexp`).
    pub word_regexp: bool,
    /// Context lines before each match (`beforeContext`).
    pub before_context: usize,
    /// Context lines after each match (`afterContext`).
    pub after_context: usize,
    /// Maximum matches per file (`-m`/`--max-count`).
    pub max_count: Option<usize>,
}

impl LexicalSearchOptions {
    /// Builds lexical options from the service-layer context options plus the
    /// already-resolved query patterns.
    ///
    /// Text flags come from `rg` (`ZvecGrepSearchOptions` in TS):
    /// `fixedStrings` → [`Self::fixed_strings`], `ignoreCase` →
    /// [`Self::ignore_case`], `maxCount` → [`Self::max_count`], and
    /// `contextLines` expands to both [`Self::before_context`] and
    /// [`Self::after_context`]. WordRegexp/before/after context and the
    /// discovery flags (`hidden`, `noIgnore`, `ignoreFiles`, `maxDepth`,
    /// `maxFileSizeBytes`, `follow`, `paths`) have no service-options
    /// counterpart and keep their defaults here; callers that need them set
    /// the fields directly.
    pub fn from_context(
        root: &Path,
        patterns: Vec<String>,
        context: &crate::service::types::ZvecGrepContextOptions<'_>,
    ) -> Self {
        let rg = context.rg.as_ref();
        Self {
            root: root.to_owned(),
            patterns,
            pattern_files: Vec::new(),
            paths: Vec::new(),
            limit: context.limit,
            include_paths: context.include_paths.clone(),
            exclude_paths: context.exclude_paths.clone(),
            globs: context.globs.clone(),
            insensitive_globs: context.insensitive_globs.clone(),
            file_types: context.file_types.clone(),
            excluded_file_types: context.excluded_file_types.clone(),
            modified_after: context.modified_after.map(|t| t.0),
            modified_before: context.modified_before.map(|t| t.0),
            fixed_strings: rg.is_some_and(|rg| rg.fixed_strings),
            ignore_case: rg.is_some_and(|rg| rg.case_insensitive),
            smart_case: rg.is_some_and(|rg| rg.smart_case),
            max_count: rg.and_then(|rg| rg.max_count),
            before_context: rg.and_then(|rg| rg.context_lines).unwrap_or(0),
            after_context: rg.and_then(|rg| rg.context_lines).unwrap_or(0),
            ..Self::default()
        }
    }

    /// True when hidden files participate: explicit flag or any include path
    /// reaching into a dot segment (mirrors `hiddenSearchArgs`).
    fn searches_hidden(&self) -> bool {
        self.hidden || includes_hidden_path(&self.include_paths)
    }
}

/// Diagnostics for one lexical search, mirroring `ZvecGrepRgDiagnostics`.
///
/// Divergence: `backend` is always [`LexicalBackend::InProcess`] and
/// `command` is the literal `"in-process"` — nothing is spawned. `args`
/// (raw ripgrep argv) is replaced by `patterns`, the effective pattern list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LexicalDiagnostics {
    pub backend: LexicalBackend,
    pub command: String,
    pub patterns: Vec<String>,
    pub ignored_directories: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub missing_paths: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub searched_paths: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
    pub truncated: bool,
}

/// Output of [`run_lexical_search`]: ranked matches plus diagnostics.
#[derive(Debug, Clone)]
pub struct LexicalSearchResult {
    pub items: Vec<crate::service::types::ContextItem>,
    pub diagnostics: LexicalDiagnostics,
}

/// Runs an exhaustive in-process lexical search.
///
/// Never spawns a subprocess. Returns an [`EngineError`] only for invalid
/// search setup (no pattern, unreadable pattern/ignore file, unknown file
/// type, unusable matcher); per-file IO failures during the walk skip that
/// file, mirroring ripgrep's tolerance.
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
            EngineErrorCode::new("LEXICAL.EMPTY_PATTERN"),
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
            EngineErrorCode::new("LEXICAL.UNKNOWN_FILE_TYPE"),
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
        if let Some(ignore) = ignore_matcher.as_ref() {
            if ignore.matched(path, false).is_ignore() {
                continue;
            }
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

// -----------------------------------------------------------------------------
// Search-path handling
// -----------------------------------------------------------------------------

struct CheckedSearchPaths {
    existing: Vec<String>,
    searched: Vec<String>,
    missing: Vec<String>,
}

/// Splits requested search paths into existing vs missing, mirroring
/// `checkSearchPaths`. Kept original strings so diagnostics echo the caller's
/// spelling.
fn check_search_paths(root: &Path, paths: &[String]) -> CheckedSearchPaths {
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

fn resolve_search_path(root: &Path, path: &str) -> PathBuf {
    let candidate = Path::new(path);
    if candidate.is_absolute() {
        crate::paths::normalize_path(candidate)
    } else {
        crate::paths::normalize_path(&root.join(candidate))
    }
}

fn absolute_normalized(path: &Path) -> PathBuf {
    if path.is_absolute() {
        crate::paths::normalize_path(path)
    } else {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        crate::paths::normalize_path(&cwd.join(path))
    }
}

// -----------------------------------------------------------------------------
// Patterns and matcher
// -----------------------------------------------------------------------------

/// Merges inline patterns with `--file` pattern files. Every line of a
/// pattern file is one pattern (only the line break is stripped), matching
/// ripgrep's `--file` handling.
fn load_patterns(patterns: &[String], pattern_files: &[PathBuf]) -> EngineResult<Vec<String>> {
    use crate::error::{EngineError, EngineErrorCode};

    let mut merged: Vec<String> = patterns.to_vec();
    for file in pattern_files {
        let text = fs::read_to_string(file).map_err(|error| {
            EngineError::new(
                EngineErrorCode::new("LEXICAL.PATTERN_FILE_UNREADABLE"),
                format!("unable to read pattern file {}", file.display()),
            )
            .with_context(format!("error={error}"))
        })?;
        for line in text.split('\n') {
            merged.push(line.strip_suffix('\r').unwrap_or(line).to_owned());
        }
    }
    Ok(merged.into_iter().filter(|p| !p.is_empty()).collect())
}

fn build_matcher(
    options: &LexicalSearchOptions,
    patterns: &[String],
) -> EngineResult<grep_regex::RegexMatcher> {
    use crate::error::{EngineError, EngineErrorCode};

    let mut alternatives: Vec<String> = Vec::with_capacity(patterns.len());
    for pattern in patterns {
        if options.fixed_strings {
            alternatives.push(regex::escape(pattern));
        } else {
            alternatives.push(pattern.clone());
        }
    }
    let mut combined = alternatives.join("|");
    combined = format!("(?:{combined})");
    if options.word_regexp {
        combined = format!(r"\b{combined}\b");
    }
    let case_insensitive = options.ignore_case
        || (options.smart_case
            && !patterns
                .iter()
                .flat_map(|pattern| pattern.chars())
                .any(|ch| ch.is_uppercase()));
    RegexMatcherBuilder::new()
        .case_insensitive(case_insensitive)
        .unicode(true)
        .multi_line(false)
        .build(&combined)
        .map_err(|error| {
            EngineError::new(
                EngineErrorCode::new("LEXICAL.INVALID_PATTERN"),
                "invalid search pattern",
            )
            .with_context(format!("error={error}"))
        })
}

// -----------------------------------------------------------------------------
// Path filters
// -----------------------------------------------------------------------------

/// True when any include path reaches into a dot segment, in which case hidden
/// files participate even without the explicit flag (mirrors
/// `includesHiddenPath`).
fn includes_hidden_path(patterns: &[String]) -> bool {
    patterns.iter().any(|pattern| {
        pattern.split(['/', '\\']).any(|segment| {
            !segment.is_empty() && segment.starts_with('.') && segment != "." && segment != ".."
        })
    })
}

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

fn expand_path_patterns(patterns: &[String]) -> Vec<String> {
    patterns
        .iter()
        .flat_map(|pattern| expand_path_pattern(pattern))
        .collect()
}

fn matches_any_path_pattern(patterns: &[String], relative: &str, absolute: &str) -> bool {
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
struct GlobFilter {
    rules: Vec<GlobRule>,
    has_positive: bool,
}

impl GlobFilter {
    fn new(globs: &[String], insensitive_globs: &[String]) -> Self {
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

    fn matches(&self, relative: &str, absolute: &str) -> bool {
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

fn build_ignore_matcher(
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
                EngineErrorCode::new("LEXICAL.IGNORE_FILE_INVALID"),
                format!("unable to read ignore file {}", file.display()),
            )
            .with_context(format!("error={error}")));
        }
    }
    builder.build().map(Some).map_err(|error| {
        EngineError::new(
            EngineErrorCode::new("LEXICAL.IGNORE_FILE_INVALID"),
            "unable to compile ignore files",
        )
        .with_context(format!("error={error}"))
    })
}

fn is_hard_ignored_name(file_name: &std::ffi::OsStr) -> bool {
    file_name == ".git" || file_name == ".zvec-grep"
}

/// True when the candidate lives under a hard-ignored directory, mirroring
/// `--glob '!**/.git/**'` / `--glob '!**/.zvec-grep/**'`.
fn is_hard_ignored_path(root: &Path, path: &Path) -> bool {
    let relative = path.strip_prefix(root).unwrap_or(path);
    relative
        .components()
        .any(|component| component.as_os_str() == ".git" || component.as_os_str() == ".zvec-grep")
}

fn is_searchable_file(file_type: Option<std::fs::FileType>, path: &Path, follow: bool) -> bool {
    if file_type.is_some_and(|kind| kind.is_file()) {
        return true;
    }
    // Symlink-to-file when following links: the walk entry itself is not a
    // plain file, so resolve through metadata.
    follow && fs::metadata(path).is_ok_and(|metadata| metadata.is_file())
}

fn matches_modified_time(
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

/// Absolute + root-relative display paths for a match, mirroring
/// `normalizeResultPath` (`relative(root, abs) || "."`).
fn display_paths(root: &Path, path: &Path) -> (String, String) {
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

// -----------------------------------------------------------------------------
// Match sink
// -----------------------------------------------------------------------------

/// grep-searcher sink turning line matches into ranked [`ContextItem`]s.
///
/// Rank is consumed per raw match, mirroring the TS pipeline where
/// `parseLine(line, items.length + 1)` runs ahead of `matchesModifiedTime`
/// (mtime is enforced per file before `search_path` here, so every raw match
/// is kept). The global limit stops the walk one item past the cap so the
/// caller can report `truncated`, mirroring the TS kill-after-limit behavior.
struct MatchSink<'a> {
    items: Vec<crate::service::types::ContextItem>,
    next_rank: usize,
    limit: Option<usize>,
    truncated: bool,
    root: &'a Path,
    options: &'a LexicalSearchOptions,
    matcher: &'a grep_regex::RegexMatcher,
    line_cache: HashMap<PathBuf, Option<Vec<String>>>,
    per_file_counts: HashMap<PathBuf, usize>,
}

impl MatchSink<'_> {
    fn stop_requested(&self) -> bool {
        self.limit.is_some_and(|limit| self.items.len() > limit)
    }

    fn file_match_count(&self, path: &Path) -> usize {
        self.per_file_counts.get(path).copied().unwrap_or(0)
    }
}

impl grep_searcher::SinkError for crate::error::EngineError {
    fn error_message<T: std::fmt::Display>(message: T) -> Self {
        crate::error::EngineError::new(
            crate::error::EngineErrorCode::new("LEXICAL.SEARCH_FAILED"),
            message.to_string(),
        )
    }
}

impl Sink for MatchSink<'_> {
    type Error = crate::error::EngineError;

    fn matched(
        &mut self,
        _searcher: &grep_searcher::Searcher,
        mat: &SinkMatch<'_>,
    ) -> Result<bool, Self::Error> {
        if self.stop_requested() {
            self.truncated = true;
            return Ok(false);
        }
        let Some(line_number) = mat.line_number() else {
            return Ok(true);
        };
        let path = current_sink_path();
        // Per-file `--max-count` cap (`-m`): stop this file once reached.
        if self
            .options
            .max_count
            .is_some_and(|cap| self.file_match_count(&path) >= cap)
        {
            return Ok(false);
        }
        // First submatch decides the column range, mirroring
        // `parseRipgrepJsonLine` (`submatches[0]`).
        let first = self.matcher.find(mat.bytes()).ok().flatten();
        let line_text = String::from_utf8_lossy(mat.bytes());
        let line_text = line_text.trim_end_matches(['\r', '\n']);
        let start_column = first
            .as_ref()
            .map_or(0, |m| char_column(line_text, m.start()));
        let end_column = first.as_ref().map_or_else(
            || line_text.chars().count(),
            |m| char_column(line_text, m.end()),
        );
        let rank = self.next_rank;
        self.next_rank += 1;
        let mut item = build_context_item(
            self.root,
            &path,
            line_number as usize,
            start_column,
            end_column,
            line_text.to_owned(),
            rank,
        );
        expand_context_item(&mut item, self.options, &mut self.line_cache);
        *self.per_file_counts.entry(path).or_insert(0) += 1;
        self.items.push(item);
        if self.stop_requested() {
            self.truncated = true;
            return Ok(false);
        }
        Ok(true)
    }
}

fn build_context_item(
    root: &Path,
    path: &Path,
    line_number: usize,
    start_column: usize,
    end_column: usize,
    line_text: String,
    rank: usize,
) -> crate::service::types::ContextItem {
    use crate::service::types::{ContentStatus, ContextFile, ContextItem, ContextItemKind};

    let (relative, absolute_display) = display_paths(root, path);
    ContextItem {
        kind: ContextItemKind::RgMatch,
        rank,
        file: ContextFile {
            absolute_path: absolute_display,
            relative_path: relative,
            root_path: crate::paths::to_display_path(root),
        },
        range: crate::types::Range::Text {
            start_line: line_number,
            end_line: line_number,
            start_offset: start_column,
            end_offset: end_column,
        },
        excerpt_range: None,
        content: crate::types::Content::Text { text: line_text },
        content_role: None,
        outline: None,
        status: ContentStatus::Fresh,
        score: None,
        matched_by: Some("lexical".to_owned()),
        metadata: None,
        entity_id: None,
        trace: None,
        query_groups: Vec::new(),
        container: None,
        selection_reason: None,
    }
}

// The sink API does not hand the file path to `matched`; stash the path of
// the file currently being searched in a thread-local set just before
// `search_path` runs.
thread_local! {
    static CURRENT_SEARCH_PATH: std::cell::RefCell<PathBuf> =
        std::cell::RefCell::new(PathBuf::new());
}

fn record_file(path: &Path) {
    CURRENT_SEARCH_PATH.with(|slot| {
        *slot.borrow_mut() = path.to_owned();
    });
}

fn current_sink_path() -> PathBuf {
    CURRENT_SEARCH_PATH.with(|slot| slot.borrow().clone())
}
/// Byte offset → character column, mirroring `textPositionAtByteOffset` for
/// the single-line case (multiline prefixes cannot occur: the searcher runs
/// without multiline mode).
fn char_column(line: &str, byte_offset: usize) -> usize {
    let mut end = byte_offset.min(line.len());
    while end > 0 && !line.is_char_boundary(end) {
        end -= 1;
    }
    line[..end].chars().count()
}

/// Expands a match with before/after context lines, mirroring
/// `expandContextItem`: the range widens to whole lines, the original range
/// moves to `excerptRange`, and content becomes the joined window.
fn expand_context_item(
    item: &mut crate::service::types::ContextItem,
    options: &LexicalSearchOptions,
    cache: &mut HashMap<PathBuf, Option<Vec<String>>>,
) {
    if options.before_context == 0 && options.after_context == 0 {
        return;
    }
    let crate::types::Range::Text {
        start_line,
        end_line,
        ..
    } = item.range
    else {
        return;
    };
    let path = PathBuf::from(&item.file.absolute_path);
    let lines = read_text_lines(&path, cache);
    let Some(lines) = lines else { return };
    if lines.is_empty() {
        return;
    }
    let start = start_line.max(1).min(lines.len() + 1);
    let end = end_line.max(1).min(lines.len());
    let window_start = start.saturating_sub(options.before_context).max(1);
    let window_end = (end + options.after_context).min(lines.len());
    if window_start > window_end {
        return;
    }
    let original = std::mem::replace(
        &mut item.range,
        crate::types::Range::Text {
            start_line: window_start,
            end_line: window_end,
            start_offset: 0,
            end_offset: lines[window_end - 1].chars().count(),
        },
    );
    item.excerpt_range = Some(original);
    item.content = crate::types::Content::Text {
        text: lines[window_start - 1..window_end].join("\n"),
    };
}

/// Cached whole-file line split, mirroring `readTextLines` (lossy UTF-8,
/// one trailing empty line dropped).
fn read_text_lines(
    path: &Path,
    cache: &mut HashMap<PathBuf, Option<Vec<String>>>,
) -> Option<Vec<String>> {
    if let Some(cached) = cache.get(path) {
        return cached.clone();
    }
    let lines = fs::read(path).ok().map(|bytes| {
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let mut parts: Vec<String> = text
            .split('\n')
            .map(|line| line.strip_suffix('\r').unwrap_or(line).to_owned())
            .collect();
        if parts.last().is_some_and(|last| last.is_empty()) {
            parts.pop();
        }
        parts
    });
    cache.insert(path.to_owned(), lines.clone());
    lines
}

use crate::error::EngineResult;

#[cfg(test)]
mod tests {
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
