//! Wire input shapes: per-tool request structs and the small input enums
//! they compose.

use schemars::JsonSchema;
use serde::Deserialize;
use zg_core::types::CodeSymbolType as CoreSymbolType;

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
