//! Option and diagnostics types for the exhaustive lexical search.
//!
//! Maps 1:1 onto `RgSearchOptions` in `engine/service/lexical.ts`; see the
//! parent module docs for the preserved TypeScript semantics.

use serde::{Deserialize, Serialize};

use std::path::{Path, PathBuf};

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
    /// Match whole lines only (`--line-regexp`/`-x`): the combined pattern
    /// is anchored (`^(?:…)$`) after pattern-file loading and after
    /// fixed-string escaping, so `--file` patterns match exact lines too.
    pub whole_line: bool,
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
    /// [`Self::after_context`]. WordRegexp/whole-line/before/after context
    /// and the discovery flags (`hidden`, `noIgnore`, `ignoreFiles`, `maxDepth`,
    /// `maxFileSizeBytes`, `follow`, `paths`) have no service-options
    /// counterpart and keep their defaults here; callers that need them set
    /// the fields directly.
    ///
    /// Every field is set explicitly (no `..Self::default()` spread) so that
    /// adding a field is a compile error here until its service mapping is
    /// decided.
    #[must_use]
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
            hidden: false,
            no_ignore: false,
            ignore_files: Vec::new(),
            max_depth: None,
            max_file_size_bytes: None,
            follow: false,
            modified_after: context.modified_after.map(|t| t.as_millis()),
            modified_before: context.modified_before.map(|t| t.as_millis()),
            fixed_strings: rg.is_some_and(|rg| rg.fixed_strings),
            ignore_case: rg.is_some_and(|rg| rg.case_insensitive),
            smart_case: rg.is_some_and(|rg| rg.smart_case),
            word_regexp: false,
            whole_line: false,
            before_context: rg.and_then(|rg| rg.context_lines).unwrap_or(0),
            after_context: rg.and_then(|rg| rg.context_lines).unwrap_or(0),
            max_count: rg.and_then(|rg| rg.max_count),
        }
    }

    /// True when hidden files participate: only the explicit `hidden` flag
    /// opts in. Dot-segment include paths no longer auto-enable hidden
    /// search (they would otherwise leak dotfiles without consent).
    pub(crate) fn searches_hidden(&self) -> bool {
        self.hidden
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

/// Output of [`crate::lexical::run_lexical_search`]: ranked matches plus
/// diagnostics.
#[derive(Debug, Clone)]
pub struct LexicalSearchResult {
    pub items: Vec<crate::service::types::ContextItem>,
    pub diagnostics: LexicalDiagnostics,
}
