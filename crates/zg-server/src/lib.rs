//! zg-server: the zvec-grep local server — daemon HTTP API, MCP endpoint,
//! job scheduling, and remote-embedding authorization.

// Test builds exercise fallible paths with `unwrap`/`expect` per the port
// plan (M8 permits `allow(unwrap_used)` under `cfg(test)` only).
#![cfg_attr(
    test,
    allow(clippy::unwrap_used, clippy::expect_used, clippy::unwrap_in_result)
)]
