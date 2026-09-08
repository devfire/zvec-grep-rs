//! MCP tool schemas: frozen numeric bounds, validated boundary newtypes,
//! and wire input/output structs.
//!
//! Mirrors `../zvec-grep/src/mcp/schemas.ts`. Per M2 the numeric bounds
//! are `const` values validated in one place: wire structs deserialize
//! loosely and convert into validated newtypes ([`QueryText`],
//! [`PathFilter`], [`SearchLimit`]) at the boundary, so a handler cannot
//! receive an out-of-range value. Output structs use the TS
//! snake_case/camelCase wire shapes verbatim (a wire fact per M3).
//!
//! Layout: shared bounds live in `bounds`; wire inputs in `inputs`;
//! job/index outputs in `outputs`; status outputs and
//! [`format_index_status`] in `status`; rmcp schema builders in
//! `tool_schema`; boundary regression tests in `tests`.

mod bounds;
mod inputs;
mod outputs;
mod status;
#[cfg(test)]
mod tests;
mod tool_schema;

pub use bounds::{
    MCP_MAX_PATH_CHARS, MCP_MAX_PATH_FILTERS, MCP_MAX_QUERY_CHARS, MCP_MAX_QUERY_GROUPS,
    MCP_MAX_SEARCH_LIMIT, PathFilter, QueryText, SearchLimit, bound_groups, bound_path_filters,
    parse_root,
};
pub use inputs::{
    CodeSymbolTypeInput, DeviceInput, FreshnessInput, IndexDropInput, IndexInput, IndexStatusInput,
    RgInput, SearchInput, ServerStatusInput, StringOrList, TimeInput,
};
pub use outputs::{
    IndexActionOutput, IndexDropOutput, IndexOutput, JobErrorOutput, JobStateOutput,
    ScanDiagnosticsOutput, SkippedFileSampleOutput,
};
pub use status::{
    IndexPolicyOutput, IndexSourceOutput, IndexStatusOutput, PersistentStatusOutput,
    RootPathOutput, RuntimeStatusOutput, ServerModelsOutput, ServerStatusOutput,
    StatusCompletionOutput, StatusEmbeddingOutput, StatusFilesOutput, StatusProgressOutput,
    StatusWorkspaceIndexOutput, format_index_status,
};
pub use tool_schema::{input_schema_for, output_schema_for};
