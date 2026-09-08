//! Load request: what the pool constructs on a cache miss.

use std::sync::Arc;

use zg_core::error::EngineError;
use zg_core::models::EmbeddingModel;
use zg_core::models::catalog::ModelReference;
use zg_core::models::embeddings::CreateEmbeddingModelOptions;
use zg_core::models::factory::create_embedding_model;

/// What to load: a catalog reference plus its runtime options.
///
/// Mirrors TS `EmbeddingModelLoadRequest` (`model` identity + `runtime`
/// overrides); the Rust shape reuses the existing
/// [`CreateEmbeddingModelOptions`] instead of a second runtime struct.
#[derive(Debug, Clone)]
pub struct ModelLoadRequest {
    /// Catalog reference, e.g. `local/potion-retrieval-32m`.
    pub reference: ModelReference,
    /// API key / endpoint / cache dir / device overrides.
    pub options: CreateEmbeddingModelOptions,
}

impl ModelLoadRequest {
    /// Cache key: reference plus every option that changes the built
    /// model, so two runtimes never share one entry by accident.
    #[must_use]
    pub fn key(&self) -> String {
        format!(
            "{}\0{}\0{}\0{}\0{:?}",
            self.reference.as_str(),
            self.options.api_key.as_deref().unwrap_or(""),
            self.options.endpoint.as_deref().unwrap_or(""),
            self.options
                .model_cache_dir
                .as_ref()
                .map_or(String::new(), |dir| dir.display().to_string()),
            self.options.device,
        )
    }
}

/// Constructor for one model. Runs on `spawn_blocking`; must be `Send`.
pub type CreateModelFn =
    Arc<dyn Fn(&ModelLoadRequest) -> Result<Arc<dyn EmbeddingModel>, EngineError> + Send + Sync>;

/// Default constructor: the `zg-core` factory dispatch.
///
/// # Errors
///
/// Propagates whatever `create_embedding_model` reports for the reference.
pub fn default_create_model(
    request: &ModelLoadRequest,
) -> Result<Arc<dyn EmbeddingModel>, EngineError> {
    create_embedding_model(&request.reference, &request.options).map_err(EngineError::from)
}
