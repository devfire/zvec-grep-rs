//! Embedding model construction, mirroring `src/engine/models/factory.ts`.
//!
//! [`plan_embedding_model`] resolves a catalog reference plus user options
//! into a fully-validated [`ModelBuildPlan`] — endpoints defaulted, API keys
//! required, cache directories defaulted. Each plan variant feeds its owning
//! backend constructor; [`create_embedding_model`] is the dispatch entry
//! point. Entries whose backend has no implementation yet (llama-cpp and
//! transformers-js until phases C/D land their cargo-feature gates) report
//! [`ModelError::BackendUnavailable`], mirroring the TypeScript
//! `unsupportedCatalogEntry` arm with a typed error instead of a string.

use std::path::PathBuf;
use std::sync::Arc;

use super::EmbeddingModel;
use super::backends::{Model2VecEmbeddingModel, Qwen3VlEmbeddingModel, QwenTextEmbeddingModel};
use super::catalog::{
    BackendKind, EmbeddingCatalogEntry, LlamaCppEntry, Model2VecEntry, ModelReference,
    QwenMultimodalEntry, QwenTextEntry, TransformersJsEntry, get_embedding_model_catalog_entry,
};
use super::embeddings::{ApiKey, CreateEmbeddingModelOptions};
use super::error::{ModelError, QwenBackend, QwenTextModel};
use crate::paths::default_home;

/// Environment variable overriding the local model cache directory.
pub const MODEL_CACHE_ENV_VAR: &str = "ZVEC_GREP_MODEL_CACHE";

/// Default local model cache: `$ZVEC_GREP_MODEL_CACHE`, else `<home>/models`.
pub fn default_model_cache_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os(MODEL_CACHE_ENV_VAR) {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    default_home().join("models")
}

/// Effective cache directory: explicit option wins over the default.
pub fn resolve_model_cache_dir(options: &CreateEmbeddingModelOptions) -> PathBuf {
    options
        .model_cache_dir
        .clone()
        .unwrap_or_else(default_model_cache_dir)
}

/// Fully-resolved parameters for one backend constructor.
#[derive(Debug, Clone)]
pub enum ModelBuildPlan {
    LlamaCpp {
        entry: LlamaCppEntry,
        cache_dir: PathBuf,
    },
    QwenText {
        entry: QwenTextEntry,
        api_key: ApiKey,
        endpoint: String,
    },
    QwenMultimodal {
        entry: QwenMultimodalEntry,
        api_key: ApiKey,
        endpoint: String,
    },
    TransformersJs {
        entry: TransformersJsEntry,
        cache_dir: PathBuf,
    },
    Model2Vec {
        entry: Model2VecEntry,
        cache_dir: PathBuf,
    },
}

impl ModelBuildPlan {
    pub fn reference(&self) -> &'static str {
        match self {
            Self::LlamaCpp { entry, .. } => entry.reference,
            Self::QwenText { entry, .. } => entry.reference,
            Self::QwenMultimodal { entry, .. } => entry.reference,
            Self::TransformersJs { entry, .. } => entry.reference,
            Self::Model2Vec { entry, .. } => entry.reference,
        }
    }
}

/// Resolves `reference` against the catalog and validates backend-specific
/// options (Qwen API key/endpoint, local cache directory).
///
/// Takes the [`ModelReference`] newtype (M2) so a bare `&str` that is not a
/// model reference cannot flow in.
pub fn plan_embedding_model(
    reference: &ModelReference,
    options: &CreateEmbeddingModelOptions,
) -> Result<ModelBuildPlan, ModelError> {
    let entry = get_embedding_model_catalog_entry(reference.as_str()).ok_or_else(|| {
        ModelError::CatalogModelNotFound {
            reference: reference.as_str().to_owned(),
        }
    })?;
    match entry {
        EmbeddingCatalogEntry::LlamaCpp(entry) => Ok(ModelBuildPlan::LlamaCpp {
            entry: *entry,
            cache_dir: resolve_model_cache_dir(options),
        }),
        EmbeddingCatalogEntry::QwenText(entry) => {
            let model = QwenTextModel::from_model_id(entry.model);
            Ok(ModelBuildPlan::QwenText {
                entry: *entry,
                api_key: require_api_key(
                    entry.reference,
                    QwenBackend::Text(model),
                    options.api_key.as_deref(),
                )?,
                endpoint: resolve_endpoint(
                    entry.reference,
                    entry.default_endpoint,
                    QwenBackend::Text(model),
                    options.endpoint.as_deref(),
                )?,
            })
        }
        EmbeddingCatalogEntry::QwenMultimodal(entry) => Ok(ModelBuildPlan::QwenMultimodal {
            entry: *entry,
            api_key: require_api_key(entry.reference, QwenBackend::Vl, options.api_key.as_deref())?,
            endpoint: resolve_endpoint(
                entry.reference,
                entry.default_endpoint,
                QwenBackend::Vl,
                options.endpoint.as_deref(),
            )?,
        }),
        EmbeddingCatalogEntry::TransformersJs(entry) => Ok(ModelBuildPlan::TransformersJs {
            entry: *entry,
            cache_dir: resolve_model_cache_dir(options),
        }),
        EmbeddingCatalogEntry::Model2Vec(entry) => Ok(ModelBuildPlan::Model2Vec {
            entry: *entry,
            cache_dir: resolve_model_cache_dir(options),
        }),
    }
}

/// Dispatch entry point: plans the model, then hands the plan to the owning
/// backend constructor.
///
/// Model2vec and both Qwen arms construct working models. The llama-cpp and
/// transformers-js arms report [`ModelError::BackendUnavailable`] until
/// phases C/D land their cargo-feature gates; unknown references report
/// [`ModelError::CatalogModelNotFound`] via [`plan_embedding_model`].
pub fn create_embedding_model(
    reference: &ModelReference,
    options: &CreateEmbeddingModelOptions,
) -> Result<Arc<dyn EmbeddingModel>, ModelError> {
    let plan = plan_embedding_model(reference, options)?;
    match plan {
        ModelBuildPlan::Model2Vec { entry, cache_dir } => Ok(Arc::new(
            Model2VecEmbeddingModel::from_plan(entry, cache_dir),
        )),
        ModelBuildPlan::QwenText {
            entry,
            api_key,
            endpoint,
        } => Ok(Arc::new(QwenTextEmbeddingModel::from_plan(
            entry, api_key, endpoint,
        ))),
        ModelBuildPlan::QwenMultimodal {
            entry,
            api_key,
            endpoint,
        } => Ok(Arc::new(Qwen3VlEmbeddingModel::from_plan(
            entry, api_key, endpoint,
        ))),
        // No `onnx`/`llama` cargo features exist yet (phases C/D): these
        // entries resolve but cannot load, so the typed answer is
        // `BackendUnavailable`, not a stringly `NOT_IMPLEMENTED`.
        ModelBuildPlan::TransformersJs { entry, .. } => Err(ModelError::BackendUnavailable {
            reference: entry.reference.to_owned(),
            backend: BackendKind::TransformersJs,
        }),
        ModelBuildPlan::LlamaCpp { entry, .. } => Err(ModelError::BackendUnavailable {
            reference: entry.reference.to_owned(),
            backend: BackendKind::LlamaCpp,
        }),
    }
}

/// Requires a non-blank API key, mirroring the Qwen backend constructors.
///
/// The error code prefix follows the catalog entry (V4 / 3.7 / VL), exactly
/// like the TS constructors — never a caller-supplied string.
pub fn require_api_key(
    reference: &str,
    backend: QwenBackend,
    api_key: Option<&str>,
) -> Result<ApiKey, ModelError> {
    let key = api_key.unwrap_or("").trim();
    if key.is_empty() {
        return Err(ModelError::missing_api_key(reference, backend));
    }
    Ok(ApiKey::new(key))
}

/// Resolves the remote endpoint: explicit option (trimmed) wins over the
/// catalog default; a blank result is a configuration error.
pub fn resolve_endpoint(
    reference: &str,
    default_endpoint: &str,
    backend: QwenBackend,
    endpoint: Option<&str>,
) -> Result<String, ModelError> {
    let resolved = endpoint.unwrap_or(default_endpoint).trim();
    if resolved.is_empty() {
        return Err(ModelError::missing_endpoint(reference, backend));
    }
    Ok(resolved.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options() -> CreateEmbeddingModelOptions {
        CreateEmbeddingModelOptions::default()
    }

    fn reference(value: &str) -> ModelReference {
        ModelReference::new(value)
    }

    #[test]
    fn unknown_reference_reports_not_found() {
        let err = plan_embedding_model(&reference("nope/unknown"), &options()).unwrap_err();
        assert_eq!(
            err.code().to_string(),
            "ZVEC_GREP.ENGINE.MODELS.EMBEDDING_CATALOG_MODEL_NOT_FOUND"
        );
    }

    #[test]
    fn qwen_text_requires_api_key() {
        let err =
            plan_embedding_model(&reference("qwen/text-embedding-v4"), &options()).unwrap_err();
        // TS-true code: the V4 subclass prefix, not the base prefix.
        assert_eq!(
            err.code().to_string(),
            "ZVEC_GREP.ENGINE.MODELS.QWEN_TEXT_EMBEDDING_V4_MISSING_API_KEY"
        );
        let err = plan_embedding_model(&reference("qwen/qwen3.7-text-embedding"), &options())
            .unwrap_err();
        assert_eq!(
            err.code().to_string(),
            "ZVEC_GREP.ENGINE.MODELS.QWEN37_TEXT_EMBEDDING_MISSING_API_KEY"
        );
    }

    #[test]
    fn qwen_text_plans_with_key() {
        let mut opts = options();
        opts.api_key = Some("  secret  ".to_owned());
        let plan = plan_embedding_model(&reference("qwen/text-embedding-v4"), &opts).unwrap();
        let ModelBuildPlan::QwenText {
            api_key, endpoint, ..
        } = plan
        else {
            panic!("expected QwenText plan");
        };
        assert_eq!(api_key.as_str(), "secret");
        assert!(endpoint.starts_with("https://"));
    }

    #[test]
    fn blank_endpoint_override_is_rejected() {
        let mut opts = options();
        opts.api_key = Some("secret".to_owned());
        opts.endpoint = Some("   ".to_owned());
        let err = plan_embedding_model(&reference("qwen/text-embedding-v4"), &opts).unwrap_err();
        assert_eq!(
            err.code().to_string(),
            "ZVEC_GREP.ENGINE.MODELS.QWEN_TEXT_EMBEDDING_V4_MISSING_ENDPOINT"
        );
    }

    #[test]
    fn create_resolves_model2vec_with_catalog_dimension() {
        let model =
            create_embedding_model(&reference("local/potion-retrieval-32m"), &options()).unwrap();
        assert_eq!(model.info().dimension, 512);
    }

    #[test]
    fn create_resolves_qwen_models_with_catalog_dimensions() {
        let mut opts = options();
        opts.api_key = Some("test-key".to_owned());
        for (name, dimension) in [
            ("qwen/text-embedding-v4", 1024),
            ("qwen/qwen3.7-text-embedding", 1024),
            ("qwen/qwen3-vl-embedding", 2560),
        ] {
            let model = create_embedding_model(&reference(name), &opts).unwrap();
            assert_eq!(model.info().dimension, dimension, "{name}");
        }
    }

    #[test]
    fn create_reports_unknown_reference() {
        let Err(err) = create_embedding_model(&reference("nope/unknown"), &options()) else {
            panic!("expected CatalogModelNotFound");
        };
        assert!(matches!(err, ModelError::CatalogModelNotFound { .. }));
        assert_eq!(
            err.code().to_string(),
            "ZVEC_GREP.ENGINE.MODELS.EMBEDDING_CATALOG_MODEL_NOT_FOUND"
        );
    }

    #[test]
    fn create_reports_backend_unavailable_when_compiled_out() {
        // No `onnx`/`llama` cargo features exist yet (phases C/D), so the
        // transformers-js and llama-cpp entries resolve but cannot load.
        for name in ["local/embeddinggemma-300m", "local/bge-small-en-v1.5"] {
            let Err(err) = create_embedding_model(&reference(name), &options()) else {
                panic!("expected BackendUnavailable for {name}");
            };
            assert!(
                matches!(err, ModelError::BackendUnavailable { .. }),
                "{name}"
            );
            assert_eq!(
                err.code().to_string(),
                "ZVEC_GREP.ENGINE.MODELS.EMBEDDING_BACKEND_UNAVAILABLE"
            );
        }
    }
}
