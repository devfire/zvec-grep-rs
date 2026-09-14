//! Workspace indexing: scan, diff, prepare, embed, commit.
//!
//! Port of `engine/pipeline/indexing/index.ts`. The TypeScript implementation
//! is async (`AbortSignal`, promise sets); this port is synchronous with
//! scoped-thread embedding parallelism:
//! - file preparation stays sequential in the calling thread;
//! - embedding units run on scoped threads bounded by the scheduler policy,
//!   each gated by the adaptive [`EmbeddingScheduler`] semaphore;
//! - storage commits stay serial in the calling thread (`&mut` storage is
//!   never shared across threads), in deterministic file order;
//! - cancellation flows through [`CancelFlag`] instead of `AbortSignal`.
//!
//! Retry, backoff, adaptive concurrency, and failure accounting mirror the TS
//! originals, including the batch → per-file → one-by-one fallback chain.

pub mod context;
pub mod diff;
pub mod embed;
pub mod input_budget;
pub mod passes;
pub mod prepare;
pub mod progress;
pub mod retry;
pub mod root_paths;
pub mod scanner;

pub use context::{IndexContext, IndexProgressSink};
pub use passes::{get_workspace_index_status, index_workspace, index_workspace_paths};
pub use retry::{ConcurrencyPolicy, EmbeddingRetryDecision, EmbeddingScheduler};
pub use scanner::CancelFlag;
