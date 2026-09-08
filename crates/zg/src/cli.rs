//! `zg` command surface: clap-derive tree mirroring `cli/args.ts`.
//!
//! The TypeScript CLI hand-rolls `parseArgs` over `process.argv`; the port
//! uses clap derive (see `docs/ts-divergence.md`). Frozen: subcommand
//! names, flag spellings (`--model-cache`, `--type-not`, `--mcp-toolset`,
//! …), and every user-facing validation message, which stays byte-identical
//! to `validateCliShape` / `parseCommand`. Clap owns `--help` rendering
//! and generic unknown-flag errors instead.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::error::CliError;

/// Top-level parser: every TS `CliCommand` plus `completions`.
#[derive(Debug, Parser)]
#[command(
    name = "zg",
    version,
    about = "Hybrid workspace search for humans and agents",
    long_about = "Hybrid workspace search: semantic + lexical retrieval over an indexed workspace, with a loopback daemon and MCP endpoint.",
    disable_help_subcommand = true
)]
pub struct Cli {
    /// Subcommand; absent prints the main help like bare `zg` (exit 0).
    #[command(subcommand)]
    pub command: Option<Command>,
}

/// All subcommands, mirroring `CliCommand` plus `completions`.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Search the workspace index (or exhaustively with `--rg`).
    Query(Box<QueryArgs>),
    /// Create, update, rebuild, or drop the workspace index.
    Index(Box<IndexArgs>),
    /// Show workspace index state.
    Status(StatusArgs),
    /// Install IDE MCP integrations.
    Install(InstallArgs),
    /// Remove IDE MCP integrations.
    Uninstall(UninstallArgs),
    /// Manage provider credentials and model defaults.
    Config(ConfigArgs),
    /// Manage remote-embedding workspace grants.
    Auth(AuthArgs),
    /// Control the loopback daemon.
    Server(ServerArgs),
    /// Show help for a command or topic.
    Help(HelpArgs),
    /// Print the version.
    Version,
    /// Print shell completions.
    Completions(CompletionsArgs),
    /// Removed alias: errors with the migration message.
    #[command(name = "serve", hide = true)]
    Serve,
}

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

/// `zg index` flags.
#[derive(Debug, Args)]
pub struct IndexArgs {
    /// Workspace root to index (default: current directory).
    pub roots: Vec<PathBuf>,
    /// Delete the index instead of building it.
    #[arg(long = "drop", action = clap::ArgAction::SetTrue)]
    pub drop: bool,
    /// Confirm destructive operations without prompting.
    #[arg(long = "yes", action = clap::ArgAction::SetTrue)]
    pub yes: bool,
    /// Rebuild from scratch.
    #[arg(long = "rebuild", action = clap::ArgAction::SetTrue)]
    pub rebuild: bool,
    /// Replace the index root-path configuration.
    #[arg(long = "reset-paths", action = clap::ArgAction::SetTrue)]
    pub reset_paths: bool,
    /// Case-sensitive glob rules (repeatable).
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
    /// Index hidden files.
    #[arg(long = "hidden", action = clap::ArgAction::SetTrue)]
    pub hidden: bool,
    /// Ignore `.gitignore`/`.ignore` rules.
    #[arg(long = "no-ignore", action = clap::ArgAction::SetTrue)]
    pub no_ignore: bool,
    /// Extra ignore files (repeatable).
    #[arg(long = "ignore-file")]
    pub ignore_files: Vec<String>,
    /// Maximum directory depth.
    #[arg(long = "max-depth")]
    pub max_depth: Option<u32>,
    /// Skip files larger than this (bytes or `10MB`).
    #[arg(long = "max-filesize")]
    pub max_filesize: Option<String>,
    /// Follow symlinks.
    #[arg(long = "follow", short = 'L', action = clap::ArgAction::SetTrue)]
    pub follow: bool,
    /// Embedding requests processed concurrently.
    #[arg(long = "embedding-concurrency")]
    pub embedding_concurrency: Option<usize>,
    /// Transport selection: direct, server, or auto.
    #[arg(long = "mode")]
    pub mode: Option<ClientModeArg>,
    /// Force direct mode (requires `--mode direct`).
    #[arg(long = "force-direct", action = clap::ArgAction::SetTrue)]
    pub force_direct: bool,
    /// Daemon/global-config home override.
    #[arg(long = "home")]
    pub home: Option<PathBuf>,
    /// Explicit embedding model reference for a new index.
    #[arg(long = "embedding")]
    pub embedding: Option<String>,
    /// Local model cache directory override.
    #[arg(long = "model-cache")]
    pub model_cache: Option<PathBuf>,
    /// Device placement for local models.
    #[arg(long = "device")]
    pub device: Option<DeviceArg>,
    /// API key for remote embedding providers.
    #[arg(long = "api-key")]
    pub api_key: Option<String>,
    /// Remote embedding endpoint override.
    #[arg(long = "endpoint")]
    pub endpoint: Option<String>,
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
}

/// `zg status` flags.
#[derive(Debug, Args)]
pub struct StatusArgs {
    /// Workspace root (default: current directory).
    pub roots: Vec<PathBuf>,
    /// Fail when the index is not ready.
    #[arg(long = "check-ready", action = clap::ArgAction::SetTrue)]
    pub check_ready: bool,
    /// Transport selection: direct, server, or auto.
    #[arg(long = "mode")]
    pub mode: Option<ClientModeArg>,
    /// Daemon/global-config home override.
    #[arg(long = "home")]
    pub home: Option<PathBuf>,
    /// Explicit embedding model reference.
    #[arg(long = "embedding")]
    pub embedding: Option<String>,
    /// Local model cache directory override.
    #[arg(long = "model-cache")]
    pub model_cache: Option<PathBuf>,
    /// Device placement for local models.
    #[arg(long = "device")]
    pub device: Option<DeviceArg>,
    /// API key for remote embedding providers.
    #[arg(long = "api-key")]
    pub api_key: Option<String>,
    /// Remote embedding endpoint override.
    #[arg(long = "endpoint")]
    pub endpoint: Option<String>,
    /// Print engine debug diagnostics to stderr.
    #[arg(long = "debug", action = clap::ArgAction::SetTrue)]
    pub debug: bool,
    /// Color mode for status output.
    #[arg(long = "color")]
    pub color: Option<ColorMode>,
    /// Disable colored output.
    #[arg(long = "no-color", action = clap::ArgAction::SetTrue)]
    pub no_color: bool,
}

/// `zg install` flags.
#[derive(Debug, Args)]
pub struct InstallArgs {
    /// Integration targets: claude, codex, opencode, cursor, qwen, qoder
    /// (repeatable, comma-separated).
    #[arg(long = "target")]
    pub target: Vec<String>,
    /// MCP tool request timeout in seconds.
    #[arg(long = "mcp-tool-timeout")]
    pub mcp_tool_timeout: Option<u32>,
    /// Environment variable carrying the daemon bearer token (http only).
    #[arg(long = "mcp-token-env")]
    pub mcp_token_env: Option<String>,
    /// MCP transport: stdio (default) or http.
    #[arg(long = "mcp-transport")]
    pub mcp_transport: Option<McpTransportArg>,
    /// MCP toolset: agent (default) or full.
    #[arg(long = "mcp-toolset")]
    pub mcp_toolset: Option<McpToolsetArg>,
    /// Skip confirmation prompts and allow overwrites.
    #[arg(long = "yes", action = clap::ArgAction::SetTrue)]
    pub yes: bool,
    /// Replace unmanaged entries without prompting.
    #[arg(long = "force", action = clap::ArgAction::SetTrue)]
    pub force: bool,
}

/// `zg uninstall` flags.
#[derive(Debug, Args)]
pub struct UninstallArgs {
    /// Integration targets (repeatable, comma-separated).
    #[arg(long = "target")]
    pub target: Vec<String>,
    /// Skip confirmation prompts.
    #[arg(long = "yes", action = clap::ArgAction::SetTrue)]
    pub yes: bool,
}

/// `zg config model set` / `zg config provider set`.
#[derive(Debug, Args)]
pub struct ConfigArgs {
    #[command(subcommand)]
    pub target: Option<ConfigTarget>,
}

/// Config targets, mirroring `config model set` / `config provider set`.
#[derive(Debug, Subcommand)]
pub enum ConfigTarget {
    /// Per-model overrides and defaults.
    Model(ConfigModelCmd),
    /// Per-provider credentials.
    Provider(ConfigProviderCmd),
}

/// `zg config model <op>`.
#[derive(Debug, Args)]
pub struct ConfigModelCmd {
    #[command(subcommand)]
    pub op: Option<ConfigModelOp>,
}

/// `zg config model set` operations.
#[derive(Debug, Subcommand)]
pub enum ConfigModelOp {
    /// Set endpoint/device/default for one catalog reference.
    Set(ConfigModelSetArgs),
}

/// `zg config model set` flags.
#[derive(Debug, Args)]
pub struct ConfigModelSetArgs {
    /// Catalog reference (`provider/model`).
    pub reference: Vec<String>,
    /// Remote endpoint override (remote models only).
    #[arg(long = "endpoint")]
    pub endpoint: Option<String>,
    /// Device placement (local models only).
    #[arg(long = "device")]
    pub device: Option<DeviceArg>,
    /// Make this reference the global default embedding.
    #[arg(long = "default", action = clap::ArgAction::SetTrue)]
    pub default: bool,
}

/// `zg config provider <op>`.
#[derive(Debug, Args)]
pub struct ConfigProviderCmd {
    #[command(subcommand)]
    pub op: Option<ConfigProviderOp>,
}

/// `zg config provider set` operations.
#[derive(Debug, Subcommand)]
pub enum ConfigProviderOp {
    /// Store the API key for one provider.
    Set(ConfigProviderSetArgs),
}

/// `zg config provider set` flags.
#[derive(Debug, Args)]
pub struct ConfigProviderSetArgs {
    /// Provider name (not `local`).
    pub reference: Vec<String>,
    /// API key to store.
    #[arg(long = "api-key")]
    pub api_key: Option<String>,
}

/// `zg auth grant|status|revoke`.
#[derive(Debug, Args)]
pub struct AuthArgs {
    #[command(subcommand)]
    pub action: Option<AuthAction>,
    /// Daemon/global-config home override.
    #[arg(long = "home", global = true)]
    pub home: Option<PathBuf>,
    /// Explicit embedding model reference.
    #[arg(long = "embedding", global = true)]
    pub embedding: Option<String>,
    /// Local model cache directory override.
    #[arg(long = "model-cache", global = true)]
    pub model_cache: Option<PathBuf>,
    /// Device placement for local models.
    #[arg(long = "device", global = true)]
    pub device: Option<DeviceArg>,
    /// API key for remote embedding providers.
    #[arg(long = "api-key", global = true)]
    pub api_key: Option<String>,
    /// Remote embedding endpoint override.
    #[arg(long = "endpoint", global = true)]
    pub endpoint: Option<String>,
}

/// Auth actions, mirroring `grant|status|revoke`.
#[derive(Debug, Subcommand)]
pub enum AuthAction {
    /// Grant workspace remote-embedding authorization.
    Grant(AuthGrantArgs),
    /// Show workspace authorization status.
    Status(AuthRootArgs),
    /// Revoke workspace grants.
    Revoke(AuthRootArgs),
}

/// `zg auth grant` flags.
#[derive(Debug, Args)]
pub struct AuthGrantArgs {
    /// Workspace root (default: nearest indexed ancestor or cwd).
    pub roots: Vec<PathBuf>,
    /// Capability to grant (only `embedding`).
    #[arg(long = "capability")]
    pub capability: Option<CapabilityArg>,
    /// Grant scope (only `workspace`).
    #[arg(long = "scope")]
    pub scope: Option<ScopeArg>,
}

/// `zg auth status|revoke` flags.
#[derive(Debug, Args)]
pub struct AuthRootArgs {
    /// Workspace root (default: nearest indexed ancestor or cwd).
    pub roots: Vec<PathBuf>,
    /// Rejected unless granting, with the TS message.
    #[arg(long = "capability", hide = true)]
    pub capability_rejected: Option<String>,
    /// Rejected unless granting, with the TS message.
    #[arg(long = "scope", hide = true)]
    pub scope_rejected: Option<String>,
}

/// `zg server on|off|status|run` and `--stdio`.
#[derive(Debug, Args)]
pub struct ServerArgs {
    #[command(subcommand)]
    pub action: Option<ServerAction>,
    /// Serve MCP over stdio instead of HTTP.
    #[arg(long = "stdio", global = true, action = clap::ArgAction::SetTrue)]
    pub stdio: bool,
    /// Listen address for `on`/`run` (loopback only).
    #[arg(long = "listen", global = true)]
    pub listen: Option<String>,
    /// Bearer-token file for the daemon.
    #[arg(long = "token-file", global = true)]
    pub token_file: Option<PathBuf>,
    /// MCP toolset: agent (default) or full.
    #[arg(long = "mcp-toolset", global = true)]
    pub mcp_toolset: Option<McpToolsetArg>,
    /// Daemon/global-config home override.
    #[arg(long = "home", global = true)]
    pub home: Option<PathBuf>,
    /// Explicit embedding model reference (run/stdio).
    #[arg(long = "embedding", global = true)]
    pub embedding: Option<String>,
    /// Local model cache directory override (run/stdio).
    #[arg(long = "model-cache", global = true)]
    pub model_cache: Option<PathBuf>,
    /// Device placement for local models (run/stdio).
    #[arg(long = "device", global = true)]
    pub device: Option<DeviceArg>,
    /// API key for remote embedding providers (run/stdio).
    #[arg(long = "api-key", global = true)]
    pub api_key: Option<String>,
    /// Remote embedding endpoint override (run/stdio).
    #[arg(long = "endpoint", global = true)]
    pub endpoint: Option<String>,
}

/// Server actions.
#[derive(Debug, Subcommand)]
pub enum ServerAction {
    /// Spawn the daemon in the background.
    On,
    /// Stop the running daemon.
    Off,
    /// Show daemon liveness.
    Status(ServerStatusArgs),
    /// Run the daemon in the foreground.
    Run,
}

/// `zg server status` flags.
#[derive(Debug, Args)]
pub struct ServerStatusArgs {
    /// Fail when the server is not ready.
    #[arg(long = "check-ready", action = clap::ArgAction::SetTrue)]
    pub check_ready: bool,
}

/// `zg help [topic]`.
#[derive(Debug, Args)]
pub struct HelpArgs {
    /// Command or topic.
    pub topic: Option<String>,
}

/// `zg completions <shell>`.
#[derive(Debug, Args)]
pub struct CompletionsArgs {
    /// Shell to complete for.
    pub shell: clap_complete::Shell,
}

/// Transport selection, mirroring `ZvecGrepClientMode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ClientModeArg {
    /// In-process engine.
    Direct,
    /// Loopback daemon over MCP.
    Server,
    /// Daemon when ready, else in-process.
    Auto,
}

/// Index freshness selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum RefreshMode {
    /// Search now; refresh in the background (server only).
    Background,
    /// Wait for a fresh index before searching.
    Wait,
    /// Never refresh.
    Off,
}

/// Content preview length.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum PreviewMode {
    /// No content lines.
    None,
    /// Short source window.
    Short,
    /// Full content.
    Full,
}

/// Color selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ColorMode {
    /// Color when writing to a terminal.
    Auto,
    /// Always colorize.
    Always,
    /// Never colorize.
    Never,
}

/// Indexed symbol kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SymbolType {
    /// Modules.
    Module,
    /// Classes.
    Class,
    /// Interfaces.
    Interface,
    /// Functions.
    Function,
    /// Values.
    Value,
    /// Aliases.
    Alias,
}

/// Device placement for local models.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum DeviceArg {
    /// Automatic selection.
    Auto,
    /// CPU execution.
    Cpu,
    /// Apple Metal.
    Metal,
    /// Vulkan.
    Vulkan,
    /// CUDA.
    Cuda,
}

/// Authorization capability (only `embedding` exists).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum CapabilityArg {
    /// Remote-embedding capability.
    Embedding,
}

/// Authorization scope (only `workspace` exists).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ScopeArg {
    /// Workspace scope.
    Workspace,
}

/// MCP toolset selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum McpToolsetArg {
    /// Search only.
    Agent,
    /// Search plus index lifecycle, rg, and status.
    Full,
}

/// Install transport selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum McpTransportArg {
    /// Spawn `zg server --stdio`.
    Stdio,
    /// Connect to the daemon HTTP endpoint.
    Http,
}

/// Validates cross-flag shapes with `args.ts`-verbatim messages.
///
/// Placement the clap tree already enforces (a flag that only exists on
/// one subcommand) needs no check here; everything below depends on
/// values or combinations clap cannot express.
pub fn validate(cli: &Cli) -> Result<(), CliError> {
    let Some(command) = &cli.command else {
        return Ok(());
    };
    match command {
        Command::Query(args) => validate_query(args),
        Command::Index(args) => validate_index(args),
        Command::Status(args) => validate_status(args),
        Command::Install(args) => validate_install(args),
        Command::Uninstall(args) => validate_uninstall(args),
        Command::Config(args) => validate_config(args),
        Command::Auth(args) => validate_auth(args),
        Command::Server(args) => validate_server(args),
        Command::Help(_) | Command::Version | Command::Completions(_) | Command::Serve => Ok(()),
    }
}

fn validate_query(args: &QueryArgs) -> Result<(), CliError> {
    if args.json_removed {
        return Err(CliError::usage(
            "--json has been removed; use the default agent markdown output or --human",
        ));
    }
    if let Some(flag) = args.rg_output.first_set() {
        return Err(CliError::usage(format!(
            "{flag} changes rg output and cannot be used with managed --rg"
        )));
    }
    if let Some(flag) = args.rg_compat.first_set() {
        return Err(CliError::rg_incompatible(&flag));
    }
    if let Some(value) = &args.allow_remote {
        if !value.is_empty() {
            return Err(CliError::usage("--allow-remote does not take a value"));
        }
    }
    if args.embedding_rejected.is_some() {
        return Err(CliError::usage(
            "--embedding is not supported with zg query",
        ));
    }
    if args.endpoint_rejected.is_some() {
        return Err(CliError::usage("--endpoint is not supported with zg query"));
    }
    if args.embedding_concurrency_rejected.is_some() {
        return Err(CliError::usage(
            "--embedding-concurrency is not supported with zg query",
        ));
    }
    if args.rg && (args.has_explicit_routes() || !args.hybrid.is_empty()) {
        return Err(CliError::usage(
            "--rg cannot be combined with --hybrid, --fts, or --vector",
        ));
    }
    if args.rg && args.fuse {
        return Err(CliError::usage("--rg cannot be combined with --fuse"));
    }
    if args.force_direct && args.mode != Some(ClientModeArg::Direct) {
        return Err(CliError::usage("--force-direct requires --mode direct"));
    }
    if args.rg && args.preview.is_some() {
        return Err(CliError::usage(
            "--preview is not supported with --rg; use -A/-B/-C for rg context",
        ));
    }
    if args.rg && args.trace {
        return Err(CliError::usage("--rg cannot be combined with --trace"));
    }
    if args.rg && (args.prefer_symbol || !args.symbol_type.is_empty()) {
        return Err(CliError::usage(
            "--rg cannot be combined with indexed symbol options",
        ));
    }
    if args.rg && args.refresh.is_some() {
        return Err(CliError::usage(
            "--rg cannot be combined with indexed refresh options",
        ));
    }
    if !args.rg && args.has_compat_options() {
        let flag = args.first_compat_option();
        return Err(CliError::usage(format!(
            "{flag} can only be used with --rg"
        )));
    }
    if args.has_discovery_options() && !args.rg {
        let flag = args.first_discovery_option();
        return Err(CliError::usage(format!(
            "{flag} can only be used with index commands or zg query --rg"
        )));
    }
    Ok(())
}

fn validate_index(args: &IndexArgs) -> Result<(), CliError> {
    if args.roots.len() > 1 {
        return Err(CliError::usage("zg index accepts at most one root path"));
    }
    if let Some(value) = &args.allow_remote {
        if !value.is_empty() {
            return Err(CliError::usage("--allow-remote does not take a value"));
        }
    }
    if args.force_direct && args.mode != Some(ClientModeArg::Direct) {
        return Err(CliError::usage("--force-direct requires --mode direct"));
    }
    if args.drop
        && (args.rebuild
            || args.reset_paths
            || args.home.is_some()
            || args.embedding.is_some()
            || args.model_cache.is_some()
            || args.device.is_some()
            || args.api_key.is_some()
            || args.endpoint.is_some()
            || !args.globs.is_empty()
            || !args.iglobs.is_empty()
            || !args.file_types.is_empty()
            || !args.excluded_file_types.is_empty()
            || args.hidden
            || args.no_ignore
            || !args.ignore_files.is_empty()
            || args.max_depth.is_some()
            || args.max_filesize.is_some()
            || args.debug
            || args.follow
            || args.embedding_concurrency.is_some())
    {
        return Err(CliError::usage(
            "zg index --drop cannot be combined with indexing options",
        ));
    }
    Ok(())
}

fn validate_status(args: &StatusArgs) -> Result<(), CliError> {
    if args.roots.len() > 1 {
        return Err(CliError::usage("zg status accepts at most one root path"));
    }
    Ok(())
}

fn validate_install(args: &InstallArgs) -> Result<(), CliError> {
    if args.mcp_transport != Some(McpTransportArg::Http) && args.mcp_token_env.is_some() {
        return Err(CliError::usage(
            "--mcp-token-env requires --mcp-transport http",
        ));
    }
    Ok(())
}

fn validate_uninstall(_args: &UninstallArgs) -> Result<(), CliError> {
    Ok(())
}

fn validate_config(args: &ConfigArgs) -> Result<(), CliError> {
    const MISSING: &str = "zg config requires provider set or model set";
    match &args.target {
        None => Err(CliError::usage(MISSING)),
        Some(ConfigTarget::Model(cmd)) => match &cmd.op {
            None => Err(CliError::usage(MISSING)),
            Some(ConfigModelOp::Set(set)) => {
                if set.reference.len() != 1 {
                    return Err(CliError::usage(
                        "zg config model set requires exactly one reference",
                    ));
                }
                Ok(())
            }
        },
        Some(ConfigTarget::Provider(cmd)) => match &cmd.op {
            None => Err(CliError::usage(MISSING)),
            Some(ConfigProviderOp::Set(set)) => {
                if set.reference.len() != 1 {
                    return Err(CliError::usage(
                        "zg config provider set requires exactly one reference",
                    ));
                }
                if set.api_key.is_none() {
                    return Err(CliError::usage("zg config provider set requires --api-key"));
                }
                Ok(())
            }
        },
    }
}

fn validate_auth(args: &AuthArgs) -> Result<(), CliError> {
    let Some(action) = &args.action else {
        return Err(CliError::usage("zg auth requires grant, status, or revoke"));
    };
    match action {
        AuthAction::Grant(grant) => {
            if grant.roots.len() > 1 {
                return Err(CliError::usage("zg auth grant accepts at most one root"));
            }
            Ok(())
        }
        AuthAction::Status(status) => {
            if status.roots.len() > 1 {
                return Err(CliError::usage("zg auth status accepts at most one root"));
            }
            if status.capability_rejected.is_some() || status.scope_rejected.is_some() {
                return Err(CliError::usage(
                    "--capability and --scope can only be used with zg auth grant",
                ));
            }
            Ok(())
        }
        AuthAction::Revoke(revoke) => {
            if revoke.roots.len() > 1 {
                return Err(CliError::usage("zg auth revoke accepts at most one root"));
            }
            if revoke.capability_rejected.is_some() || revoke.scope_rejected.is_some() {
                return Err(CliError::usage(
                    "--capability and --scope can only be used with zg auth grant",
                ));
            }
            Ok(())
        }
    }
}

fn validate_server(args: &ServerArgs) -> Result<(), CliError> {
    let action_name = match &args.action {
        Some(ServerAction::On) => Some("on"),
        Some(ServerAction::Off) => Some("off"),
        Some(ServerAction::Status(_)) => Some("status"),
        Some(ServerAction::Run) => Some("run"),
        None => None,
    };
    if action_name.is_none() && !args.stdio {
        return Err(CliError::usage(
            "zg server requires on, off, status, run, or --stdio",
        ));
    }
    if action_name.is_some() && args.stdio {
        return Err(CliError::usage(
            "--stdio cannot be combined with a server action",
        ));
    }
    if args.listen.is_some()
        && action_name != Some("run")
        && action_name != Some("on")
        && !args.stdio
    {
        return Err(CliError::usage(
            "--listen can only be used with zg server on or run",
        ));
    }
    if args.token_file.is_some() && action_name == Some("status") {
        return Err(CliError::usage(
            "--token-file cannot be used with zg server status",
        ));
    }
    if args.mcp_toolset.is_some()
        && action_name != Some("on")
        && action_name != Some("run")
        && !args.stdio
    {
        return Err(CliError::usage(
            "--mcp-toolset can only be used with zg server on or run",
        ));
    }
    Ok(())
}

impl QueryArgs {
    /// True when explicit `--fts`/`--vector` routes are present.
    pub fn has_explicit_routes(&self) -> bool {
        !self.fts.is_empty() || !self.vector.is_empty()
    }

    /// True when any ripgrep-only option is set (valid with `--rg` only).
    fn has_compat_options(&self) -> bool {
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
    fn first_compat_option(&self) -> &'static str {
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
    fn has_discovery_options(&self) -> bool {
        self.hidden
            || self.no_ignore
            || !self.ignore_files.is_empty()
            || self.max_depth.is_some()
            || self.max_filesize.is_some()
            || self.follow
    }

    /// First discovery option for the placement error.
    fn first_discovery_option(&self) -> &'static str {
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

/// Parses `--max-filesize` values: plain bytes or `K`/`M`/`G` suffixed
/// (`10MB`, `512k`). Mirrors `parseByteSize` in scale, not in message.
pub fn parse_byte_size(value: &str) -> Result<u64, CliError> {
    let trimmed = value.trim();
    let split = trimmed
        .char_indices()
        .find(|(_, marker)| marker.is_alphabetic())
        .map(|(index, _)| index);
    let (digits, suffix) = match split {
        Some(index) => trimmed.split_at(index),
        None => (trimmed, ""),
    };
    let base: f64 = digits.trim().parse().map_err(|_| {
        CliError::usage(format!(
            "--max-filesize must be a byte size, got \"{value}\""
        ))
    })?;
    if base < 0.0 {
        return Err(CliError::usage(format!(
            "--max-filesize must be a byte size, got \"{value}\""
        )));
    }
    let factor = match suffix.trim().to_lowercase().as_str() {
        "" | "b" => 1.0,
        "k" | "kb" => 1024.0,
        "m" | "mb" => 1024.0 * 1024.0,
        "g" | "gb" => 1024.0 * 1024.0 * 1024.0,
        _ => {
            return Err(CliError::usage(format!(
                "--max-filesize must be a byte size, got \"{value}\""
            )));
        }
    };
    Ok((base * factor) as u64)
}

/// Parses `--modified-after`/`--modified-before`: unix millis, RFC 3339,
/// `YYYY-MM-DD HH:MM:SS`, or `YYYY-MM-DD` (local midnight). Mirrors the
/// MCP `TimeInput` accepted set and message exactly.
pub fn parse_modified_time(value: &str, option: &str) -> Result<i64, CliError> {
    use chrono::TimeZone;
    let trimmed = value.trim();
    if !trimmed.is_empty() && trimmed.chars().all(|marker| marker.is_ascii_digit()) {
        return trimmed.parse::<i64>().map_err(|_| invalid_time(option));
    }
    if let Ok(date) = chrono::NaiveDate::parse_from_str(trimmed, "%Y-%m-%d")
        && let Some(midnight) = date.and_hms_opt(0, 0, 0)
        && let Some(local) = chrono::Local.from_local_datetime(&midnight).single()
    {
        return Ok(local.timestamp_millis());
    }
    if let Ok(moment) = chrono::DateTime::parse_from_rfc3339(trimmed) {
        return Ok(moment.timestamp_millis());
    }
    for format in ["%Y-%m-%dT%H:%M:%S", "%Y-%m-%d %H:%M:%S"] {
        if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(trimmed, format)
            && let Some(local) = chrono::Local.from_local_datetime(&naive).single()
        {
            return Ok(local.timestamp_millis());
        }
    }
    Err(invalid_time(option))
}

fn invalid_time(option: &str) -> CliError {
    CliError::usage(format!(
        "{option} requires an epoch millisecond value or a parseable date"
    ))
}

/// Validates `--mcp-token-env` names, mirroring `parseEnvironmentVariable`.
pub fn parse_environment_variable(value: &str, option: &str) -> Result<String, CliError> {
    let valid = !value.is_empty()
        && value
            .chars()
            .all(|marker| marker.is_ascii_alphanumeric() || marker == '_')
        && !value
            .chars()
            .next()
            .is_some_and(|marker| marker.is_ascii_digit());
    if valid {
        return Ok(value.to_owned());
    }
    Err(CliError::usage(format!(
        "{option} must be a valid environment variable name"
    )))
}

/// Splits `--target` values on commas and whitespace, dropping empties.
pub fn split_targets(values: &[String]) -> Vec<String> {
    values
        .iter()
        .flat_map(|value| value.split([',', ' ', '\t', '\n']))
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use clap::Parser;

    fn parse(argv: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(argv)
    }

    #[test]
    fn query_snapshot() {
        let cli = parse(&[
            "zg",
            "query",
            "hello world",
            "--hybrid",
            "other",
            "--limit",
            "5",
            "--mode",
            "direct",
            "--glob",
            "*.rs",
            "--trace",
        ])
        .unwrap();
        let Command::Query(args) = cli.command.unwrap() else {
            panic!("expected query");
        };
        assert_eq!(args.queries, vec!["hello world"]);
        assert_eq!(args.hybrid, vec!["other"]);
        assert_eq!(args.limit, Some(5));
        assert_eq!(args.mode, Some(ClientModeArg::Direct));
        assert_eq!(args.globs, vec!["*.rs"]);
        assert!(args.trace);
        validate(&Cli {
            command: Some(Command::Query(args)),
        })
        .unwrap();
    }

    #[test]
    fn rg_snapshot_with_short_flags() {
        let cli = parse(&["zg", "query", "--rg", "pattern", "-i", "-C", "2"]).unwrap();
        let Command::Query(args) = cli.command.unwrap() else {
            panic!("expected query");
        };
        assert!(args.rg && args.ignore_case);
        assert_eq!(args.context, Some(2));
        validate(&Cli {
            command: Some(Command::Query(args)),
        })
        .unwrap();
    }

    #[test]
    fn rg_rejects_hybrid() {
        let cli = parse(&["zg", "query", "--rg", "x", "--hybrid", "y"]).unwrap();
        let error = validate(&cli).expect_err("--rg + --hybrid must fail");
        assert_eq!(
            error.to_string(),
            "--rg cannot be combined with --hybrid, --fts, or --vector"
        );
    }

    #[test]
    fn rg_output_option_uses_ts_text() {
        let cli = parse(&["zg", "query", "--rg", "x", "--count"]).unwrap();
        let error = validate(&cli).expect_err("--count must fail");
        assert_eq!(
            error.to_string(),
            "--count changes rg output and cannot be used with managed --rg"
        );
    }

    #[test]
    fn compat_flag_directs_to_the_tool() {
        let cli = parse(&["zg", "query", "--rg", "x", "--threads", "4"]).unwrap();
        let error = validate(&cli).expect_err("--threads must fail");
        assert!(error.to_string().contains("zvec_grep_rg"));
    }

    #[test]
    fn compat_flag_requires_rg() {
        let cli = parse(&["zg", "query", "x", "--ignore-case"]).unwrap();
        let error = validate(&cli).expect_err("--ignore-case without --rg must fail");
        assert_eq!(
            error.to_string(),
            "--ignore-case can only be used with --rg"
        );
    }

    #[test]
    fn discovery_flag_requires_index_or_rg() {
        let cli = parse(&["zg", "query", "x", "--hidden"]).unwrap();
        let error = validate(&cli).expect_err("--hidden without --rg must fail");
        assert_eq!(
            error.to_string(),
            "--hidden can only be used with index commands or zg query --rg"
        );
    }

    #[test]
    fn removed_json_flag_uses_ts_text() {
        let cli = parse(&["zg", "query", "x", "--json"]).unwrap();
        let error = validate(&cli).expect_err("--json must fail");
        assert_eq!(
            error.to_string(),
            "--json has been removed; use the default agent markdown output or --human"
        );
    }

    #[test]
    fn auth_shape_errors_are_verbatim() {
        let cli = parse(&["zg", "auth", "status"]).unwrap();
        validate(&cli).unwrap();
        let cli = parse(&["zg", "auth"]).unwrap();
        assert!(validate(&cli).is_err());
    }

    #[test]
    fn server_shape_errors_are_verbatim() {
        let cli = parse(&["zg", "server", "run", "--stdio"]).unwrap();
        let error = validate(&cli).expect_err("run + --stdio must fail");
        assert_eq!(
            error.to_string(),
            "--stdio cannot be combined with a server action"
        );
        let cli = parse(&["zg", "server"]).unwrap();
        let error = validate(&cli).expect_err("bare server must fail");
        assert_eq!(
            error.to_string(),
            "zg server requires on, off, status, run, or --stdio"
        );
    }

    #[test]
    fn config_and_index_shapes() {
        let cli = parse(&["zg", "config", "provider", "set", "qwen"]).unwrap();
        let error = validate(&cli).expect_err("missing --api-key must fail");
        assert_eq!(
            error.to_string(),
            "zg config provider set requires --api-key"
        );
        let cli = parse(&["zg", "index", "a", "b"]).unwrap();
        let error = validate(&cli).expect_err("two roots must fail");
        assert_eq!(error.to_string(), "zg index accepts at most one root path");
        let cli = parse(&["zg", "index", "--drop", "--rebuild"]).unwrap();
        let error = validate(&cli).expect_err("drop + rebuild must fail");
        assert_eq!(
            error.to_string(),
            "zg index --drop cannot be combined with indexing options"
        );
    }

    #[test]
    fn byte_sizes_and_times_parse() {
        assert_eq!(parse_byte_size("512").unwrap(), 512);
        assert_eq!(parse_byte_size("10MB").unwrap(), 10 * 1024 * 1024);
        assert_eq!(parse_byte_size("2g").unwrap(), 2 * 1024 * 1024 * 1024);
        assert!(parse_byte_size("nope").is_err());
        assert_eq!(parse_modified_time("0", "--x").unwrap(), 0);
        assert!(parse_modified_time("not-a-date", "--x").is_err());
    }

    #[test]
    fn target_splitting() {
        assert_eq!(
            split_targets(&["claude, codex".to_owned(), "qwen".to_owned()]),
            vec!["claude", "codex", "qwen"]
        );
    }
}
