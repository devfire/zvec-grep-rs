//! zg-core: the zvec-grep engine.
//!
//! Layered exactly like the TypeScript original:
//!
//! ```text
//! types/ids/errors        pure domain model
//! utils/                  fs coordination (locks, leases, atomic JSON)
//! file_type/config/…      detection, configuration, manifests
//! extraction/             files → entity fragments
//! models/                 embedding backends
//! storage/                zvec-backed workspace index storage
//! lexical/                exhaustive text/regex path (grep crates)
//! service/ + pipeline/    orchestration: indexing + hybrid search
//! ```

// Test builds exercise fallible paths with `unwrap`/`expect`/`panic!` per the
// port plan (M8 permits `allow(unwrap_used)`/`allow(expect_used)` under
// `cfg(test)` only; `panic!` in `let-else` refusal branches is the same class).
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::unwrap_in_result,
        clippy::panic
    )
)]

pub mod authorization;
pub mod code_formats;
pub mod config;
pub mod error;
pub mod extraction;
pub mod file_size_policy;
pub mod file_type;
pub mod ids;
pub mod index_status;
pub mod lexical;
pub mod manifest;
pub mod models;
pub mod paths;
pub mod pipeline;
pub mod service;
pub mod storage;
pub mod types;
pub mod utils;

pub use error::{EngineError, EngineErrorCode, EngineResult};
