//! MCP endpoint: tool schemas, normalization, result rendering, and
//! transports over rmcp 0.6.
//!
//! Mirrors `../zvec-grep/src/mcp/` (`schemas`, `toolset`, `tools`,
//! `input-normalization`, `result-format`, `request-state`,
//! `request-metadata`, `progress-heartbeat`, `stdio-bridge`,
//! `http-transport`). Handlers delegate to [`DaemonBackend`](crate::backend::DaemonBackend)
//! commands and reimplement no search/index logic.

pub mod error;
pub mod http_transport;
pub mod input_normalization;
pub mod progress_heartbeat;
pub mod request_metadata;
pub mod request_state;
pub mod result_format;
pub mod schemas;
pub mod stdio_bridge;
pub mod tools;
pub mod toolset;
