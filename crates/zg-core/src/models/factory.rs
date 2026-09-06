//! Embedding model construction, mirroring `src/engine/models/factory.ts`.
//!
//! [`plan_embedding_model`] resolves a catalog reference plus user options
//! into a fully-validated [`ModelBuildPlan`] — endpoints defaulted, API keys
//! required, cache directories defaulted. The backends wave consumes each
//! plan variant in its constructor; [`create_embedding_model`] is the
//! dispatch entry point and reports `NOT_IMPLEMENTED` (exactly like the
//! TypeScript `unsupportedCatalogEntry` arm) until a backend claims its arm.

use std::path::PathBuf;
use std::sync::Arc;

use super::EmbeddingModel;
use super::catalog::{
    EmbeddingCatalogEntry, LlamaCppEntry, Model2VecEntry, QwenMultimodalEntry, QwenTextEntry,
    TransformersJsEntry, get_embedding_model_catalog_entry,
};
use super::embeddings::{ApiKey, CreateEmbeddingModelOptions};
use crate::error::{EngineError, EngineErrorCode, EngineResult};
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
pub fn plan_embedding_model(
    reference: &str,
    options: &CreateEmbeddingModelOptions,
) -> EngineResult<ModelBuildPlan> {
    let entry = get_embedding_model_catalog_entry(reference).ok_or_else(|| {
        EngineError::new(
            EngineErrorCode::new("MODELS.EMBEDDING_CATALOG_MODEL_NOT_FOUND"),
            "unknown embedding model reference",
        )
        .with_context(format!("embedding={reference}"))
    })?;
    match entry {
        EmbeddingCatalogEntry::LlamaCpp(entry) => Ok(ModelBuildPlan::LlamaCpp {
            entry: *entry,
            cache_dir: resolve_model_cache_dir(options),
        }),
        EmbeddingCatalogEntry::QwenText(entry) => Ok(ModelBuildPlan::QwenText {
            entry: *entry,
            api_key: require_api_key(
                entry.reference,
                "Qwen text embedding",
                "QWEN_TEXT_EMBEDDING",
                options.api_key.as_deref(),
            )?,
            endpoint: resolve_endpoint(
                entry.reference,
                entry.default_endpoint,
                "QWEN_TEXT_EMBEDDING",
                options.endpoint.as_deref(),
            )?,
        }),
        EmbeddingCatalogEntry::QwenMultimodal(entry) => Ok(ModelBuildPlan::QwenMultimodal {
            entry: *entry,
            api_key: require_api_key(
                entry.reference,
                "Qwen3 VL embedding",
                "QWEN3_VL_EMBEDDING",
                options.api_key.as_deref(),
            )?,
            endpoint: resolve_endpoint(
                entry.reference,
                entry.default_endpoint,
                "QWEN3_VL_EMBEDDING",
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
/// Per-backend arms currently report `EMBEDDING_MODEL_NOT_IMPLEMENTED` —
/// the exact TypeScript `unsupportedCatalogEntry` behavior — until the
/// backends wave claims them. Unknown references report
/// `EMBEDDING_CATALOG_MODEL_NOT_FOUND` via [`plan_embedding_model`].
pub fn create_embedding_model(
    reference: &str,
    options: &CreateEmbeddingModelOptions,
) -> EngineResult<Arc<dyn EmbeddingModel>> {
    let plan = plan_embedding_model(reference, options)?;
    Err(EngineError::new(
        EngineErrorCode::new("MODELS.EMBEDDING_MODEL_NOT_IMPLEMENTED"),
        "embedding backend not implemented",
    )
    .with_context(format!(
        "reference={} backend={}",
        plan.reference(),
        match &plan {
            ModelBuildPlan::LlamaCpp { .. } => "llama-cpp",
            ModelBuildPlan::QwenText { .. } | ModelBuildPlan::QwenMultimodal { .. } => "qwen",
            ModelBuildPlan::TransformersJs { .. } => "transformers-js",
            ModelBuildPlan::Model2Vec { .. } => "model2vec",
        }
    )))
}

/// Requires a non-blank API key, mirroring the Qwen backend constructors.
pub fn require_api_key(
    reference: &str,
    display_name: &str,
    error_code_prefix: &str,
    api_key: Option<&str>,
) -> EngineResult<ApiKey> {
    let key = api_key.unwrap_or("").trim();
    if key.is_empty() {
        return Err(EngineError::new(
            EngineErrorCode::new(&format!("MODELS.{error_code_prefix}_MISSING_API_KEY")),
            "missing API key",
        )
        .with_context(format!(
            "model={reference} backend={display_name}\nhint=Pass --api-key, set ZVEC_GREP_API_KEY, or configure providers.qwen.apiKey."
        )));
    }
    Ok(ApiKey::new(key))
}

/// Resolves the remote endpoint: explicit option (trimmed) wins over the
/// catalog default; a blank result is a configuration error.
pub fn resolve_endpoint(
    reference: &str,
    default_endpoint: &str,
    error_code_prefix: &str,
    endpoint: Option<&str>,
) -> EngineResult<String> {
    let resolved = endpoint.unwrap_or(default_endpoint).trim();
    if resolved.is_empty() {
        return Err(EngineError::new(
            EngineErrorCode::new(&format!("MODELS.{error_code_prefix}_MISSING_ENDPOINT")),
            "missing endpoint",
        )
        .with_context(format!("model={reference}")));
    }
    Ok(resolved.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options() -> CreateEmbeddingModelOptions {
        CreateEmbeddingModelOptions::default()
    }

    #[test]
    fn unknown_reference_reports_not_found() {
        let err = plan_embedding_model("nope/unknown", &options()).unwrap_err();
        assert_eq!(
            err.code().to_string(),
            "ZVEC_GREP.ENGINE.MODELS.EMBEDDING_CATALOG_MODEL_NOT_FOUND"
        );
    }

    #[test]
    fn qwen_text_requires_api_key() {
        let err = plan_embedding_model("qwen/text-embedding-v4", &options()).unwrap_err();
        assert_eq!(
            err.code().to_string(),
            "ZVEC_GREP.ENGINE.MODELS.QWEN_TEXT_EMBEDDING_MISSING_API_KEY"
        );
    }

    #[test]
    fn qwen_text_plans_with_key() {
        let mut opts = options();
        opts.api_key = Some("  secret  ".to_owned());
        let plan = plan_embedding_model("qwen/text-embedding-v4", &opts).unwrap();
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
        let err = plan_embedding_model("qwen/text-embedding-v4", &opts).unwrap_err();
        assert_eq!(
            err.code().to_string(),
            "ZVEC_GREP.ENGINE.MODELS.QWEN_TEXT_EMBEDDING_MISSING_ENDPOINT"
        );
    }
}
