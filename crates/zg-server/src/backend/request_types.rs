//! Index, lexical, and status request/response types.
//!
//! Owned, borrow-free shapes that cross the actor boundary alongside the
//! search types in [`super::search_types`]: the index input
//! ([`IndexInput`]), the lexical query ([`RgQuery`]), the status overlay
//! ([`DaemonIndexStatus`]), and the liveness snapshot
//! ([`DaemonServerStatus`]).

use std::path::PathBuf;

use zg_core::index_status::IndexCompletion;
use zg_core::service::types::ZvecGrepInfoResult;
use zg_core::types::{FileScanDiagnostics, WorkspaceIndexStatus};

use crate::job_scheduler::IndexJobSnapshot;
use crate::root_runtime::Generation;

/// Owned lexical request (no borrows: crosses the actor boundary).
#[derive(Debug, Clone, Default)]
pub struct RgQuery {
    /// Regex (or literal) alternatives.
    pub patterns: Vec<String>,
    /// Search paths relative to the root (empty searches everything).
    pub paths: Vec<String>,
    /// Maximum matches collected.
    pub limit: Option<usize>,
    /// Treat patterns as literals.
    pub fixed_strings: bool,
    /// Case-insensitive matching.
    pub ignore_case: bool,
    /// Case-insensitive when every pattern is lowercase.
    pub smart_case: bool,
    /// Wrap patterns with word boundaries.
    pub word_regexp: bool,
    /// Match whole lines only (`--line-regexp`/`-x`).
    pub whole_line: bool,
    /// Maximum matches per file.
    pub max_count: Option<usize>,
    /// Pattern files: every non-empty line is one more pattern.
    pub pattern_files: Vec<String>,
    /// Case-sensitive glob filters.
    pub globs: Vec<String>,
    /// Case-insensitive glob filters.
    pub insensitive_globs: Vec<String>,
    /// Ripgrep file-type names to include.
    pub file_types: Vec<String>,
    /// Ripgrep file-type names to exclude.
    pub excluded_file_types: Vec<String>,
    /// Search hidden files.
    pub hidden: bool,
    /// Ignore ignore-files.
    pub no_ignore: bool,
    /// Extra ignore files.
    pub ignore_files: Vec<String>,
    /// Maximum directory depth.
    pub max_depth: Option<usize>,
    /// Skip files larger than this.
    pub max_file_size_bytes: Option<u64>,
    /// Context lines before each match.
    pub before_context: usize,
    /// Context lines after each match.
    pub after_context: usize,
}

/// Owned index request.
#[derive(Debug, Clone, Default)]
pub struct IndexInput {
    /// Rebuild from scratch.
    pub rebuild: bool,
    /// Changed paths for an incremental run (empty reconciles fully).
    pub changed_paths: Vec<PathBuf>,
}

/// Index status with its live job overlay.
#[derive(Debug, Clone)]
pub struct DaemonIndexStatus {
    /// Persisted status (cached after each finished run).
    pub status: WorkspaceIndexStatus,
    /// Latest job for the root, if any.
    pub job: Option<IndexJobSnapshot>,
    /// Completion counters with live progress overlaid while running.
    pub completion: Option<IndexCompletion>,
    /// Skipped-file diagnostics from the latest finished run, if any.
    pub scan_diagnostics: Option<FileScanDiagnostics>,
    /// Workspace info (policy, embedding, manifest); `None` when the info
    /// read fails (e.g. a disabled index) while status stays available.
    pub info: Option<ZvecGrepInfoResult>,
    /// Dirty revision counter at read time.
    pub dirty_revision: Generation,
    /// Newest indexed revision counter at read time.
    pub indexed_revision: Generation,
    /// Whether the root's filesystem watcher is active.
    pub watcher_active: bool,
}

/// Daemon liveness snapshot.
#[derive(Debug, Clone)]
pub struct DaemonServerStatus {
    /// Backend creation time, unix millis.
    pub started_at_ms: u64,
    /// Live root actors.
    pub runtimes: usize,
    /// Scheduler queue depth.
    pub queued_jobs: usize,
    /// Running jobs.
    pub running_jobs: usize,
    /// Resident models and active leases.
    pub pool_loaded: usize,
    /// Active model leases.
    pub pool_leases: usize,
}
