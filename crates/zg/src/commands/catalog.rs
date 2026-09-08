//! Embedding catalog lookups shared by command handlers.
//!
//! Wraps `zg_core::models::catalog` with CLI-facing validation:
//! every reference is checked against the frozen catalog before use.

use zg_core::models::catalog::{EmbeddingCatalogEntry, ModelReference, list_embedding_models};

use crate::error::CliError;

/// Validates that a reference names a catalog entry, mirroring
/// `requireEmbeddingModelCatalogEntry`.
pub(crate) fn catalog_reference(reference: &str) -> Result<ModelReference, CliError> {
    if catalog_entry(reference).is_some() {
        return Ok(ModelReference::new(reference.to_owned()));
    }
    Err(CliError::config_invalid(format!(
        "Unknown embedding model \"{reference}\". Use zg help models for the catalog."
    )))
}

/// Finds a catalog entry by reference.
pub(crate) fn catalog_entry(reference: &str) -> Option<&'static EmbeddingCatalogEntry> {
    list_embedding_models()
        .iter()
        .find(|entry| catalog_identity(entry).0 == reference)
}

/// `(reference, provider, model, dimension)` for one catalog entry.
///
/// The match is exhaustive over [`EmbeddingCatalogEntry`]: a new backend
/// variant fails compilation here until its identity is defined.
pub(crate) fn catalog_identity(
    entry: &EmbeddingCatalogEntry,
) -> (&'static str, &'static str, &'static str, usize) {
    match entry {
        EmbeddingCatalogEntry::LlamaCpp(model) => (
            model.reference,
            model.provider,
            model.model,
            model.dimension,
        ),
        EmbeddingCatalogEntry::QwenText(model) => (
            model.reference,
            model.provider,
            model.model,
            model.dimension,
        ),
        EmbeddingCatalogEntry::QwenMultimodal(model) => (
            model.reference,
            model.provider,
            model.model,
            model.dimension,
        ),
        EmbeddingCatalogEntry::TransformersJs(model) => (
            model.reference,
            model.provider,
            model.model,
            model.dimension,
        ),
        EmbeddingCatalogEntry::Model2Vec(model) => (
            model.reference,
            model.provider,
            model.model,
            model.dimension,
        ),
    }
}

/// Maps a recorded `(provider, model)` schema back to its catalog
/// reference.
pub(crate) fn catalog_reference_for(provider: &str, model: &str) -> Option<ModelReference> {
    list_embedding_models()
        .iter()
        .find(|entry| {
            let identity = catalog_identity(entry);
            identity.1 == provider && identity.2 == model
        })
        .map(|entry| ModelReference::new(catalog_identity(entry).0.to_owned()))
}
