//! Embedding-model resolution for the facade: injected handle first, then
//! the recorded manifest schema, explicit option, and catalog fallback.

use std::sync::Arc;

use super::service::ZvecGrepService;
use crate::error::{EngineError, EngineResult, codes};
use crate::manifest::WorkspaceManifest;
use crate::models::catalog::{EmbeddingCatalogEntry, ModelReference};
use crate::models::embeddings::{CreateEmbeddingModelOptions, DeviceKind};
use crate::models::factory::create_embedding_model;
use crate::models::resolution::{ResolveEmbeddingReferenceOptions, resolve_embedding_reference};
use crate::models::{EmbeddingModel, EmbeddingModelInfo};
use crate::types::WorkspaceIndexEmbeddingSchema;

/// Fallback embedding model when nothing selects one, mirroring
/// `DEFAULT_LOCAL_EMBEDDING` in the TS service.
pub const DEFAULT_EMBEDDING_REFERENCE: &str = "local/potion-code-16m-v2";

impl ZvecGrepService {
    pub(super) fn model_for_manifest(
        &self,
        manifest: Option<&WorkspaceManifest>,
    ) -> EngineResult<Arc<dyn EmbeddingModel>> {
        if let Some(model) = &self.embedding_model {
            return Ok(Arc::clone(model));
        }
        let existing = manifest.and_then(schema_reference);
        let reference = resolve_embedding_reference(&ResolveEmbeddingReferenceOptions {
            explicit: self.embedding.clone(),
            existing,
            global_default: None,
            environment: None,
            fallback: Some(ModelReference::new(DEFAULT_EMBEDDING_REFERENCE)),
        })?
        .ok_or_else(|| {
            EngineError::new(
                codes::config_invalid_embedding_runtime(),
                "no embedding model is selected",
            )
        })?;
        let options = CreateEmbeddingModelOptions {
            api_key: self.api_key.clone(),
            endpoint: self.endpoint.clone(),
            model_cache_dir: self.model_cache_dir.clone(),
            device: DeviceKind::Auto,
        };
        create_embedding_model(&reference, &options).map_err(EngineError::from)
    }
}

pub(super) fn embedding_schema(model: &dyn EmbeddingModel) -> WorkspaceIndexEmbeddingSchema {
    let info: &EmbeddingModelInfo = model.info();
    WorkspaceIndexEmbeddingSchema {
        provider: info.provider.clone(),
        model: info.model.clone(),
        dimension: info.dimension,
        metric: info.metric,
    }
}

/// Maps a recorded embedding schema back to its catalog reference by
/// provider + model identity.
fn schema_reference(manifest: &WorkspaceManifest) -> Option<ModelReference> {
    let schema = manifest.info.embedding.clone().flatten()?;
    let entry = get_embedding_model_catalog_entry_for_schema(&schema.provider, &schema.model)?;
    Some(ModelReference::new(entry))
}

fn get_embedding_model_catalog_entry_for_schema(
    provider: &str,
    model: &str,
) -> Option<&'static str> {
    crate::models::catalog::EMBEDDING_MODEL_CATALOG
        .iter()
        .find_map(|entry| {
            let (entry_provider, entry_model, reference) = match entry {
                EmbeddingCatalogEntry::LlamaCpp(entry) => {
                    (entry.provider, entry.model, entry.reference)
                }
                EmbeddingCatalogEntry::QwenText(entry) => {
                    (entry.provider, entry.model, entry.reference)
                }
                EmbeddingCatalogEntry::QwenMultimodal(entry) => {
                    (entry.provider, entry.model, entry.reference)
                }
                EmbeddingCatalogEntry::TransformersJs(entry) => {
                    (entry.provider, entry.model, entry.reference)
                }
                EmbeddingCatalogEntry::Model2Vec(entry) => {
                    (entry.provider, entry.model, entry.reference)
                }
            };
            (entry_provider == provider && entry_model == model).then_some(reference)
        })
}
