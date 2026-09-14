//! Validated search inputs: `NormalizedSearchInput` plus wire normalization
//! (query groups, path filters, modified times).


use chrono::{Local, TimeZone};
use zg_core::types::{CodeSymbolType as CoreSymbolType, UnixMillis};

use crate::backend::{SearchFreshness, SearchQuery, SearchRoute, SearchRouteMode};
use crate::mcp::error::McpError;
use crate::mcp::schemas::{
    FreshnessInput, PathFilter, QueryText, SearchInput, SearchLimit, StringOrList,
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
