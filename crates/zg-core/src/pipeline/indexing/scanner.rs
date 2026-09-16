//! Workspace file scanner: directory walk, ignore rules, file typing.
//!
//! Port of `engine/pipeline/indexing/scanner/index.ts`. Synchronous
//! (`std::fs`) where the TS original is async; cancellation flows through an
//! optional atomic flag instead of `AbortSignal`.
//!
//! Facade: each concern lives in `scanner/<part>.rs`; this file only declares
//! the submodules and re-exports the public surface so downstream `use`
//! lines keep compiling.

pub mod file_info;
pub mod hidden;
pub mod ignore;
pub mod types;
pub mod walk;

pub use file_info::create_scan_diagnostics;
pub use types::{CancelFlag, PathKind, ScanOptions, ScanResult};
pub use walk::{path_can_affect_index, scan_directory_path, scan_file_path, scan_root_paths};
