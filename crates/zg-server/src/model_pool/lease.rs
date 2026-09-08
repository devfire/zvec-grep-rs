//! RAII checkout guard: dropping (or releasing) returns the lease.

use std::sync::Arc;

use zg_core::models::EmbeddingModel;

use super::pool::EmbeddingModelPool;
use super::state::Shared;

/// Checked-out model. Dropping (or [`ModelLease::release`]) returns the
/// lease to the pool; the model unloads on idle TTL or pool close.
pub struct ModelLease {
    model: Arc<dyn EmbeddingModel>,
    key: String,
    pool: Option<Arc<Shared>>,
}

impl ModelLease {
    pub(crate) fn new(model: Arc<dyn EmbeddingModel>, key: String, pool: Arc<Shared>) -> Self {
        Self {
            model,
            key,
            pool: Some(pool),
        }
    }

    /// Checked-out model handle.
    #[must_use]
    pub fn model(&self) -> &Arc<dyn EmbeddingModel> {
        &self.model
    }

    /// Pool key the lease was checked out under.
    #[must_use]
    pub fn key(&self) -> &str {
        &self.key
    }

    /// Returns the lease; equivalent to dropping it.
    pub fn release(mut self) {
        self.return_lease();
    }

    fn return_lease(&mut self) {
        if let Some(pool) = self.pool.take() {
            EmbeddingModelPool::from_shared(pool).release_key(&self.key);
        }
    }
}

impl std::fmt::Debug for ModelLease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModelLease")
            .field("key", &self.key)
            .finish_non_exhaustive()
    }
}

impl Drop for ModelLease {
    fn drop(&mut self) {
        self.return_lease();
    }
}
