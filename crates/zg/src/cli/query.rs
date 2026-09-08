//! `zg query` flags, mirroring the `query` branch of `parseArgs`.
//!
//! Includes the hidden rg output/compat flag groups (rejected verbatim by
//! [`validate()`](super::validate()) and the `QueryArgs` placement helpers the
//! validator inspects.

use std::path::PathBuf;

use clap::Args;

use super::values::{ClientModeArg, ColorMode, DeviceArg, PreviewMode, RefreshMode, SymbolType};

/// `zg query` flags, mirroring the `query` branch of `parseArgs`.
#[derive(Debug, Args)]
pub struct QueryArgs {
    /// Natural-language queries (also `--hybrid` for extra groups).
    pub queries: Vec<String>,
    /// Exhaustive in-process lexical search instead of the index.
    #[arg(long = "rg", action = clap::ArgAction::SetTrue)]
    pub rg: bool,
    /// Extra primary hybrid-search groups (repeatable).
    #[arg(long = "hybrid")]
    pub hybrid: Vec<String>,
    /// Supplemental lexical-route groups (repeatable).
    #[arg(long = "fts")]
    pub fts: Vec<String>,
    /// Supplemental semantic-route groups (repeatable).
    #[arg(long = "vector")]
    pub vector: Vec<String>,
    /// Collapse all groups into one ranked plan.
    #[arg(long = "fuse", action = clap::ArgAction::SetTrue)]
    pub fuse: bool,
    /// Maximum items per group.
    #[arg(long = "limit")]
    pub limit: Option<usize>,
    /// Index freshness: background, wait, or off.
    #[arg(long = "refresh")]
    pub refresh: Option<RefreshMode>,
    /// Prefer exact indexed symbols when the query names a symbol.
    #[arg(long = "prefer-symbol", action = clap::ArgAction::SetTrue)]
    pub prefer_symbol: bool,
    /// Include per-hit search trace payloads.
    #[arg(long = "trace", action = clap::ArgAction::SetTrue)]
    pub trace: bool,
    /// Human-readable output instead of agent markdown.
    #[arg(long = "human", action = clap::ArgAction::SetTrue)]
    pub human: bool,
    /// Content preview length.
    #[arg(long = "preview")]
    pub preview: Option<PreviewMode>,
    /// Color mode for human output.
    #[arg(long = "color")]
    pub color: Option<ColorMode>,
    /// Disable colored output.
    #[arg(long = "no-color", action = clap::ArgAction::SetTrue)]
    pub no_color: bool,
    /// Restrict indexed results to symbol types (repeatable).
    #[arg(long = "symbol-type")]
    pub symbol_type: Vec<SymbolType>,
    /// Only query files modified after this time (millis or date).
    #[arg(long = "modified-after")]
    pub modified_after: Option<String>,
    /// Only query files modified before this time (millis or date).
    #[arg(long = "modified-before")]
    pub modified_before: Option<String>,
    /// Case-sensitive glob rules (repeatable, `!` negates).
    #[arg(long = "glob", short = 'g')]
    pub globs: Vec<String>,
    /// Case-insensitive glob rules (repeatable).
    #[arg(long = "iglob")]
    pub iglobs: Vec<String>,
    /// Ripgrep file-type names to include (repeatable).
    #[arg(long = "type", short = 't')]
    pub file_types: Vec<String>,
    /// Ripgrep file-type names to exclude (repeatable).
    #[arg(long = "type-not", short = 'T')]
    pub excluded_file_types: Vec<String>,
    /// Search hidden files (index or `--rg` only).
    #[arg(long = "hidden", action = clap::ArgAction::SetTrue)]
    pub hidden: bool,
    /// Ignore `.gitignore`/`.ignore` rules (index or `--rg` only).
    #[arg(long = "no-ignore", action = clap::ArgAction::SetTrue)]
    pub no_ignore: bool,
    /// Extra ignore files (repeatable, index or `--rg` only).
    #[arg(long = "ignore-file")]
    pub ignore_files: Vec<String>,
    /// Maximum directory depth (index or `--rg` only).
    #[arg(long = "max-depth")]
    pub max_depth: Option<u32>,
    /// Skip files larger than this (bytes or `10MB`, index or `--rg` only).
    #[arg(long = "max-filesize")]
    pub max_filesize: Option<String>,
    /// Follow symlinks (index or `--rg` only).
    #[arg(long = "follow", short = 'L', action = clap::ArgAction::SetTrue)]
    pub follow: bool,
    /// Transport selection: direct, server, or auto.
    #[arg(long = "mode")]
    pub mode: Option<ClientModeArg>,
    /// Force direct mode (requires `--mode direct`).
    #[arg(long = "force-direct", action = clap::ArgAction::SetTrue)]
    pub force_direct: bool,
    /// Daemon/global-config home override.
    #[arg(long = "home")]
    pub home: Option<PathBuf>,
    /// API key for remote embedding providers.
    #[arg(long = "api-key")]
    pub api_key: Option<String>,
    /// Local model cache directory override.
    #[arg(long = "model-cache")]
    pub model_cache: Option<PathBuf>,
    /// Device placement for local models.
    #[arg(long = "device")]
    pub device: Option<DeviceArg>,
    /// Print engine debug diagnostics to stderr.
    #[arg(long = "debug", action = clap::ArgAction::SetTrue)]
    pub debug: bool,
    /// Allow one remote-embedding operation without a stored grant.
    #[arg(
        long = "allow-remote",
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = ""
    )]
    pub allow_remote: Option<String>,
    /// Removed flag: errors with the migration message.
    #[arg(long = "json", action = clap::ArgAction::SetTrue, hide = true)]
    pub json_removed: bool,
    /// Rejected on query with the TS message (query takes no model).
    #[arg(long = "embedding", hide = true)]
    pub embedding_rejected: Option<String>,
    /// Rejected on query with the TS message.
    #[arg(long = "endpoint", hide = true)]
    pub endpoint_rejected: Option<String>,
    /// Rejected on query with the TS message.
    #[arg(long = "embedding-concurrency", hide = true)]
    pub embedding_concurrency_rejected: Option<String>,
    /// Case-insensitive matching (`--rg`).
    #[arg(long = "ignore-case", short = 'i', action = clap::ArgAction::SetTrue)]
    pub ignore_case: bool,
    /// Wrap patterns with word boundaries (`--rg`).
    #[arg(long = "word-regexp", short = 'w', action = clap::ArgAction::SetTrue)]
    pub word_regexp: bool,
    /// Treat patterns as literals (`--rg`).
    #[arg(long = "fixed-strings", short = 'F', action = clap::ArgAction::SetTrue)]
    pub fixed_strings: bool,
    /// Case-insensitive when every pattern is lowercase (`--rg`).
    #[arg(long = "smart-case", short = 'S', action = clap::ArgAction::SetTrue)]
    pub smart_case: bool,
    /// Force case-sensitive matching (`--rg`, the default).
    #[arg(long = "case-sensitive", action = clap::ArgAction::SetTrue)]
    pub case_sensitive: bool,
    /// Match whole lines (`--rg`): patterns become `^(?:pat)$`.
    #[arg(long = "line-regexp", short = 'x', action = clap::ArgAction::SetTrue)]
    pub line_regexp: bool,
    /// Extra regex alternatives, OR-ed (`--rg`, repeatable).
    #[arg(long = "regexp")]
    pub regexp: Vec<String>,
    /// Pattern files, one pattern per non-empty line (`--rg`, repeatable).
    #[arg(long = "file")]
    pub pattern_files: Vec<PathBuf>,
    /// Context lines around each match (`--rg`).
    #[arg(long = "context", short = 'C')]
    pub context: Option<u32>,
    /// Context lines before each match (`--rg`).
    #[arg(long = "before-context", short = 'B')]
    pub before_context: Option<u32>,
    /// Context lines after each match (`--rg`).
    #[arg(long = "after-context", short = 'A')]
    pub after_context: Option<u32>,
    /// Maximum matches per file (`--rg`).
    #[arg(long = "max-count", short = 'm')]
    pub max_count: Option<usize>,
    /// Explicit search paths, relative or absolute (`--rg`).
    #[arg(long = "paths")]
    pub rg_paths: Vec<String>,
    #[command(flatten)]
    pub rg_output: RgOutputFlags,
    #[command(flatten)]
    pub rg_compat: RgCompatFlags,
}

/// Output-changing rg flags: rejected verbatim
/// (`"{flag} changes rg output and cannot be used with managed --rg"`).
#[derive(Debug, Args, Default)]
pub struct RgOutputFlags {
    #[arg(long = "count", action = clap::ArgAction::SetTrue, hide = true)]
    pub count: bool,
    #[arg(long = "count-matches", action = clap::ArgAction::SetTrue, hide = true)]
    pub count_matches: bool,
    #[arg(long = "files", action = clap::ArgAction::SetTrue, hide = true)]
    pub files: bool,
    #[arg(long = "files-with-matches", action = clap::ArgAction::SetTrue, hide = true)]
    pub files_with_matches: bool,
    #[arg(long = "files-without-match", action = clap::ArgAction::SetTrue, hide = true)]
    pub files_without_match: bool,
    #[arg(long = "column", action = clap::ArgAction::SetTrue, hide = true)]
    pub column: bool,
    #[arg(long = "byte-offset", action = clap::ArgAction::SetTrue, hide = true)]
    pub byte_offset: bool,
    #[arg(long = "no-column", action = clap::ArgAction::SetTrue, hide = true)]
    pub no_column: bool,
    #[arg(long = "colors", action = clap::ArgAction::SetTrue, hide = true)]
    pub colors: bool,
    #[arg(long = "context-separator", action = clap::ArgAction::SetTrue, hide = true)]
    pub context_separator: bool,
    #[arg(long = "field-context-separator", action = clap::ArgAction::SetTrue, hide = true)]
    pub field_context_separator: bool,
    #[arg(long = "field-match-separator", action = clap::ArgAction::SetTrue, hide = true)]
    pub field_match_separator: bool,
    #[arg(long = "heading", action = clap::ArgAction::SetTrue, hide = true)]
    pub heading: bool,
    #[arg(long = "no-heading", action = clap::ArgAction::SetTrue, hide = true)]
    pub no_heading: bool,
    #[arg(long = "no-filename", action = clap::ArgAction::SetTrue, hide = true)]
    pub no_filename: bool,
    #[arg(long = "no-line-number", action = clap::ArgAction::SetTrue, hide = true)]
    pub no_line_number: bool,
    #[arg(long = "only-matching", action = clap::ArgAction::SetTrue, hide = true)]
    pub only_matching: bool,
    #[arg(long = "passthru", action = clap::ArgAction::SetTrue, hide = true)]
    pub passthru: bool,
    #[arg(long = "path-separator", action = clap::ArgAction::SetTrue, hide = true)]
    pub path_separator: bool,
    #[arg(long = "quiet", action = clap::ArgAction::SetTrue, hide = true)]
    pub quiet: bool,
    #[arg(long = "pretty", action = clap::ArgAction::SetTrue, hide = true)]
    pub pretty: bool,
    #[arg(long = "replace", action = clap::ArgAction::SetTrue, hide = true)]
    pub replace: bool,
    #[arg(long = "stats", action = clap::ArgAction::SetTrue, hide = true)]
    pub stats: bool,
    #[arg(long = "trim", action = clap::ArgAction::SetTrue, hide = true)]
    pub trim: bool,
    #[arg(long = "vimgrep", action = clap::ArgAction::SetTrue, hide = true)]
    pub vimgrep: bool,
}

impl RgOutputFlags {
    /// First output-changing flag present, if any.
    pub fn first_set(&self) -> Option<&'static str> {
        let flags = [
            (self.count, "--count"),
            (self.count_matches, "--count-matches"),
            (self.files, "--files"),
            (self.files_with_matches, "--files-with-matches"),
            (self.files_without_match, "--files-without-match"),
            (self.column, "--column"),
            (self.byte_offset, "--byte-offset"),
            (self.no_column, "--no-column"),
            (self.colors, "--colors"),
            (self.context_separator, "--context-separator"),
            (self.field_context_separator, "--field-context-separator"),
            (self.field_match_separator, "--field-match-separator"),
            (self.heading, "--heading"),
            (self.no_heading, "--no-heading"),
            (self.no_filename, "--no-filename"),
            (self.no_line_number, "--no-line-number"),
            (self.only_matching, "--only-matching"),
            (self.passthru, "--passthru"),
            (self.path_separator, "--path-separator"),
            (self.quiet, "--quiet"),
            (self.pretty, "--pretty"),
            (self.replace, "--replace"),
            (self.stats, "--stats"),
            (self.trim, "--trim"),
            (self.vimgrep, "--vimgrep"),
        ];
        flags.iter().find_map(|(set, name)| set.then_some(*name))
    }
}

/// Non-mappable rg compatibility flags: rejected with a directive to
/// `zvec_grep_rg` (the in-process engine never spawns rg).
#[derive(Debug, Args, Default)]
pub struct RgCompatFlags {
    #[arg(long = "invert-match", action = clap::ArgAction::SetTrue, hide = true)]
    pub invert_match: bool,
    #[arg(long = "multiline", action = clap::ArgAction::SetTrue, hide = true)]
    pub multiline: bool,
    #[arg(long = "multiline-dotall", action = clap::ArgAction::SetTrue, hide = true)]
    pub multiline_dotall: bool,
    #[arg(long = "pcre2", action = clap::ArgAction::SetTrue, hide = true)]
    pub pcre2: bool,
    #[arg(long = "text", action = clap::ArgAction::SetTrue, hide = true)]
    pub text: bool,
    #[arg(long = "binary", action = clap::ArgAction::SetTrue, hide = true)]
    pub binary: bool,
    #[arg(long = "auto-hybrid-regex", action = clap::ArgAction::SetTrue, hide = true)]
    pub auto_hybrid_regex: bool,
    #[arg(long = "crlf", action = clap::ArgAction::SetTrue, hide = true)]
    pub crlf: bool,
    #[arg(long = "no-crlf", action = clap::ArgAction::SetTrue, hide = true)]
    pub no_crlf: bool,
    #[arg(long = "mmap", action = clap::ArgAction::SetTrue, hide = true)]
    pub mmap: bool,
    #[arg(long = "no-mmap", action = clap::ArgAction::SetTrue, hide = true)]
    pub no_mmap: bool,
    #[arg(long = "no-multiline", action = clap::ArgAction::SetTrue, hide = true)]
    pub no_multiline: bool,
    #[arg(long = "no-search-zip", action = clap::ArgAction::SetTrue, hide = true)]
    pub no_search_zip: bool,
    #[arg(long = "search-zip", action = clap::ArgAction::SetTrue, hide = true)]
    pub search_zip: bool,
    #[arg(long = "no-ignore-dot", action = clap::ArgAction::SetTrue, hide = true)]
    pub no_ignore_dot: bool,
    #[arg(long = "no-ignore-files", action = clap::ArgAction::SetTrue, hide = true)]
    pub no_ignore_files: bool,
    #[arg(long = "no-ignore-global", action = clap::ArgAction::SetTrue, hide = true)]
    pub no_ignore_global: bool,
    #[arg(long = "no-ignore-parent", action = clap::ArgAction::SetTrue, hide = true)]
    pub no_ignore_parent: bool,
    #[arg(long = "no-ignore-vcs", action = clap::ArgAction::SetTrue, hide = true)]
    pub no_ignore_vcs: bool,
    #[arg(long = "no-config", action = clap::ArgAction::SetTrue, hide = true)]
    pub no_config: bool,
    #[arg(long = "no-fixed-strings", action = clap::ArgAction::SetTrue, hide = true)]
    pub no_fixed_strings: bool,
    #[arg(long = "one-file-system", action = clap::ArgAction::SetTrue, hide = true)]
    pub one_file_system: bool,
    #[arg(long = "stop-on-nonmatch", action = clap::ArgAction::SetTrue, hide = true)]
    pub stop_on_nonmatch: bool,
    #[arg(long = "unicode", action = clap::ArgAction::SetTrue, hide = true)]
    pub unicode: bool,
    #[arg(long = "no-unicode", action = clap::ArgAction::SetTrue, hide = true)]
    pub no_unicode: bool,
    #[arg(long = "glob-case-insensitive", action = clap::ArgAction::SetTrue, hide = true)]
    pub glob_case_insensitive: bool,
    #[arg(long = "dfa-size-limit", hide = true)]
    pub dfa_size_limit: Option<String>,
    #[arg(long = "encoding", hide = true)]
    pub encoding: Option<String>,
    #[arg(long = "engine", hide = true)]
    pub engine: Option<String>,
    #[arg(long = "max-columns", hide = true)]
    pub max_columns: Option<String>,
    #[arg(long = "regex-size-limit", hide = true)]
    pub regex_size_limit: Option<String>,
    #[arg(long = "threads", hide = true)]
    pub threads: Option<String>,
}

impl RgCompatFlags {
    /// First non-mappable flag present, if any.
    pub fn first_set(&self) -> Option<String> {
        let bools = [
            (self.invert_match, "--invert-match"),
            (self.multiline, "--multiline"),
            (self.multiline_dotall, "--multiline-dotall"),
            (self.pcre2, "--pcre2"),
            (self.text, "--text"),
            (self.binary, "--binary"),
            (self.auto_hybrid_regex, "--auto-hybrid-regex"),
            (self.crlf, "--crlf"),
            (self.no_crlf, "--no-crlf"),
            (self.mmap, "--mmap"),
            (self.no_mmap, "--no-mmap"),
            (self.no_multiline, "--no-multiline"),
            (self.no_search_zip, "--no-search-zip"),
            (self.search_zip, "--search-zip"),
            (self.no_ignore_dot, "--no-ignore-dot"),
            (self.no_ignore_files, "--no-ignore-files"),
            (self.no_ignore_global, "--no-ignore-global"),
            (self.no_ignore_parent, "--no-ignore-parent"),
            (self.no_ignore_vcs, "--no-ignore-vcs"),
            (self.no_config, "--no-config"),
            (self.no_fixed_strings, "--no-fixed-strings"),
            (self.one_file_system, "--one-file-system"),
            (self.stop_on_nonmatch, "--stop-on-nonmatch"),
            (self.unicode, "--unicode"),
            (self.no_unicode, "--no-unicode"),
            (self.glob_case_insensitive, "--glob-case-insensitive"),
        ];
        if let Some((_, name)) = bools.iter().find(|(set, _)| *set) {
            return Some((*name).to_owned());
        }
        let valued = [
            (self.dfa_size_limit.is_some(), "--dfa-size-limit"),
            (self.encoding.is_some(), "--encoding"),
            (self.engine.is_some(), "--engine"),
            (self.max_columns.is_some(), "--max-columns"),
            (self.regex_size_limit.is_some(), "--regex-size-limit"),
            (self.threads.is_some(), "--threads"),
        ];
        valued
            .iter()
            .find_map(|(set, name)| set.then_some((*name).to_owned()))
    }
}

impl QueryArgs {
    /// True when explicit `--fts`/`--vector` routes are present.
    pub fn has_explicit_routes(&self) -> bool {
        !self.fts.is_empty() || !self.vector.is_empty()
    }

    /// True when any ripgrep-only option is set (valid with `--rg` only).
    pub(crate) fn has_compat_options(&self) -> bool {
        self.ignore_case
            || self.word_regexp
            || self.fixed_strings
            || self.smart_case
            || self.case_sensitive
            || self.line_regexp
            || !self.regexp.is_empty()
            || !self.pattern_files.is_empty()
            || self.context.is_some()
            || self.before_context.is_some()
            || self.after_context.is_some()
            || self.max_count.is_some()
            || !self.rg_paths.is_empty()
    }

    /// First ripgrep-only option for the placement error.
    pub(crate) fn first_compat_option(&self) -> &'static str {
        if self.ignore_case {
            return "--ignore-case";
        }
        if self.word_regexp {
            return "--word-regexp";
        }
        if self.fixed_strings {
            return "--fixed-strings";
        }
        if self.smart_case {
            return "--smart-case";
        }
        if self.case_sensitive {
            return "--case-sensitive";
        }
        if self.line_regexp {
            return "--line-regexp";
        }
        if !self.regexp.is_empty() {
            return "--regexp";
        }
        if !self.pattern_files.is_empty() {
            return "--file";
        }
        if self.context.is_some() {
            return "--context";
        }
        if self.before_context.is_some() {
            return "--before-context";
        }
        if self.after_context.is_some() {
            return "--after-context";
        }
        if self.max_count.is_some() {
            return "--max-count";
        }
        "--paths"
    }

    /// True when any discovery option is set.
    pub(crate) fn has_discovery_options(&self) -> bool {
        self.hidden
            || self.no_ignore
            || !self.ignore_files.is_empty()
            || self.max_depth.is_some()
            || self.max_filesize.is_some()
            || self.follow
    }

    /// First discovery option for the placement error.
    pub(crate) fn first_discovery_option(&self) -> &'static str {
        if self.hidden {
            return "--hidden";
        }
        if self.no_ignore {
            return "--no-ignore";
        }
        if !self.ignore_files.is_empty() {
            return "--ignore-file";
        }
        if self.max_depth.is_some() {
            return "--max-depth";
        }
        if self.max_filesize.is_some() {
            return "--max-filesize";
        }
        "--follow"
    }
}
