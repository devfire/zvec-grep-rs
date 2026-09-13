//! Shared validation bounds: frozen numeric limits, validated boundary
//! newtypes, and list/root helpers.

use crate::mcp::error::McpError;

/// Maximum hybrid-search groups per request.
pub const MCP_MAX_QUERY_GROUPS: usize = 32;
/// Maximum characters per query string.
pub const MCP_MAX_QUERY_CHARS: usize = 4_000;
/// Maximum path filters per list.
pub const MCP_MAX_PATH_FILTERS: usize = 128;
/// Maximum characters per path filter or root.
pub const MCP_MAX_PATH_CHARS: usize = 1_024;
/// Maximum items returned per query group.
pub const MCP_MAX_SEARCH_LIMIT: usize = 50;

/// Character-budget check shared by the boundary validators below: text
/// longer than `max` `char`s is rejected by the caller.
fn within_char_budget(text: &str, max: usize) -> bool {
    text.chars().count() <= max
}

/// Query text validated against [`MCP_MAX_QUERY_CHARS`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryText(String);

impl QueryText {
    /// Validates length; emptiness is filtered at normalization (TS trims
    /// and drops empty groups rather than rejecting them).
    ///
    /// # Errors
    ///
    /// Returns [`McpError::InvalidParams`] when the value exceeds the character budget.
    pub fn parse(value: String) -> Result<Self, McpError> {
        if !within_char_budget(&value, MCP_MAX_QUERY_CHARS) {
            return Err(McpError::invalid_params(format!(
                "Query exceeds {MCP_MAX_QUERY_CHARS} characters."
            )));
        }
        Ok(Self(value))
    }

    /// Borrowed text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Path filter validated against [`MCP_MAX_PATH_CHARS`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathFilter(String);

impl PathFilter {
    /// Validates length.
    ///
    /// # Errors
    ///
    /// Returns [`McpError::InvalidParams`] when the value exceeds the character budget.
    pub fn parse(value: String) -> Result<Self, McpError> {
        if !within_char_budget(&value, MCP_MAX_PATH_CHARS) {
            return Err(McpError::invalid_params(format!(
                "Path filter exceeds {MCP_MAX_PATH_CHARS} characters."
            )));
        }
        Ok(Self(value))
    }

    /// Borrowed text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Search result limit validated against [`MCP_MAX_SEARCH_LIMIT`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchLimit(usize);

impl SearchLimit {
    /// Validates positivity and the upper bound (mirrors zod
    /// `.positive().max(50)`).
    ///
    /// # Errors
    ///
    /// Returns [`McpError::InvalidParams`] when the value is zero or above the maximum.
    pub fn parse(value: usize) -> Result<Self, McpError> {
        if value == 0 || value > MCP_MAX_SEARCH_LIMIT {
            return Err(McpError::invalid_params(format!(
                "Limit must be between 1 and {MCP_MAX_SEARCH_LIMIT}."
            )));
        }
        Ok(Self(value))
    }

    /// Validated value.
    #[must_use]
    pub const fn get(self) -> usize {
        self.0
    }
}

/// Validates a group list against [`MCP_MAX_QUERY_GROUPS`].
///
/// # Errors
///
/// Returns [`McpError::InvalidParams`] when the list exceeds the group cap.
pub fn bound_groups<T>(items: Vec<T>, what: &str) -> Result<Vec<T>, McpError> {
    if items.len() > MCP_MAX_QUERY_GROUPS {
        return Err(McpError::invalid_params(format!(
            "{what} exceeds {MCP_MAX_QUERY_GROUPS} groups."
        )));
    }
    Ok(items)
}

/// Validates a path-filter list against [`MCP_MAX_PATH_FILTERS`].
///
/// # Errors
///
/// Returns [`McpError::InvalidParams`] when the list exceeds the filter cap.
pub fn bound_path_filters(items: Vec<PathFilter>) -> Result<Vec<PathFilter>, McpError> {
    if items.len() > MCP_MAX_PATH_FILTERS {
        return Err(McpError::invalid_params(format!(
            "Path filters exceed {MCP_MAX_PATH_FILTERS} entries."
        )));
    }
    Ok(items)
}

/// Parses an absolute root, mirroring `absoluteRootSchema` ("root is
/// required." / absolute-path refinement, 1024-char cap). Existence and
/// permissions are the backend's concern: like TS, validation here only
/// proves the path is absolute.
///
/// # Errors
///
/// Returns [`McpError::InvalidParams`] when the value is empty, over the character
/// cap, or not an absolute path.
pub fn parse_root(value: &str) -> Result<crate::root_runtime::RootKey, McpError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(McpError::invalid_params("root is required."));
    }
    if !within_char_budget(trimmed, MCP_MAX_PATH_CHARS) {
        return Err(McpError::invalid_params(format!(
            "root exceeds {MCP_MAX_PATH_CHARS} characters."
        )));
    }
    crate::root_runtime::RootKey::parse(trimmed)
        .map_err(|_| McpError::invalid_params("root must be an absolute path."))
}
