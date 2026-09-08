//! MCP tool schemas: frozen numeric bounds, validated boundary newtypes,
//! and wire input/output structs.
//!
//! Mirrors `../zvec-grep/src/mcp/schemas.ts`. Per M2 the numeric bounds
//! are `const` values validated in one place: wire structs deserialize
//! loosely and convert into validated newtypes ([`QueryText`],
//! [`PathFilter`], [`SearchLimit`]) at the boundary, so a handler cannot
//! receive an out-of-range value. Output structs use the TS
//! snake_case/camelCase wire shapes verbatim (a wire fact per M3).

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use zg_core::types::CodeSymbolType as CoreSymbolType;

use crate::backend::DaemonIndexStatus;
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

/// Query text validated against [`MCP_MAX_QUERY_CHARS`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryText(String);

impl QueryText {
    /// Validates length; emptiness is filtered at normalization (TS trims
    /// and drops empty groups rather than rejecting them).
    pub fn parse(value: String) -> Result<Self, McpError> {
        if value.chars().count() > MCP_MAX_QUERY_CHARS {
            return Err(McpError::invalid_params(format!(
                "Query exceeds {MCP_MAX_QUERY_CHARS} characters."
            )));
        }
        Ok(Self(value))
    }

    /// Borrowed text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Path filter validated against [`MCP_MAX_PATH_CHARS`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathFilter(String);

impl PathFilter {
    /// Validates length.
    pub fn parse(value: String) -> Result<Self, McpError> {
        if value.chars().count() > MCP_MAX_PATH_CHARS {
            return Err(McpError::invalid_params(format!(
                "Path filter exceeds {MCP_MAX_PATH_CHARS} characters."
            )));
        }
        Ok(Self(value))
    }

    /// Borrowed text.
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
    pub fn parse(value: usize) -> Result<Self, McpError> {
        if value == 0 || value > MCP_MAX_SEARCH_LIMIT {
            return Err(McpError::invalid_params(format!(
                "Limit must be between 1 and {MCP_MAX_SEARCH_LIMIT}."
            )));
        }
        Ok(Self(value))
    }

    /// Validated value.
    pub const fn get(self) -> usize {
        self.0
    }
}

/// Validates a group list against [`MCP_MAX_QUERY_GROUPS`].
pub fn bound_groups<T>(items: Vec<T>, what: &str) -> Result<Vec<T>, McpError> {
    if items.len() > MCP_MAX_QUERY_GROUPS {
        return Err(McpError::invalid_params(format!(
            "{what} exceeds {MCP_MAX_QUERY_GROUPS} groups."
        )));
    }
    Ok(items)
}

/// Validates a path-filter list against [`MCP_MAX_PATH_FILTERS`].
pub fn bound_path_filters(items: Vec<PathFilter>) -> Result<Vec<PathFilter>, McpError> {
    if items.len() > MCP_MAX_PATH_FILTERS {
        return Err(McpError::invalid_params(format!(
            "Path filters exceed {MCP_MAX_PATH_FILTERS} entries."
        )));
    }
    Ok(items)
}

/// A string or a list of strings (mirrors `boundedStringList`).
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum StringOrList {
    /// Single value.
    Single(String),
    /// Multiple values.
    Multiple(Vec<String>),
}

/// Epoch millis or a parseable date (mirrors `timeInputSchema`).
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum TimeInput {
    /// Epoch milliseconds.
    Millis(i64),
    /// Date string parsed by [`crate::mcp::input_normalization::parse_modified_time`].
    Text(String),
}

/// Requested search freshness (mirrors the `"eventual" | "wait_for_fresh"`
/// enum, defaulting to eventual).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FreshnessInput {
    /// Search the committed index immediately.
    #[default]
    Eventual,
    /// Settle pending index work before searching.
    WaitForFresh,
}

/// Symbol-type restriction (mirrors `codeSymbolTypeSchema`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum CodeSymbolTypeInput {
    /// Module symbols.
    Module,
    /// Class symbols.
    Class,
    /// Interface symbols.
    Interface,
    /// Function symbols.
    Function,
    /// Value symbols.
    Value,
    /// Alias symbols.
    Alias,
}

impl From<CodeSymbolTypeInput> for CoreSymbolType {
    fn from(value: CodeSymbolTypeInput) -> Self {
        match value {
            CodeSymbolTypeInput::Module => Self::Module,
            CodeSymbolTypeInput::Class => Self::Class,
            CodeSymbolTypeInput::Interface => Self::Interface,
            CodeSymbolTypeInput::Function => Self::Function,
            CodeSymbolTypeInput::Value => Self::Value,
            CodeSymbolTypeInput::Alias => Self::Alias,
        }
    }
}

/// One-request local embedding device override (mirrors the `device`
/// enum). Accepted for wire compatibility and rejected when present:
/// per-request device overrides are not supported, mirroring the
/// credential rationale below.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum DeviceInput {
    /// Automatic selection.
    Auto,
    /// CPU execution.
    Cpu,
    /// Metal execution.
    Metal,
    /// Vulkan execution.
    Vulkan,
    /// CUDA execution.
    Cuda,
}

/// `zvec_grep_search` input (mirrors `zvecGrepSearchInputSchema`).
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SearchInput {
    /// Absolute workspace root visible to the daemon.
    pub root: String,
    /// One primary hybrid-search group.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(
        description = "One primary hybrid-search group using natural-language or exact terms."
    )]
    pub query: Option<StringOrList>,
    /// One or more primary hybrid-search groups.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(
        description = "One or more primary hybrid-search groups. Each group is searched separately and retains group metadata."
    )]
    pub queries: Option<StringOrList>,
    /// Supplemental lexical-route groups.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(
        description = "Supplemental lexical-route groups for exact anchors; retrieval routes, not hard constraints."
    )]
    pub fts: Option<StringOrList>,
    /// Supplemental semantic-route groups.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(
        description = "Supplemental semantic/vector-route groups; retrieval routes, not hard constraints."
    )]
    pub vector: Option<StringOrList>,
    /// Maximum returned items per query group.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(
        description = "Maximum returned items per query group, or for the single fused plan."
    )]
    pub limit: Option<usize>,
    /// Ordered case-sensitive rg-style glob rules.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub globs: Option<StringOrList>,
    /// Ordered case-insensitive rg-style glob rules.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub insensitive_globs: Option<StringOrList>,
    /// Ripgrep file type names to include.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_types: Option<StringOrList>,
    /// Ripgrep file type names to exclude.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub excluded_file_types: Option<StringOrList>,
    /// Include hidden paths.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hidden: Option<bool>,
    /// Do not respect ignore files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub no_ignore: Option<bool>,
    /// Additional ignore files relative to the root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ignore_files: Option<StringOrList>,
    /// Maximum recursive directory depth.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_depth: Option<u32>,
    /// Maximum indexed file size in bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_file_size_bytes: Option<u32>,
    /// Follow symbolic links.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follow: Option<bool>,
    /// Embedding requests processed concurrently during updates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding_concurrency: Option<u32>,
    /// Collapse groups into one ranked search plan.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fuse: Option<bool>,
    /// Prefer exact indexed symbols when the query names a symbol.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefer_symbol: Option<bool>,
    /// Restrict indexed results to symbol types.
    #[serde(default)]
    pub symbol_types: Vec<CodeSymbolTypeInput>,
    /// Only query files modified after this time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified_after: Option<TimeInput>,
    /// Only query files modified before this time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified_before: Option<TimeInput>,
    /// Include per-hit search trace in structured output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace: Option<bool>,
    /// Whether to search immediately or wait for a fresh index.
    #[serde(default)]
    pub freshness: FreshnessInput,
    /// Whether an eventual search may schedule a background index update.
    #[serde(default = "default_auto_update")]
    pub auto_update: bool,
}

/// TS `autoUpdate` defaults to true.
const fn default_auto_update() -> bool {
    true
}

/// `zvec_grep_index` input (mirrors `zvecGrepIndexInputSchema`).
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct IndexInput {
    /// Absolute workspace root visible to the daemon.
    pub root: String,
    /// Permanently remove the workspace index.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drop: Option<bool>,
    /// Embedding model reference for a new index.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding: Option<String>,
    /// Explicitly rebuild the existing index.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rebuild: Option<bool>,
    /// Replace the index root-path configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reset_paths: Option<bool>,
    /// Ordered case-sensitive glob rules for indexed files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub globs: Option<StringOrList>,
    /// Ordered case-insensitive glob rules for indexed files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub insensitive_globs: Option<StringOrList>,
    /// Ripgrep file type names to include.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_types: Option<StringOrList>,
    /// Ripgrep file type names to exclude.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub excluded_file_types: Option<StringOrList>,
    /// Include hidden paths.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hidden: Option<bool>,
    /// Do not respect ignore files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub no_ignore: Option<bool>,
    /// Additional ignore files relative to the root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ignore_files: Option<StringOrList>,
    /// Maximum recursive directory depth.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_depth: Option<u32>,
    /// Maximum indexed file size in bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_file_size_bytes: Option<u32>,
    /// Follow symbolic links.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follow: Option<bool>,
    /// Embedding requests processed concurrently.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding_concurrency: Option<u32>,
    /// Return skipped-file diagnostics after a completed index job.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub debug: Option<bool>,
    /// Wait for the submitted index job to finish.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait: Option<bool>,
    /// One-request API key override. Rejected when present: credentials
    /// must come from the daemon configuration, never the wire.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// One-request device override. Rejected when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<DeviceInput>,
    /// Remote embedding endpoint override. Rejected when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
}

/// `zvec_grep_index_drop` input.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct IndexDropInput {
    /// Absolute workspace root visible to the daemon.
    pub root: String,
}

/// `zvec_grep_index_status` input.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct IndexStatusInput {
    /// Absolute workspace root visible to the daemon.
    pub root: String,
}

/// `zvec_grep_server_status` input (empty object).
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
pub struct ServerStatusInput {}

/// `zvec_grep_rg` input (mirrors `zvecGrepRgInputSchema`).
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RgInput {
    /// Absolute workspace root visible to the daemon.
    pub root: String,
    /// The command MUST start with `rg`; it is parsed as arguments and
    /// never executed by a shell.
    pub command: String,
}

/// Index job lifecycle state (mirrors the TS state strings).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum JobStateOutput {
    /// Job is queued.
    Queued,
    /// Job is running.
    Running,
    /// Job succeeded.
    Succeeded,
    /// Job failed.
    Failed,
    /// Job was cancelled.
    Cancelled,
}

impl From<zg_core::index_status::IndexJobState> for JobStateOutput {
    fn from(state: zg_core::index_status::IndexJobState) -> Self {
        match state {
            zg_core::index_status::IndexJobState::Queued => Self::Queued,
            zg_core::index_status::IndexJobState::Running => Self::Running,
            zg_core::index_status::IndexJobState::Succeeded => Self::Succeeded,
            zg_core::index_status::IndexJobState::Failed => Self::Failed,
            zg_core::index_status::IndexJobState::Cancelled => Self::Cancelled,
        }
    }
}

/// Terminal job error payload (mirrors `jobErrorSchema`).
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct JobErrorOutput {
    /// Frozen error code.
    pub code: String,
    /// One-line message.
    pub message: String,
    /// Redacted engine context, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    /// Redacted cause chain, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cause: Option<String>,
}

impl From<&crate::job_scheduler::IndexJobError> for JobErrorOutput {
    fn from(error: &crate::job_scheduler::IndexJobError) -> Self {
        Self {
            code: error.code.clone(),
            message: error.message.clone(),
            context: error.context.clone(),
            cause: error.cause.clone(),
        }
    }
}

/// One skipped-file sample (mirrors the TS camelCase sample shape).
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SkippedFileSampleOutput {
    /// Absolute path.
    pub absolute_path: String,
    /// Root-relative path.
    pub relative_path: String,
    /// Skip reason (`empty` | `too_large` | `unsupported` | `binary`).
    pub reason: String,
    /// File size, when measured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    /// Limit that excluded the file, when applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit_bytes: Option<u64>,
}

/// Scan diagnostics payload (mirrors the TS `scan_diagnostics` shape).
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ScanDiagnosticsOutput {
    /// Files skipped during the scan.
    #[serde(rename = "skippedFiles")]
    pub skipped_files: usize,
    /// Skip counts by snake_case reason.
    #[serde(rename = "skippedByReason")]
    pub skipped_by_reason: BTreeMap<String, usize>,
    /// Bounded skip samples.
    #[serde(rename = "skippedSamples")]
    pub skipped_samples: Vec<SkippedFileSampleOutput>,
}

impl From<&zg_core::types::FileScanDiagnostics> for ScanDiagnosticsOutput {
    fn from(diagnostics: &zg_core::types::FileScanDiagnostics) -> Self {
        Self {
            skipped_files: diagnostics.skipped_files,
            skipped_by_reason: diagnostics
                .skipped_by_reason
                .iter()
                .map(|(reason, count)| (skipped_reason_name(*reason).to_owned(), *count))
                .collect(),
            skipped_samples: diagnostics
                .skipped_samples
                .iter()
                .map(|sample| SkippedFileSampleOutput {
                    absolute_path: sample.absolute_path.clone(),
                    relative_path: sample.relative_path.clone(),
                    reason: skipped_reason_name(sample.reason).to_owned(),
                    size_bytes: sample.size_bytes,
                    limit_bytes: sample.limit_bytes,
                })
                .collect(),
        }
    }
}

/// Snake_case reason name matching the TS `reason` enum.
const fn skipped_reason_name(reason: zg_core::types::SkippedFileReason) -> &'static str {
    match reason {
        zg_core::types::SkippedFileReason::Empty => "empty",
        zg_core::types::SkippedFileReason::TooLarge => "too_large",
        zg_core::types::SkippedFileReason::Unsupported => "unsupported",
        zg_core::types::SkippedFileReason::Binary => "binary",
    }
}

/// Index action taken (mirrors `"index" | "drop"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum IndexActionOutput {
    /// Index was created or updated.
    Index,
    /// Index was dropped.
    Drop,
}

/// `zvec_grep_index` structured output.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct IndexOutput {
    /// Indexed root.
    pub root: String,
    /// Submitted job id.
    #[serde(rename = "job_id")]
    pub job_id: String,
    /// Job state at return time.
    pub state: JobStateOutput,
    /// True when an existing live job was reused.
    pub reused: bool,
    /// Action taken, when the request selected one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<IndexActionOutput>,
    /// True when the request dropped the index.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dropped: Option<bool>,
    /// Terminal job error, when failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<JobErrorOutput>,
    /// Skipped-file diagnostics (with `debug` after a completed job).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scan_diagnostics: Option<ScanDiagnosticsOutput>,
}

/// `zvec_grep_index_drop` structured output.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct IndexDropOutput {
    /// Indexed root.
    pub root: String,
    /// True when storage existed and was removed.
    pub removed: bool,
}

/// Index policy (mirrors `"enabled" | "disabled" | "undecided"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum IndexPolicyOutput {
    /// Indexing enabled.
    Enabled,
    /// Indexing disabled.
    Disabled,
    /// No policy recorded.
    Undecided,
}

impl From<Option<zg_core::types::WorkspaceIndexPolicy>> for IndexPolicyOutput {
    fn from(policy: Option<zg_core::types::WorkspaceIndexPolicy>) -> Self {
        match policy {
            Some(zg_core::types::WorkspaceIndexPolicy::Enabled) => Self::Enabled,
            Some(zg_core::types::WorkspaceIndexPolicy::Disabled) => Self::Disabled,
            None => Self::Undecided,
        }
    }
}

/// Index source (mirrors `"index" | "unindexed"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum IndexSourceOutput {
    /// Results come from an index.
    Index,
    /// No index exists.
    Unindexed,
}

/// One root-path entry (mirrors the TS snake_case `root_paths` items).
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct RootPathOutput {
    /// Absolute path.
    pub absolute_path: String,
    /// Whether the path is scanned recursively.
    pub recursive: bool,
    /// Include rules, when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include: Option<Vec<String>>,
    /// Exclude rules, when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclude: Option<Vec<String>>,
    /// Case-sensitive glob rules, when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub globs: Option<Vec<String>>,
    /// Case-insensitive glob rules, when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub insensitive_globs: Option<Vec<String>>,
    /// File types to include, when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_types: Option<Vec<String>>,
    /// File types to exclude, when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub excluded_file_types: Option<Vec<String>>,
    /// Include hidden paths, when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hidden: Option<bool>,
    /// Ignore-files disabled, when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub no_ignore: Option<bool>,
    /// Additional ignore files, when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ignore_files: Option<Vec<String>>,
    /// Maximum directory depth, when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_depth: Option<u32>,
    /// Maximum file size, when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_file_size_bytes: Option<u64>,
    /// Follow symlinks, when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follow: Option<bool>,
}

impl From<&zg_core::types::RootPath> for RootPathOutput {
    fn from(path: &zg_core::types::RootPath) -> Self {
        Self {
            absolute_path: path.absolute_path.clone(),
            recursive: path.recursive,
            include: nonempty(path.include.clone()),
            exclude: nonempty(path.exclude.clone()),
            globs: nonempty(path.globs.clone()),
            insensitive_globs: nonempty(path.insensitive_globs.clone()),
            file_types: nonempty(path.file_types.clone()),
            excluded_file_types: nonempty(path.excluded_file_types.clone()),
            hidden: path.hidden,
            no_ignore: path.no_ignore,
            ignore_files: nonempty(path.ignore_files.clone()),
            max_depth: path.max_depth,
            max_file_size_bytes: path.max_file_size_bytes,
            follow: path.follow,
        }
    }
}

fn nonempty(items: Vec<String>) -> Option<Vec<String>> {
    if items.is_empty() { None } else { Some(items) }
}

/// Embedding identity in status output.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct StatusEmbeddingOutput {
    /// Provider name.
    pub provider: String,
    /// Model reference.
    pub model: String,
    /// Vector dimension.
    pub dimension: usize,
    /// Distance metric.
    pub metric: String,
}

impl From<&zg_core::service::types::EmbeddingInfo> for StatusEmbeddingOutput {
    fn from(embedding: &zg_core::service::types::EmbeddingInfo) -> Self {
        Self {
            provider: embedding.provider.clone(),
            model: embedding.model.clone(),
            dimension: embedding.dimension,
            metric: format!("{:?}", embedding.metric).to_lowercase(),
        }
    }
}

/// Workspace index identity in status output.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct StatusWorkspaceIndexOutput {
    /// Index id.
    pub id: String,
    /// Index name.
    pub name: String,
    /// Storage path.
    pub path: String,
    /// Configured root paths.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub root_paths: Vec<RootPathOutput>,
    /// Embedding identity, when recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding: Option<StatusEmbeddingOutput>,
    /// Index version, when recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index_version: Option<i64>,
    /// Creation time, epoch millis.
    pub created_time: i64,
    /// Update time, epoch millis.
    pub updated_time: i64,
}

/// File counters in status output.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct StatusFilesOutput {
    /// Files stored.
    pub stored: usize,
    /// Files scanned.
    pub scanned: usize,
    /// Entities indexed.
    pub indexed: usize,
    /// Files pending.
    pub pending: usize,
    /// Files failed.
    pub failed: usize,
    /// Files added.
    pub added: usize,
    /// Files modified.
    pub modified: usize,
    /// Files deleted.
    pub deleted: usize,
    /// Files unchanged.
    pub unchanged: usize,
    /// Entities indexed.
    pub entities: usize,
    /// Truncated fragments.
    pub truncated_fragments: usize,
}

impl From<&zg_core::types::WorkspaceIndexStatus> for StatusFilesOutput {
    fn from(status: &zg_core::types::WorkspaceIndexStatus) -> Self {
        Self {
            stored: status.files_stored,
            scanned: status.files_scanned,
            indexed: status.entities_indexed,
            pending: status.files_pending,
            failed: status.files_failed,
            added: status.files_added,
            modified: status.files_modified,
            deleted: status.files_deleted,
            unchanged: status.files_unchanged,
            entities: status.entities_indexed,
            truncated_fragments: status.fragments_truncated,
        }
    }
}

/// Persistent block of the status output.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct PersistentStatusOutput {
    /// Daemon home directory.
    pub home: String,
    /// Workspace index storage path (empty when unindexed).
    pub index_path: String,
    /// Manifest identity, when an index exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_index: Option<StatusWorkspaceIndexOutput>,
    /// File counters, when an index exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files: Option<StatusFilesOutput>,
    /// Suggested next action, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<String>,
}

/// Live progress in status output.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct StatusProgressOutput {
    /// Lifecycle phase.
    pub phase: String,
    /// Files in scope, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files_total: Option<usize>,
    /// Files indexed, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files_indexed: Option<usize>,
    /// Files failed, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files_failed: Option<usize>,
    /// Detail message, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl From<&zg_core::types::IndexProgress> for StatusProgressOutput {
    fn from(progress: &zg_core::types::IndexProgress) -> Self {
        Self {
            phase: progress
                .phase
                .map(|phase| format!("{phase:?}").to_lowercase())
                .unwrap_or_else(|| "indexing".to_owned()),
            files_total: progress.files_total,
            files_indexed: progress.files_indexed,
            files_failed: progress.files_failed,
            detail: progress.detail.clone(),
        }
    }
}

/// Completion counters in status output.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct StatusCompletionOutput {
    /// Up-to-date files.
    pub completed: usize,
    /// Total files in scope.
    pub total: usize,
}

/// Runtime block of the status output (mirrors `formatIndexStatus`).
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct RuntimeStatusOutput {
    /// Filesystem watcher is active.
    pub watcher_active: bool,
    /// Dirty revision counter.
    pub dirty_revision: u64,
    /// Newest indexed revision counter.
    pub indexed_revision: u64,
    /// Active job id, when a job is live.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_job_id: Option<String>,
    /// Active job state, when a job is live.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_state: Option<JobStateOutput>,
    /// Live progress, when reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<StatusProgressOutput>,
    /// Completion overlay, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion: Option<StatusCompletionOutput>,
    /// Terminal job error, when failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<JobErrorOutput>,
}

/// `zvec_grep_index_status` structured output.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct IndexStatusOutput {
    /// Status root.
    pub root: String,
    /// Whether an index covers the root.
    pub indexed: bool,
    /// Index policy.
    pub index_policy: IndexPolicyOutput,
    /// Result source.
    pub source: IndexSourceOutput,
    /// Persistent index state.
    pub persistent: PersistentStatusOutput,
    /// Live runtime state, when the daemon owns the root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<RuntimeStatusOutput>,
}

/// Model pool summary in server status output.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ServerModelsOutput {
    /// Resident models.
    pub loaded: usize,
    /// Active leases.
    pub active_leases: usize,
}

/// `zvec_grep_server_status` structured output.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ServerStatusOutput {
    /// Server version.
    pub version: String,
    /// Milliseconds since backend creation.
    pub uptime_ms: u64,
    /// Whether shutdown is underway.
    pub shutting_down: bool,
    /// Live root actors.
    pub active_runtimes: usize,
    /// Queued jobs.
    pub queued_jobs: usize,
    /// Running jobs.
    pub running_jobs: usize,
    /// Model pool summary.
    pub models: ServerModelsOutput,
}

/// Formats a daemon status into the MCP status output (mirrors TS
/// `formatIndexStatus`).
pub fn format_index_status(root: &str, status: &DaemonIndexStatus) -> IndexStatusOutput {
    let indexed = status.info.as_ref().is_some_and(|info| info.indexed);
    let job = status.job.as_ref();
    IndexStatusOutput {
        root: root.to_owned(),
        indexed,
        index_policy: status
            .info
            .as_ref()
            .and_then(|info| info.index_policy)
            .into(),
        source: if indexed {
            IndexSourceOutput::Index
        } else {
            IndexSourceOutput::Unindexed
        },
        persistent: PersistentStatusOutput {
            home: crate::logger::daemon_home().to_string_lossy().into_owned(),
            index_path: status
                .info
                .as_ref()
                .and_then(|info| info.workspace_index.as_ref())
                .map(|index| index.path.clone())
                .unwrap_or_default(),
            workspace_index: status
                .info
                .as_ref()
                .and_then(|info| info.workspace_index.as_ref())
                .map(|index| StatusWorkspaceIndexOutput {
                    id: index.id.clone(),
                    name: index.name.clone(),
                    path: index.path.clone(),
                    root_paths: index.root_paths.iter().map(RootPathOutput::from).collect(),
                    embedding: index.embedding.clone().flatten().as_ref().map(|schema| {
                        StatusEmbeddingOutput {
                            provider: schema.provider.clone(),
                            model: schema.model.clone(),
                            dimension: schema.dimension,
                            metric: format!("{:?}", schema.metric).to_lowercase(),
                        }
                    }),
                    index_version: index.index_version,
                    created_time: index.created_time.as_millis(),
                    updated_time: index.updated_time.as_millis(),
                }),
            files: Some(StatusFilesOutput::from(&status.status)),
            suggestion: status
                .info
                .as_ref()
                .and_then(|info| info.suggestion.clone()),
        },
        runtime: Some(RuntimeStatusOutput {
            watcher_active: status.watcher_active,
            dirty_revision: status.dirty_revision.get(),
            indexed_revision: status.indexed_revision.get(),
            active_job_id: job.map(|job| job.id.to_string()),
            job_state: job.map(|job| JobStateOutput::from(job.state)),
            progress: job
                .and_then(|job| job.progress.as_ref())
                .map(StatusProgressOutput::from),
            completion: status
                .completion
                .as_ref()
                .map(|completion| StatusCompletionOutput {
                    completed: completion.completed,
                    total: completion.total,
                }),
            error: job
                .and_then(|job| job.error.as_ref())
                .map(JobErrorOutput::from),
        }),
    }
}

/// Parses an absolute root, mirroring `absoluteRootSchema` ("root is
/// required." / absolute-path refinement, 1024-char cap). Existence and
/// permissions are the backend's concern: like TS, validation here only
/// proves the path is absolute.
pub fn parse_root(value: &str) -> Result<crate::root_runtime::RootKey, McpError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(McpError::invalid_params("root is required."));
    }
    if trimmed.chars().count() > MCP_MAX_PATH_CHARS {
        return Err(McpError::invalid_params(format!(
            "root exceeds {MCP_MAX_PATH_CHARS} characters."
        )));
    }
    crate::root_runtime::RootKey::parse(trimmed)
        .map_err(|_| McpError::invalid_params("root must be an absolute path."))
}

/// Builds the rmcp input schema object for a wire input type.
pub fn input_schema_for<T: JsonSchema>() -> std::sync::Arc<rmcp::model::JsonObject> {
    let schema = schemars::schema_for!(T);
    let value = serde_json::to_value(&schema).unwrap_or(serde_json::Value::Bool(true));
    let object = value.as_object().cloned().unwrap_or_default();
    std::sync::Arc::new(object)
}

/// Builds the rmcp output schema object for a wire output type.
pub fn output_schema_for<T: JsonSchema>() -> std::sync::Arc<rmcp::model::JsonObject> {
    input_schema_for::<T>()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_text_rejects_overlong_input() {
        let long = "x".repeat(MCP_MAX_QUERY_CHARS + 1);
        assert!(QueryText::parse(long).is_err());
        assert!(QueryText::parse("ok".to_owned()).is_ok());
    }

    #[test]
    fn search_limit_rejects_zero_and_over_max() {
        assert!(SearchLimit::parse(0).is_err());
        assert!(SearchLimit::parse(MCP_MAX_SEARCH_LIMIT + 1).is_err());
        assert_eq!(SearchLimit::parse(7).unwrap().get(), 7);
    }

    #[test]
    fn root_requires_absolute_path() {
        assert!(parse_root("").is_err());
        assert!(parse_root("relative/path").is_err());
    }

    #[test]
    fn search_input_schema_derives_object() {
        let schema = input_schema_for::<SearchInput>();
        assert_eq!(
            schema.get("type").and_then(|kind| kind.as_str()),
            Some("object")
        );
    }
}
