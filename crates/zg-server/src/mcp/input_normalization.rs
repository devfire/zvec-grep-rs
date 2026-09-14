//! MCP input normalization: validated search/rg inputs from wire structs.
//!
//! Mirrors `../zvec-grep/src/mcp/input-normalization.ts`
//! (`normalizeSearchInput`, `contextOptionsFromRgInput`) and folds in the
//! managed-rg command parser from `../zvec-grep/src/cli/managed-rg.ts`
//! (the MCP layer is its only consumer). The real-rg `extraArgs`
//! surface (invert, multiline, engines, threads, encodings) is rejected:
//! the port searches in-process and cannot forward raw ripgrep flags
//! (see `docs/ts-divergence.md`).

pub mod paths;
pub mod rg_args;
pub mod rg_scan;
pub mod search;

pub use rg_args::rg_query_from_input;
pub use search::{
    NormalizedRoute, NormalizedSearchInput, normalize_search_input, parse_modified_time,
};
