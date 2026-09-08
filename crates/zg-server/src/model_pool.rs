//! Embedding model pool: LRU cache with RAII leases and a bounded embed gate.
//!
//! Mirrors `../zvec-grep/src/daemon/model-pool.ts` (`EmbeddingModelPool`:
//! keyed entries, single-flight loads, lease counting, idle TTL, LRU trim
//! at `maxLoadedModels`). Two divergences (see `docs/ts-divergence.md`):
//!
//! - Loads run on `spawn_blocking` (model construction is synchronous CPU
//!   work) instead of `await createModel(...)`; concurrent acquirers of a
//!   loading key wait on a [`tokio::sync::Notify`] instead of
//!   sharing a promise. Waiters share a dehydrated error (the
//!   allocation-free [`zg_core::error::EngineErrorCode`] is
//!   `Copy`, so the code survives dehydration exactly) rather than
//!   re-running the load.
//! - TS `model.dispose()` has no Rust analogue: eviction drops the last
//!   `Arc`, and the model destructors run then. The observable behavior —
//!   at most `max_loaded_models` idle models resident — is identical.
//!
//! The pool also owns the single bounded embed gate from M6: all CPU-bound
//! embed work should hold [`EmbeddingModelPool::embed_permits`] (sized to
//! `available_parallelism`) so a many-root daemon cannot oversubscribe the
//! blocking pool.
//!
//! Layout: `pool::EmbeddingModelPool` is the checkout surface (`pool`);
//! load shapes live in `request`, failures in `error`, tuning in
//! `options`, entries and shared state in `state`, the RAII guard in
//! `lease`, behavior tests in `tests`.

mod error;
mod lease;
mod options;
mod pool;
mod request;
mod state;
#[cfg(test)]
mod tests;

pub use error::AcquireError;
pub use lease::ModelLease;
pub use options::{
    DEFAULT_MAX_LOADED_MODELS, DEFAULT_MODEL_IDLE_TTL, EmbeddingModelPoolOptions, ModelPoolSnapshot,
};
pub use pool::EmbeddingModelPool;
pub use request::{CreateModelFn, ModelLoadRequest, default_create_model};
