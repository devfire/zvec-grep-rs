//! Pool tuning: idle TTL, resident-model cap, constructor override, and
//! the load snapshot.

use std::time::Duration;

use crate::logger::DaemonLogger;

use super::request::CreateModelFn;

/// Idle TTL before a zero-lease model is evicted.
pub const DEFAULT_MODEL_IDLE_TTL: Duration = Duration::from_secs(15 * 60);

/// Default cap on resident loaded models.
pub const DEFAULT_MAX_LOADED_MODELS: usize = 1;

/// Options for [`crate::model_pool::EmbeddingModelPool`].
#[derive(Clone, Default)]
pub struct EmbeddingModelPoolOptions {
    /// Zero-lease idle TTL; defaults to 15 minutes. Zero evicts eagerly.
    pub idle_ttl: Option<Duration>,
    /// Cap on resident models; defaults to 1.
    pub max_loaded_models: Option<usize>,
    /// Model constructor; defaults to
    /// [`crate::model_pool::default_create_model`].
    pub create_model: Option<CreateModelFn>,
    /// Daemon logger for `model.load` / `model.cache_hit` / `model.evicted`.
    pub logger: Option<DaemonLogger>,
}

/// Pool load snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelPoolSnapshot {
    /// Entries with a resident model.
    pub loaded: usize,
    /// Leases currently checked out.
    pub active_leases: usize,
}
