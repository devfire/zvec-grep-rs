//! zg-server: the zvec-grep local server — daemon HTTP API, MCP endpoint,
//! job scheduling, and remote-embedding authorization.

// Test builds exercise fallible paths with `unwrap`/`expect` per the port
// plan (M8 permits `allow(unwrap_used)` under `cfg(test)` only).
#![cfg_attr(
    test,
    allow(clippy::unwrap_used, clippy::expect_used, clippy::unwrap_in_result)
)]

pub mod backend;
pub mod change_set;
pub mod config;
pub mod errors;
pub mod http_server;
pub mod index_coordinator;
pub mod job_scheduler;
pub mod logger;
pub mod model_pool;
pub mod read_session_cache;
pub mod root_runtime;
pub mod runtime_manager;
pub mod server_controller;
pub mod trace;
pub mod watch_manager;
