//! MCP input normalization: validated search/rg inputs from wire structs.
//!
//! Mirrors `../zvec-grep/src/mcp/input-normalization.ts`
//! (`normalizeSearchInput`, `contextOptionsFromRgInput`) and folds in the
//! managed-rg command parser from `../zvec-grep/src/cli/managed-rg.ts`
//! (the MCP layer is its only consumer). The real-rg `extraArgs`
//! surface (invert, multiline, engines, threads, encodings) is rejected:
//! the port searches in-process and cannot forward raw ripgrep flags
//! (see `docs/ts-divergence.md`).

use std::path::{Path, PathBuf};

use chrono::{Local, TimeZone};
use zg_core::types::{CodeSymbolType as CoreSymbolType, UnixMillis};

use crate::backend::{RgQuery, SearchFreshness, SearchQuery, SearchRoute, SearchRouteMode};
use crate::mcp::error::McpError;
use crate::mcp::schemas::{
    FreshnessInput, PathFilter, QueryText, RgInput, SearchInput, SearchLimit, StringOrList,
    TimeInput, bound_groups, bound_path_filters, parse_root,
};
use crate::root_runtime::RootKey;

/// Fully validated search input: the MCP boundary shape of
/// `NormalizedSearchInput`.
#[derive(Debug, Clone)]
pub struct NormalizedSearchInput {
    /// Validated absolute root.
    pub root: RootKey,
    /// Combined primary groups (`query` + `queries`).
    pub queries: Vec<QueryText>,
    /// Supplemental routes (`fts` → fts, `vector` → vector).
    pub routes: Vec<NormalizedRoute>,
    /// Collapse all groups into one ranked plan.
    pub fuse: bool,
    /// Validated result limit.
    pub limit: Option<SearchLimit>,
    /// Include per-hit trace payloads.
    pub trace: bool,
    /// Prefer exact indexed symbols.
    pub prefer_symbol: bool,
    /// Symbol-type restrictions.
    pub symbol_types: Vec<CoreSymbolType>,
    /// Case-sensitive glob rules.
    pub globs: Vec<PathFilter>,
    /// Case-insensitive glob rules.
    pub insensitive_globs: Vec<PathFilter>,
    /// File types to include.
    pub file_types: Vec<PathFilter>,
    /// File types to exclude.
    pub excluded_file_types: Vec<PathFilter>,
    /// Include hidden paths.
    pub hidden: bool,
    /// Ignore ignore-files.
    pub no_ignore: bool,
    /// Additional ignore files.
    pub ignore_files: Vec<PathFilter>,
    /// Maximum directory depth.
    pub max_depth: Option<u32>,
    /// Maximum indexed file size.
    pub max_file_size_bytes: Option<u32>,
    /// Follow symlinks.
    pub follow: bool,
    /// Embedding concurrency for updates.
    pub embedding_concurrency: Option<u32>,
    /// Lower file-mtime bound.
    pub modified_after: Option<UnixMillis>,
    /// Upper file-mtime bound.
    pub modified_before: Option<UnixMillis>,
    /// Requested freshness.
    pub freshness: SearchFreshness,
    /// Background refresh allowed.
    pub auto_update: bool,
}

/// One supplemental retrieval route with validated query text.
#[derive(Debug, Clone)]
pub struct NormalizedRoute {
    /// Retrieval mode.
    pub mode: SearchRouteMode,
    /// Route query.
    pub query: QueryText,
}

impl NormalizedSearchInput {
    /// Maps onto the backend actor query.
    pub fn into_backend_query(self) -> SearchQuery {
        SearchQuery {
            query: None,
            queries: self
                .queries
                .iter()
                .map(|query| query.as_str().to_owned())
                .collect(),
            routes: self
                .routes
                .iter()
                .map(|route| SearchRoute {
                    mode: route.mode,
                    query: route.query.as_str().to_owned(),
                })
                .collect(),
            fuse: self.fuse,
            limit: self.limit.map(SearchLimit::get),
            trace: self.trace,
            prefer_symbol: self.prefer_symbol,
            symbol_types: self.symbol_types,
            globs: as_strings(&self.globs),
            insensitive_globs: as_strings(&self.insensitive_globs),
            file_types: as_strings(&self.file_types),
            excluded_file_types: as_strings(&self.excluded_file_types),
            modified_after: self.modified_after,
            modified_before: self.modified_before,
            freshness: self.freshness,
            auto_update: self.auto_update,
        }
    }
}

fn as_strings(filters: &[PathFilter]) -> Vec<String> {
    filters
        .iter()
        .map(|filter| filter.as_str().to_owned())
        .collect()
}

/// Normalizes a wire search input, mirroring `normalizeSearchInput`.
///
/// # Errors
///
/// Returns [`McpError::InvalidParams`] when no query group is supplied, or when the root,
/// queries, limits, path filters, or modified times fail validation.
pub fn normalize_search_input(input: &SearchInput) -> Result<NormalizedSearchInput, McpError> {
    let mut queries = normalize_query_list(input.query.as_ref(), "query")?;
    queries.extend(normalize_query_list(input.queries.as_ref(), "queries")?);
    let mut routes = Vec::new();
    for query in normalize_query_list(input.fts.as_ref(), "fts")? {
        routes.push(NormalizedRoute {
            mode: SearchRouteMode::Fts,
            query,
        });
    }
    for query in normalize_query_list(input.vector.as_ref(), "vector")? {
        routes.push(NormalizedRoute {
            mode: SearchRouteMode::Vector,
            query,
        });
    }
    if queries.is_empty() && routes.is_empty() {
        return Err(McpError::invalid_params(
            "zvec_grep_search requires query, queries, fts, or vector.",
        ));
    }
    if input.symbol_types.len() > 6 {
        return Err(McpError::invalid_params("symbolTypes exceeds 6 entries."));
    }
    Ok(NormalizedSearchInput {
        root: parse_root(&input.root)?,
        queries,
        routes,
        fuse: input.fuse.unwrap_or(false),
        limit: input.limit.map(SearchLimit::parse).transpose()?,
        trace: input.trace.unwrap_or(false),
        prefer_symbol: input.prefer_symbol.unwrap_or(false),
        symbol_types: input
            .symbol_types
            .iter()
            .map(|kind| (*kind).into())
            .collect(),
        globs: normalize_path_filters(input.globs.as_ref())?,
        insensitive_globs: normalize_path_filters(input.insensitive_globs.as_ref())?,
        file_types: normalize_path_filters(input.file_types.as_ref())?,
        excluded_file_types: normalize_path_filters(input.excluded_file_types.as_ref())?,
        hidden: input.hidden.unwrap_or(false),
        no_ignore: input.no_ignore.unwrap_or(false),
        ignore_files: normalize_path_filters(input.ignore_files.as_ref())?,
        max_depth: input.max_depth,
        max_file_size_bytes: input.max_file_size_bytes,
        follow: input.follow.unwrap_or(false),
        embedding_concurrency: input.embedding_concurrency,
        modified_after: input
            .modified_after
            .as_ref()
            .map(|value| parse_modified_time(value, "modifiedAfter"))
            .transpose()?,
        modified_before: input
            .modified_before
            .as_ref()
            .map(|value| parse_modified_time(value, "modifiedBefore"))
            .transpose()?,
        freshness: match input.freshness {
            FreshnessInput::Eventual => SearchFreshness::Eventual,
            FreshnessInput::WaitForFresh => SearchFreshness::WaitForFresh,
        },
        auto_update: input.auto_update,
    })
}

/// Normalizes one string-or-list query field: bound-checks each raw value
/// (mirroring the zod `.max` on the raw string), trims, and drops empties.
fn normalize_query_list(
    value: Option<&StringOrList>,
    what: &str,
) -> Result<Vec<QueryText>, McpError> {
    let mut items = Vec::new();
    for raw in flatten_list(value) {
        let parsed = QueryText::parse(raw)?;
        let trimmed = parsed.as_str().trim();
        if !trimmed.is_empty() {
            items.push(QueryText::parse(trimmed.to_owned())?);
        }
    }
    bound_groups(items, what)
}

/// Normalizes one path-filter field: a single string is one filter (no
/// splitting — mirroring `normalizePlainStringList`), arrays are bounded.
fn normalize_path_filters(value: Option<&StringOrList>) -> Result<Vec<PathFilter>, McpError> {
    let mut items = Vec::new();
    for raw in flatten_list(value) {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            items.push(PathFilter::parse(trimmed.to_owned())?);
        }
    }
    bound_path_filters(items)
}

fn flatten_list(value: Option<&StringOrList>) -> Vec<String> {
    match value {
        None => Vec::new(),
        Some(StringOrList::Single(one)) => vec![one.clone()],
        Some(StringOrList::Multiple(many)) => many.clone(),
    }
}

/// Parses epoch millis or a date string, mirroring TS `parseModifiedTime`
/// (digits → millis, `YYYY-MM-DD` → local midnight, RFC 3339 and
/// `YYYY-MM-DD HH:MM:SS` → instant). Anything else errors with the TS
/// message.
///
/// # Errors
///
/// Returns [`McpError::InvalidParams`] when the value is negative or unparseable.
pub fn parse_modified_time(value: &TimeInput, option: &str) -> Result<UnixMillis, McpError> {
    match value {
        TimeInput::Millis(millis) => {
            if *millis < 0 {
                return Err(invalid_time(option));
            }
            Ok(UnixMillis::from_millis(*millis))
        }
        TimeInput::Text(text) => {
            let trimmed = text.trim();
            if !trimmed.is_empty() && trimmed.chars().all(|char| char.is_ascii_digit()) {
                let millis: i64 = trimmed.parse().map_err(|_| invalid_time(option))?;
                if millis < 0 {
                    return Err(invalid_time(option));
                }
                return Ok(UnixMillis::from_millis(millis));
            }
            if let Ok(date) = chrono::NaiveDate::parse_from_str(trimmed, "%Y-%m-%d")
                && let Some(midnight) = date.and_hms_opt(0, 0, 0)
                && let Some(local) = Local.from_local_datetime(&midnight).single()
            {
                return Ok(UnixMillis::from_millis(local.timestamp_millis()));
            }
            if let Ok(instant) = chrono::DateTime::parse_from_rfc3339(trimmed) {
                return Ok(UnixMillis::from_millis(instant.timestamp_millis()));
            }
            for format in ["%Y-%m-%dT%H:%M:%S", "%Y-%m-%d %H:%M:%S"] {
                if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(trimmed, format)
                    && let Some(local) = Local.from_local_datetime(&naive).single()
                {
                    return Ok(UnixMillis::from_millis(local.timestamp_millis()));
                }
            }
            Err(invalid_time(option))
        }
    }
}

fn invalid_time(option: &str) -> McpError {
    McpError::invalid_params(format!(
        "{option} requires an epoch millisecond value or a parseable date"
    ))
}

/// Parses an `rg` command string into a backend lexical query, mirroring
/// `parseManagedRgCommand` + `contextOptionsFromRgInput`.
///
/// # Errors
///
/// Returns [`McpError::InvalidParams`] when the root is invalid, the rg command is
/// malformed or unsupported, or a path escapes the root.
pub fn rg_query_from_input(input: &RgInput) -> Result<(RootKey, RgQuery), McpError> {
    let root = parse_root(&input.root)?;
    let parsed = parse_managed_rg_command(&input.command)?;
    for path in parsed
        .paths
        .iter()
        .chain(parsed.ignore_files.iter())
        .chain(parsed.pattern_files.iter())
    {
        assert_root_scoped(root.as_str(), path)?;
    }
    Ok((
        root,
        RgQuery {
            patterns: parsed.patterns,
            paths: parsed.paths,
            limit: parsed.limit,
            fixed_strings: parsed.fixed_strings,
            ignore_case: parsed.ignore_case,
            max_count: parsed.max_count,
            pattern_files: parsed.pattern_files,
            globs: parsed.globs,
            insensitive_globs: parsed.insensitive_globs,
            file_types: parsed.file_types,
            excluded_file_types: parsed.excluded_file_types,
            hidden: parsed.hidden,
            no_ignore: parsed.no_ignore,
            ignore_files: parsed.ignore_files,
            max_depth: parsed.max_depth,
            max_file_size_bytes: parsed.max_file_size_bytes,
            smart_case: parsed.smart_case,
            word_regexp: parsed.word_regexp,
            before_context: parsed.before_context,
            after_context: parsed.after_context,
        },
    ))
}

/// Parsed `rg` argv before root scoping.
#[derive(Default)]
struct ParsedRgCommand {
    patterns: Vec<String>,
    paths: Vec<String>,
    limit: Option<usize>,
    fixed_strings: bool,
    ignore_case: bool,
    smart_case: bool,
    word_regexp: bool,
    max_count: Option<usize>,
    pattern_files: Vec<String>,
    globs: Vec<String>,
    insensitive_globs: Vec<String>,
    file_types: Vec<String>,
    excluded_file_types: Vec<String>,
    hidden: bool,
    no_ignore: bool,
    ignore_files: Vec<String>,
    max_depth: Option<usize>,
    max_file_size_bytes: Option<u64>,
    before_context: usize,
    after_context: usize,
}

fn parse_managed_rg_command(command: &str) -> Result<ParsedRgCommand, McpError> {
    let tokens = scan_rg_command(command)?;
    let (argv, limit) = split_head_suffix(&tokens)?;
    let argv = apply_rg_compat(argv);
    if !argv.first().is_some_and(|first| first == "rg") {
        return Err(McpError::invalid_params(
            "rg command must start with \"rg\".",
        ));
    }
    if argv.len() == 1 {
        return Err(McpError::invalid_params("rg command requires a pattern."));
    }
    let rest = argv
        .get(1..)
        .ok_or_else(|| McpError::invalid_params("rg command requires a pattern."))?;
    let mut parsed = parse_rg_argv(rest)?;
    parsed.command.limit = limit;
    // Positional pattern unless `-e`/`-f` supplied (mirrors
    // `normalizeManagedRgInput`); patterns are trimmed and empties dropped.
    if parsed.command.patterns.is_empty() && parsed.command.pattern_files.is_empty() {
        let Some(first) = parsed.positionals().first().cloned() else {
            return Err(McpError::invalid_params(
                "rg command requires a non-empty pattern.",
            ));
        };
        parsed.command.patterns = vec![first];
        parsed.positional_start = 1;
    }
    parsed.command.patterns = std::mem::take(&mut parsed.command.patterns)
        .into_iter()
        .map(|pattern| pattern.trim().to_owned())
        .filter(|pattern| !pattern.is_empty())
        .collect();
    if parsed.command.patterns.is_empty() && parsed.command.pattern_files.is_empty() {
        return Err(McpError::invalid_params(
            "rg command requires a non-empty pattern.",
        ));
    }
    Ok(parsed.into_command())
}

/// Raw argv parse with positional tracking.
struct RawRgParse {
    command: ParsedRgCommand,
    positionals: Vec<String>,
    positional_start: usize,
}

impl RawRgParse {
    fn positionals(&self) -> &[String] {
        &self.positionals
    }

    fn into_command(mut self) -> ParsedRgCommand {
        let start = self.positional_start.min(self.positionals.len());
        self.command.paths = self.positionals.split_off(start);
        self.command
    }
}

fn parse_rg_argv(argv: &[String]) -> Result<RawRgParse, McpError> {
    let mut command = ParsedRgCommand::default();
    let mut positionals = Vec::new();
    let mut index = 0;
    let mut end_of_flags = false;
    while index < argv.len() {
        let Some(token) = argv.get(index) else {
            break;
        };
        if end_of_flags || !token.starts_with('-') || token == "-" {
            if token == "-" {
                return Err(McpError::invalid_params(
                    "rg command cannot read patterns from stdin.",
                ));
            }
            // `---x` tokens are patterns, never flags (mirrors the TS
            // `--` insertion for triple-hyphen tokens).
            positionals.push(token.clone());
            index += 1;
            continue;
        }
        if token == "--" {
            end_of_flags = true;
            index += 1;
            continue;
        }
        if let Some(name) = token.strip_prefix("--") {
            index = parse_long_flag(argv, index, name, &mut command)?;
            continue;
        }
        index = parse_short_group(argv, index, &mut command)?;
    }
    Ok(RawRgParse {
        command,
        positionals,
        positional_start: 0,
    })
}

/// Long flags mappable onto the in-process engine; anything else errors
/// with the TS messages.
fn parse_long_flag(
    argv: &[String],
    index: usize,
    name: &str,
    command: &mut ParsedRgCommand,
) -> Result<usize, McpError> {
    let (flag, inline) = match name.split_once('=') {
        Some((flag, value)) => (flag, Some(value)),
        None => (name, None),
    };
    // Value flags accept `--flag value` and `--flag=value`.
    let value_of = |flag: &str| -> Result<String, McpError> {
        if let Some(inline) = inline {
            return Ok(inline.to_owned());
        }
        argv.get(index + 1).cloned().ok_or_else(|| {
            McpError::invalid_params(format!("rg command option \"--{flag}\" requires a value."))
        })
    };
    // True when the value came from the next token (consume it too).
    let consumed_next = inline.is_none();
    match flag {
        "regexp" => {
            command.patterns.push(value_of("regexp")?);
            Ok(index + if consumed_next { 2 } else { 1 })
        }
        "file" => {
            let path = value_of("file")?;
            reject_stdin_pattern_file(&path)?;
            command.pattern_files.push(path);
            Ok(index + if consumed_next { 2 } else { 1 })
        }
        "glob" => {
            command.globs.push(value_of("glob")?);
            Ok(index + if consumed_next { 2 } else { 1 })
        }
        "iglob" => {
            command.insensitive_globs.push(value_of("iglob")?);
            Ok(index + if consumed_next { 2 } else { 1 })
        }
        "type" => {
            command.file_types.push(value_of("type")?);
            Ok(index + if consumed_next { 2 } else { 1 })
        }
        "type-not" => {
            command.excluded_file_types.push(value_of("type-not")?);
            Ok(index + if consumed_next { 2 } else { 1 })
        }
        "ignore-file" => {
            command.ignore_files.push(value_of("ignore-file")?);
            Ok(index + if consumed_next { 2 } else { 1 })
        }
        "max-count" => {
            command.max_count = Some(parse_non_negative("--max-count", &value_of("max-count")?)?);
            Ok(index + if consumed_next { 2 } else { 1 })
        }
        "max-depth" => {
            command.max_depth = Some(parse_non_negative("--max-depth", &value_of("max-depth")?)?);
            Ok(index + if consumed_next { 2 } else { 1 })
        }
        "max-filesize" => {
            command.max_file_size_bytes = Some(parse_byte_size(
                "--max-filesize",
                &value_of("max-filesize")?,
            )?);
            Ok(index + if consumed_next { 2 } else { 1 })
        }
        "after-context" => {
            command.after_context =
                parse_non_negative("--after-context", &value_of("after-context")?)?;
            Ok(index + if consumed_next { 2 } else { 1 })
        }
        "before-context" => {
            command.before_context =
                parse_non_negative("--before-context", &value_of("before-context")?)?;
            Ok(index + if consumed_next { 2 } else { 1 })
        }
        "context" => {
            let lines = parse_non_negative("--context", &value_of("context")?)?;
            command.before_context = lines;
            command.after_context = lines;
            Ok(index + if consumed_next { 2 } else { 1 })
        }
        "fixed-strings" => {
            reject_inline("fixed-strings", inline)?;
            command.fixed_strings = true;
            Ok(index + 1)
        }
        "no-fixed-strings" => {
            reject_inline("no-fixed-strings", inline)?;
            command.fixed_strings = false;
            Ok(index + 1)
        }
        "ignore-case" => {
            reject_inline("ignore-case", inline)?;
            command.ignore_case = true;
            Ok(index + 1)
        }
        "case-sensitive" => {
            reject_inline("case-sensitive", inline)?;
            command.ignore_case = false;
            command.smart_case = false;
            Ok(index + 1)
        }
        "smart-case" => {
            reject_inline("smart-case", inline)?;
            command.smart_case = true;
            Ok(index + 1)
        }
        "word-regexp" => {
            reject_inline("word-regexp", inline)?;
            command.word_regexp = true;
            Ok(index + 1)
        }
        "hidden" => {
            reject_inline("hidden", inline)?;
            command.hidden = true;
            Ok(index + 1)
        }
        "no-ignore" => {
            reject_inline("no-ignore", inline)?;
            command.no_ignore = true;
            Ok(index + 1)
        }
        "files-with-matches" => {
            // Stripped by the compat layer before parsing; reaching here
            // means it arrived quoted oddly — treat like `-l`.
            reject_inline("files-with-matches", inline)?;
            command.max_count = Some(1);
            Ok(index + 1)
        }
        "follow" => Err(McpError::invalid_params(
            "rg command option \"--follow\" is not supported by the MCP tool.",
        )),
        "recursive" | "line-number" | "with-filename" => {
            reject_inline(flag, inline)?;
            Ok(index + 1)
        }
        _ if is_rg_output_option(flag) => Err(McpError::invalid_params(format!(
            "--{flag} changes rg output and cannot be used with managed --rg"
        ))),
        _ if is_rg_engine_option(flag) => Err(McpError::invalid_params(format!(
            "rg command option \"--{flag}\" is not supported by the MCP tool."
        ))),
        _ => Err(McpError::invalid_params(format!(
            "Unsupported --rg option: --{flag}"
        ))),
    }
}

/// Short-flag groups (`-in`, `-em1`): booleans combine, value flags take
/// the inline remainder or the next token (mirrors
/// `readShortOptionValue`).
fn parse_short_group(
    argv: &[String],
    index: usize,
    command: &mut ParsedRgCommand,
) -> Result<usize, McpError> {
    let Some(token) = argv.get(index) else {
        return Err(McpError::invalid_params("rg command requires a pattern."));
    };
    let bytes = token.as_bytes();
    let mut offset = 1;
    while offset < bytes.len() {
        let Some(short_byte) = bytes.get(offset) else {
            break;
        };
        let short = *short_byte as char;
        let flag = format!("-{short}");
        match short {
            'n' | 'H' => {
                // Display no-ops (mirrors the TS compatibility marking).
                offset += 1;
            }
            'F' => {
                command.fixed_strings = true;
                offset += 1;
            }
            'i' => {
                command.ignore_case = true;
                offset += 1;
            }
            's' => {
                command.ignore_case = false;
                command.smart_case = false;
                offset += 1;
            }
            'S' => {
                command.smart_case = true;
                offset += 1;
            }
            'w' => {
                command.word_regexp = true;
                offset += 1;
            }
            'l' => {
                // Stripped by the compat layer; a group remainder means
                // `-l` arrived inside a group like `-il`.
                command.max_count = Some(1);
                offset += 1;
            }
            'L' => {
                return Err(McpError::invalid_params(
                    "rg command option \"--follow\" is not supported by the MCP tool.",
                ));
            }
            'e' | 'g' | 't' | 'T' | 'f' | 'm' | 'A' | 'B' | 'C' => {
                let inline = token[offset + 1..].to_owned();
                let consumed = usize::from(inline.is_empty());
                let value = if inline.is_empty() {
                    argv.get(index + 1).cloned().ok_or_else(|| {
                        McpError::invalid_params(format!(
                            "rg command option \"{flag}\" requires a value."
                        ))
                    })?
                } else {
                    inline
                };
                match short {
                    'e' => command.patterns.push(value),
                    'g' => command.globs.push(value),
                    't' => command.file_types.push(value),
                    'T' => command.excluded_file_types.push(value),
                    'f' => {
                        reject_stdin_pattern_file(&value)?;
                        command.pattern_files.push(value);
                    }
                    'm' => command.max_count = Some(parse_non_negative(&flag, &value)?),
                    'A' => command.after_context = parse_non_negative(&flag, &value)?,
                    'B' => command.before_context = parse_non_negative(&flag, &value)?,
                    _ => {
                        let lines = parse_non_negative(&flag, &value)?;
                        command.before_context = lines;
                        command.after_context = lines;
                    }
                }
                return Ok(index + 1 + consumed);
            }
            _ => {
                if is_short_output_option(short) {
                    return Err(McpError::invalid_params(format!(
                        "{flag} changes rg output and cannot be used with managed --rg"
                    )));
                }
                return Err(McpError::invalid_params(format!(
                    "Unsupported --rg option: {flag}"
                )));
            }
        }
    }
    Ok(index + 1)
}

fn reject_inline(flag: &str, inline: Option<&str>) -> Result<(), McpError> {
    if inline.is_some() {
        return Err(McpError::invalid_params(format!(
            "rg command option \"--{flag}\" takes no value."
        )));
    }
    Ok(())
}

fn reject_stdin_pattern_file(path: &str) -> Result<(), McpError> {
    if path == "-" {
        return Err(McpError::invalid_params(
            "rg command cannot read patterns from stdin.",
        ));
    }
    Ok(())
}

/// Output-changing options, mirroring `MANAGED_RG_OUTPUT_OPTIONS` (plus
/// the short aliases the TS short switch maps to output errors).
fn is_rg_output_option(flag: &str) -> bool {
    matches!(
        flag,
        "count"
            | "count-matches"
            | "files"
            | "files-with-matches"
            | "files-without-match"
            | "column"
            | "byte-offset"
            | "no-column"
            | "colors"
            | "color"
            | "context-separator"
            | "field-context-separator"
            | "field-match-separator"
            | "json"
            | "heading"
            | "no-heading"
            | "no-filename"
            | "no-line-number"
            | "only-matching"
            | "passthru"
            | "path-separator"
            | "quiet"
            | "pretty"
            | "replace"
            | "stats"
            | "trim"
            | "null"
            | "no-messages"
            | "vimgrep"
    )
}

/// Short aliases for output-changing options.
fn is_short_output_option(short: char) -> bool {
    matches!(short, 'c' | 'o' | 'O' | 'q' | 'p' | '0' | 'N' | 'P')
}

/// Real-rg engine/behavior flags accepted by TS (forwarded to the rg
/// binary) but unmappable onto the in-process engine.
fn is_rg_engine_option(flag: &str) -> bool {
    matches!(
        flag,
        "auto-hybrid-regex"
            | "binary"
            | "crlf"
            | "invert-match"
            | "line-regexp"
            | "mmap"
            | "multiline"
            | "multiline-dotall"
            | "no-crlf"
            | "no-ignore-dot"
            | "no-ignore-files"
            | "no-ignore-global"
            | "no-ignore-parent"
            | "no-ignore-vcs"
            | "no-config"
            | "no-mmap"
            | "no-multiline"
            | "no-search-zip"
            | "pcre2"
            | "one-file-system"
            | "search-zip"
            | "stop-on-nonmatch"
            | "text"
            | "unicode"
            | "no-unicode"
            | "glob-case-insensitive"
            | "dfa-size-limit"
            | "encoding"
            | "engine"
            | "max-columns"
            | "regex-size-limit"
            | "threads"
            | "hidden" // handled above; listed for exhaustiveness of `--no-` forms below
            | "no-hidden"
            | "unrestricted"
            | "no-messages"
            | "sort"
            | "sortr"
    )
}

fn parse_non_negative(option: &str, value: &str) -> Result<usize, McpError> {
    if value.is_empty() || !value.chars().all(|char| char.is_ascii_digit()) {
        return Err(McpError::invalid_params(format!(
            "{option} requires a non-negative integer"
        )));
    }
    value
        .parse::<usize>()
        .map_err(|_| McpError::invalid_params(format!("{option} requires a non-negative integer")))
}

/// Byte sizes with `K/M/G/T` suffixes, mirroring TS `parseByteSize`.
fn parse_byte_size(option: &str, value: &str) -> Result<u64, McpError> {
    let trimmed = value.trim();
    let (digits, suffix) = match trimmed.chars().last() {
        Some(last) if last.is_ascii_alphabetic() => (
            &trimmed[..trimmed.len() - 1],
            Some(last.to_ascii_uppercase()),
        ),
        _ => (trimmed, None),
    };
    let amount: u64 = digits.parse().map_err(|_| {
        McpError::invalid_params(format!(
            "{option} requires bytes or an integer K/M/G/T size"
        ))
    })?;
    let multiplier: u64 = match suffix {
        None => 1,
        Some('K') => 1024,
        Some('M') => 1024 * 1024,
        Some('G') => 1024 * 1024 * 1024,
        Some('T') => 1024 * 1024 * 1024 * 1024,
        Some(_) => {
            return Err(McpError::invalid_params(format!(
                "{option} requires bytes or an integer K/M/G/T size"
            )));
        }
    };
    amount
        .checked_mul(multiplier)
        .ok_or_else(|| McpError::invalid_params(format!("{option} is too large")))
}

/// Shell-ish tokenizer mirroring `scanManagedRgCommand`: single/double
/// quotes, backslash escapes (non-Windows behavior), `|`/`>` splitting,
/// and rejection of NUL, newlines, shell operators, and expansions.
fn scan_rg_command(command: &str) -> Result<Vec<String>, McpError> {
    let mut tokens = Vec::new();
    let mut token = String::new();
    let mut started = false;
    let mut quote: Option<char> = None;
    let mut escaping = false;
    let chars: Vec<char> = command.chars().collect();
    let mut index = 0;
    while index < chars.len() {
        let Some(char) = chars.get(index).copied() else {
            break;
        };
        if char == '\0' {
            return Err(McpError::invalid_params(
                "rg command cannot contain NUL characters.",
            ));
        }
        if char == '\n' || char == '\r' {
            return Err(McpError::invalid_params(
                "rg command must be a single command on one line.",
            ));
        }
        if escaping {
            token.push(char);
            started = true;
            escaping = false;
            index += 1;
            continue;
        }
        if quote == Some('\'') {
            if char == '\'' {
                quote = None;
            } else {
                token.push(char);
            }
            started = true;
            index += 1;
            continue;
        }
        if quote == Some('"') {
            if char == '"' {
                quote = None;
            } else if char == '\\' {
                let next = chars.get(index + 1).copied();
                match next {
                    None => {
                        return Err(McpError::invalid_params(
                            "rg command ends with an incomplete escape.",
                        ));
                    }
                    Some(next) if matches!(next, '"' | '\\' | '$' | '`') => {
                        token.push(next);
                        index += 1;
                    }
                    _ => token.push(char),
                }
            } else if char == '`' || starts_expansion(&chars, index) {
                return Err(McpError::invalid_params(
                    "rg command does not support shell expansion.",
                ));
            } else {
                token.push(char);
            }
            started = true;
            index += 1;
            continue;
        }
        if char.is_whitespace() {
            if started {
                tokens.push(std::mem::take(&mut token));
                started = false;
            }
            index += 1;
            continue;
        }
        if char == '\'' || char == '"' {
            quote = Some(char);
            started = true;
            index += 1;
            continue;
        }
        if char == '\\' {
            // Windows shells treat backslashes as separators; everywhere
            // else they escape (mirrors the TS platform branch).
            if cfg!(windows) {
                token.push(char);
            } else {
                escaping = true;
            }
            started = true;
            index += 1;
            continue;
        }
        if char == '|' || char == '>' {
            if started {
                tokens.push(std::mem::take(&mut token));
                started = false;
            }
            tokens.push(char.to_string());
            index += 1;
            continue;
        }
        if matches!(char, '&' | ';' | '<' | '(' | ')') {
            return Err(McpError::invalid_params(format!(
                "rg command does not support shell operator {char:?}."
            )));
        }
        if char == '`' || starts_expansion(&chars, index) {
            return Err(McpError::invalid_params(
                "rg command does not support shell expansion.",
            ));
        }
        token.push(char);
        started = true;
        index += 1;
    }
    if escaping {
        return Err(McpError::invalid_params(
            "rg command ends with an incomplete escape.",
        ));
    }
    if let Some(open) = quote {
        return Err(McpError::invalid_params(format!(
            "rg command has an unclosed {open} quote."
        )));
    }
    if started {
        tokens.push(token);
    }
    Ok(tokens)
}

fn starts_expansion(chars: &[char], index: usize) -> bool {
    chars.get(index).copied() == Some('$')
        && matches!(chars.get(index + 1).copied(), Some('(') | Some('{'))
}

/// Splits the trailing `| head` bound and the `2> /dev/null` red herring,
/// mirroring `normalizeManagedRgShellSuffix`.
fn split_head_suffix(tokens: &[String]) -> Result<(Vec<String>, Option<usize>), McpError> {
    let mut argv = tokens.to_vec();
    let mut limit = None;
    if let Some(pipe) = argv.iter().rposition(|token| token == "|") {
        // `split_off` keeps `[..pipe]` in place; the suffix (pipe
        // included) is validated as the head bound.
        let suffix = argv.split_off(pipe);
        let head_args = suffix.get(1..).ok_or_else(|| {
            McpError::invalid_params(
                "rg command only supports a trailing \"| head\", \"| head -N\", or \"| head -n N\" output bound.",
            )
        })?;
        limit = Some(parse_head_limit(head_args)?);
    }
    if matches!(argv.as_slice(), [.., a, b, c] if a.as_str() == "2" && b.as_str() == ">" && c.as_str() == "/dev/null")
    {
        argv.truncate(argv.len() - 3);
    }
    if argv.iter().any(|token| token == "|" || token == ">") {
        let operator = argv
            .iter()
            .find(|token| token.as_str() == "|" || token.as_str() == ">")
            .cloned()
            .unwrap_or_default();
        return Err(McpError::invalid_params(format!(
            "rg command does not support shell operator {operator:?}."
        )));
    }
    Ok((argv, limit))
}

/// `| head`, `| head -N`, `| head -n N` — the only supported suffix.
fn parse_head_limit(suffix: &[String]) -> Result<usize, McpError> {
    let unsupported = || {
        McpError::invalid_params(
            "rg command only supports a trailing \"| head\", \"| head -N\", or \"| head -n N\" output bound.",
        )
    };
    let raw = match suffix {
        [head] if head == "head" => "10".to_owned(),
        [head, count] if head == "head" && is_head_count(count) => count[1..].to_owned(),
        [head, flag, count] if head == "head" && flag == "-n" && is_digits(count) => count.clone(),
        _ => return Err(unsupported()),
    };
    let limit: usize = raw.parse().map_err(|_| {
        McpError::invalid_params("rg command head limit must be a positive safe integer.")
    })?;
    if limit == 0 {
        return Err(McpError::invalid_params(
            "rg command head limit must be a positive safe integer.",
        ));
    }
    Ok(limit)
}

fn is_head_count(token: &str) -> bool {
    token.starts_with('-')
        && token.len() > 1
        && token[1..].chars().all(|char| char.is_ascii_digit())
}

fn is_digits(token: &str) -> bool {
    !token.is_empty() && token.chars().all(|char| char.is_ascii_digit())
}

/// `-l`/`--files-with-matches` becomes `--max-count 1`, mirroring
/// `normalizeManagedRgCompatibilityTokens`.
fn apply_rg_compat(argv: Vec<String>) -> Vec<String> {
    let mut tokens = Vec::with_capacity(argv.len());
    let mut files_with_matches = false;
    for token in &argv {
        if token == "-l" || token == "--files-with-matches" {
            files_with_matches = true;
            continue;
        }
        if token.starts_with('-')
            && !token.starts_with("--")
            && token.contains('l')
            && token.len() > 1
        {
            let stripped: String = token.chars().filter(|char| *char != 'l').collect();
            if stripped != "-" {
                tokens.push(stripped);
            }
            files_with_matches = true;
            continue;
        }
        tokens.push(token.clone());
    }
    if files_with_matches && !tokens.is_empty() {
        tokens.insert(1, "--max-count".to_owned());
        tokens.insert(2, "1".to_owned());
    }
    tokens
}

/// Lexical path resolution against the root: rejects escapes (mirrors
/// `assertRootScopedPath` with the TS `{label}` messages).
fn assert_root_scoped(root: &str, path: &str) -> Result<(), McpError> {
    let resolved = join_root(root, path);
    if path_escapes(root, &resolved) {
        return Err(McpError::invalid_params(format!(
            "search path must stay within root: {path}"
        )));
    }
    let canonical = canonical_through_ancestors(&resolved);
    let canonical_root = canonical_through_ancestors(root);
    if path_escapes(
        &canonical_root.to_string_lossy(),
        &canonical.to_string_lossy(),
    ) {
        return Err(McpError::invalid_params(format!(
            "search path resolves outside root: {path}"
        )));
    }
    Ok(())
}

fn join_root(root: &str, path: &str) -> String {
    let joined = Path::new(root).join(path);
    normalize_lexical(&joined)
}

/// Lexical `..`/`.` normalization without touching the filesystem.
fn normalize_lexical(path: &Path) -> String {
    let mut parts: Vec<std::ffi::OsString> = Vec::new();
    for component in path.components() {
        use std::path::Component;
        match component {
            Component::ParentDir => {
                parts.pop();
            }
            Component::CurDir => {}
            other @ Component::Prefix(_)
            | other @ Component::RootDir
            | other @ Component::Normal(_) => parts.push(other.as_os_str().to_owned()),
        }
    }
    let mut normalized = PathBuf::new();
    normalized.extend(parts);
    normalized.to_string_lossy().into_owned()
}

fn path_escapes(root: &str, path: &str) -> bool {
    let root = root.trim_end_matches('/');
    if path == root {
        return false;
    }
    !path.starts_with('/') || !path.starts_with(&format!("{root}/"))
}

/// Canonicalizes through the nearest existing ancestor (mirrors
/// `resolveThroughExistingAncestor`).
fn canonical_through_ancestors(path: &str) -> PathBuf {
    let mut current = PathBuf::from(path);
    let mut missing = Vec::new();
    loop {
        match current.canonicalize() {
            Ok(canonical) => {
                let mut full = canonical;
                for segment in missing {
                    full.push(segment);
                }
                return full;
            }
            Err(_) => {
                let Some(parent) = current.parent().map(Path::to_path_buf) else {
                    return PathBuf::from(path);
                };
                let Some(name) = current.file_name().map(|name| name.to_owned()) else {
                    return PathBuf::from(path);
                };
                if parent == current {
                    return PathBuf::from(path);
                }
                missing.insert(0, name);
                current = parent;
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::mcp::schemas::{MCP_MAX_QUERY_CHARS, SearchInput};

    fn search(query: &str) -> SearchInput {
        SearchInput {
            root: "/repo".to_owned(),
            query: Some(StringOrList::Single(query.to_owned())),
            ..SearchInput::default()
        }
    }

    #[test]
    fn requires_a_query_group() {
        let error = normalize_search_input(&SearchInput {
            root: "/repo".to_owned(),
            ..SearchInput::default()
        })
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "zvec_grep_search requires query, queries, fts, or vector."
        );
    }

    #[test]
    fn trims_and_drops_empty_groups() {
        let mut input = search("  ");
        input.queries = Some(StringOrList::Multiple(vec![" ok ".to_owned()]));
        let normalized = normalize_search_input(&input).unwrap();
        assert_eq!(normalized.queries.len(), 1);
        assert_eq!(normalized.queries[0].as_str(), "ok");
    }

    #[test]
    fn rejects_overlong_queries() {
        let input = search(&"x".repeat(MCP_MAX_QUERY_CHARS + 1));
        assert!(normalize_search_input(&input).is_err());
    }

    #[test]
    fn parses_basic_rg_command() {
        let (root, query) = rg_query_from_input(&RgInput {
            root: "/repo".to_owned(),
            command: "rg -i --glob '*.rs' 'fn main' src | head -5".to_owned(),
        })
        .unwrap();
        assert_eq!(root.as_str(), "/repo");
        assert_eq!(query.patterns, vec!["fn main"]);
        assert_eq!(query.paths, vec!["src"]);
        assert_eq!(query.limit, Some(5));
        assert!(query.ignore_case);
        assert_eq!(query.globs, vec!["*.rs"]);
    }

    #[test]
    fn rg_rejects_shell_operators_and_unknown_flags() {
        let command = |command: &str| RgInput {
            root: "/repo".to_owned(),
            command: command.to_owned(),
        };
        assert!(rg_query_from_input(&command("rg foo && rm")).is_err());
        assert!(rg_query_from_input(&command("rg --threads 4 foo")).is_err());
        assert!(rg_query_from_input(&command("rg --json foo")).is_err());
        assert!(rg_query_from_input(&command("rg --follow foo")).is_err());
        assert!(rg_query_from_input(&command("grep foo")).is_err());
        assert!(rg_query_from_input(&command("rg")).is_err());
    }

    #[test]
    fn rg_rejects_paths_outside_root() {
        let input = RgInput {
            root: "/repo".to_owned(),
            command: "rg foo ../outside".to_owned(),
        };
        assert!(rg_query_from_input(&input).is_err());
    }

    #[test]
    fn parses_modified_times() {
        let millis = parse_modified_time(&TimeInput::Millis(10), "modifiedAfter").unwrap();
        assert_eq!(millis, UnixMillis::from_millis(10));
        assert!(parse_modified_time(&TimeInput::Millis(-1), "modifiedAfter").is_err());
        let dated = parse_modified_time(&TimeInput::Text("2024-01-02".to_owned()), "modifiedAfter")
            .unwrap();
        assert!(dated.as_millis() > 0);
        assert!(
            parse_modified_time(&TimeInput::Text("not a date".to_owned()), "modifiedAfter")
                .is_err()
        );
    }
}
