//! Embedding model construction, mirroring `src/engine/models/factory.ts`.
//!
//! [`plan_embedding_model`] resolves a catalog reference plus user options
//! into a fully-validated [`ModelBuildPlan`] — endpoints defaulted, API keys
//! required, cache directories defaulted. Each plan variant feeds its owning
//! backend constructor; [`create_embedding_model`] is the dispatch entry
//! point. The heavy local backends (`onnx`, `llama`) construct their models
//! only when their cargo feature is enabled (M7); compiled out, the arm
//! reports [`ModelError::BackendUnavailable`], mirroring the TypeScript
//! `unsupportedCatalogEntry` arm with a typed error instead of a string.

use std::path::PathBuf;
use std::sync::Arc;

use super::EmbeddingModel;
#[cfg(feature = "llama")]
use super::backends::LlamaCppEmbeddingModel;
#[cfg(feature = "onnx")]
use super::backends::OnnxEmbeddingModel;
use super::backends::{Model2VecEmbeddingModel, Qwen3VlEmbeddingModel, QwenTextEmbeddingModel};
use super::catalog::{
    EmbeddingCatalogEntry, LlamaCppEntry, Model2VecEntry, ModelReference, QwenMultimodalEntry,
    QwenTextEntry, TransformersJsEntry, get_embedding_model_catalog_entry,
};
// Only the compiled-out backend arms report `BackendUnavailable`, so the
// kind import is dead when every backend is enabled.
#[cfg(any(not(feature = "onnx"), not(feature = "llama")))]
use super::catalog::BackendKind;
use super::embeddings::{ApiKey, CreateEmbeddingModelOptions, DeviceKind};
use super::error::{ModelError, QwenBackend, QwenTextModel};
use crate::config::{
    EmbeddingDevice, EmbeddingRuntimeConfig, GlobalConfig, resolve_embedding_runtime_options,
};
use crate::error::EngineResult;
use crate::paths::default_home;

/// Environment variable overriding the local model cache directory.
pub const MODEL_CACHE_ENV_VAR: &str = "ZVEC_GREP_MODEL_CACHE";

/// Default local model cache: `$ZVEC_GREP_MODEL_CACHE`, else `<home>/models`.
#[must_use]
pub fn default_model_cache_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os(MODEL_CACHE_ENV_VAR)
        && !dir.is_empty()
    {
        return PathBuf::from(dir);
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
        allow_private_endpoint: bool,
    },
    QwenMultimodal {
        entry: QwenMultimodalEntry,
        api_key: ApiKey,
        endpoint: String,
        allow_private_endpoint: bool,
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
    #[must_use]
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

fn device_to_config(device: DeviceKind) -> EmbeddingDevice {
    match device {
        DeviceKind::Auto => EmbeddingDevice::Auto,
        DeviceKind::Cpu => EmbeddingDevice::Cpu,
        DeviceKind::Metal => EmbeddingDevice::Metal,
        DeviceKind::Vulkan => EmbeddingDevice::Vulkan,
        DeviceKind::Cuda => EmbeddingDevice::Cuda,
    }
}

fn device_from_config(device: EmbeddingDevice) -> DeviceKind {
    match device {
        EmbeddingDevice::Auto => DeviceKind::Auto,
        EmbeddingDevice::Cpu => DeviceKind::Cpu,
        EmbeddingDevice::Metal => DeviceKind::Metal,
        EmbeddingDevice::Vulkan => DeviceKind::Vulkan,
        EmbeddingDevice::Cuda => DeviceKind::Cuda,
    }
}

/// Applies global config + workspace + env resolution to raw model
/// options: explicit fields in `options` win, then `workspace`, then
/// per-model/per-provider global config, then env. `device == Auto`
/// and `allow_private_endpoint == false` in `options` count as unset
/// (they are the defaults), so configured values can supply them.
/// Also fills `model_cache_dir` from `defaults.modelCacheDir`.
///
/// # Errors
///
/// Returns `CONFIG.INVALID_EMBEDDING_RUNTIME` when explicit options
/// are invalid for the reference (endpoint on a local model, device
/// on a remote model, non-HTTP(S) endpoint).
pub fn resolved_create_options(
    reference: &ModelReference,
    options: &CreateEmbeddingModelOptions,
    workspace: &EmbeddingRuntimeConfig,
    config: &GlobalConfig,
) -> EngineResult<CreateEmbeddingModelOptions> {
    let explicit = EmbeddingRuntimeConfig {
        api_key: options.api_key.clone(),
        endpoint: options.endpoint.clone(),
        device: if options.device == DeviceKind::Auto {
            None
        } else {
            Some(device_to_config(options.device))
        },
        allow_private_endpoint: options.allow_private_endpoint.then_some(true),
    };
    let resolved =
        resolve_embedding_runtime_options(reference.as_str(), &explicit, workspace, config)?;
    Ok(CreateEmbeddingModelOptions {
        api_key: if resolved.api_key.is_empty() {
            options.api_key.clone()
        } else {
            Some(resolved.api_key)
        },
        endpoint: resolved.endpoint,
        model_cache_dir: options.model_cache_dir.clone().or_else(|| {
            config
                .defaults
                .as_ref()
                .and_then(|defaults| defaults.model_cache_dir.as_ref())
                .map(PathBuf::from)
        }),
        device: resolved.device.map_or(options.device, device_from_config),
        allow_private_endpoint: resolved.allow_private_endpoint,
    })
}

/// Resolves `reference` against the catalog and validates backend-specific
/// options (Qwen API key/endpoint, local cache directory).
///
/// Takes the [`ModelReference`] newtype (M2) so a bare `&str` that is not a
/// model reference cannot flow in.
///
/// # Errors
///
/// Returns [`ModelError::CatalogModelNotFound`] for an unknown reference, [`ModelError::MissingApiKey`]
/// when a Qwen entry lacks an API key, or [`ModelError::MissingEndpoint`] when its endpoint is blank.
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
                allow_private_endpoint: options.allow_private_endpoint
                    || crate::config::allow_private_endpoint_from_env(),
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
            allow_private_endpoint: options.allow_private_endpoint
                || crate::config::allow_private_endpoint_from_env(),
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
/// Model2vec, both Qwen arms, and — when their cargo features are enabled —
/// the transformers-js (`onnx`) and llama-cpp (`llama`) arms construct
/// working models. With the feature compiled out the heavy arms report
/// [`ModelError::BackendUnavailable`]; unknown references report
/// [`ModelError::CatalogModelNotFound`] via [`plan_embedding_model`].
///
/// # Errors
///
/// Returns [`ModelError::CatalogModelNotFound`] for an unknown reference,
/// [`ModelError::MissingApiKey`] or [`ModelError::MissingEndpoint`] for invalid Qwen options, or
/// [`ModelError::BackendUnavailable`] when the entry's backend is compiled out.
pub fn create_embedding_model(
    reference: &ModelReference,
    options: &CreateEmbeddingModelOptions,
) -> Result<Arc<dyn EmbeddingModel>, ModelError> {
    let plan = plan_embedding_model(reference, options)?;
    // Only the `onnx`/`llama` arms consume the device; without either feature
    // the binding is dead (default builds warn on it).
    // `DeviceKind` is `Copy`: the plan borrow and this read coexist.
    #[cfg(any(feature = "onnx", feature = "llama"))]
    let device = options.device;
    match plan {
        ModelBuildPlan::Model2Vec { entry, cache_dir } => Ok(Arc::new(
            Model2VecEmbeddingModel::from_plan(entry, cache_dir),
        )),
        ModelBuildPlan::QwenText {
            entry,
            api_key,
            endpoint,
            allow_private_endpoint,
        } => Ok(Arc::new(QwenTextEmbeddingModel::from_plan(
            entry,
            api_key,
            endpoint,
            allow_private_endpoint,
        ))),
        ModelBuildPlan::QwenMultimodal {
            entry,
            api_key,
            endpoint,
            allow_private_endpoint,
        } => Ok(Arc::new(Qwen3VlEmbeddingModel::from_plan(
            entry,
            api_key,
            endpoint,
            allow_private_endpoint,
        ))),
        #[cfg(feature = "onnx")]
        ModelBuildPlan::TransformersJs { entry, cache_dir } => Ok(Arc::new(
            OnnxEmbeddingModel::from_plan(entry, cache_dir, device),
        )),
        #[cfg(not(feature = "onnx"))]
        ModelBuildPlan::TransformersJs { entry, .. } => Err(ModelError::BackendUnavailable {
            reference: entry.reference.to_owned(),
            backend: BackendKind::TransformersJs,
        }),
        #[cfg(feature = "llama")]
        ModelBuildPlan::LlamaCpp { entry, cache_dir } => Ok(Arc::new(
            LlamaCppEmbeddingModel::from_plan(entry, cache_dir, device),
        )),
        #[cfg(not(feature = "llama"))]
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
///
/// # Errors
///
/// Returns [`ModelError::MissingApiKey`] when the key is missing or blank.
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
///
/// # Errors
///
/// Returns [`ModelError::MissingEndpoint`] when the resolved endpoint is blank.
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
    fn resolved_options_apply_global_config_and_explicit_wins() {
        use crate::config::{EmbeddingDevice, GlobalConfig, GlobalDefaults, ProviderConfig};
        use std::collections::BTreeMap;
        let config = GlobalConfig {
            version: 1,
            defaults: Some(GlobalDefaults {
                embedding: None,
                model_cache_dir: Some("/tmp/global-cache".to_owned()),
            }),
            providers: Some(BTreeMap::from([(
                "qwen".to_owned(),
                ProviderConfig {
                    api_key: Some("global-key".to_owned()),
                },
            )])),
            models: Some(BTreeMap::from([(
                "qwen/text-embedding-v4".to_owned(),
                crate::config::EmbeddingModelConfig {
                    endpoint: Some("http://192.168.1.10:11434/v1".to_owned()),
                    device: None,
                    allow_private_endpoint: Some(true),
                },
            )])),
            client: None,
            server: None,
        };
        let resolved = resolved_create_options(
            &reference("qwen/text-embedding-v4"),
            &options(),
            &EmbeddingRuntimeConfig::default(),
            &config,
        )
        .unwrap();
        assert_eq!(resolved.api_key.as_deref(), Some("global-key"));
        assert_eq!(
            resolved.endpoint.as_deref(),
            Some("http://192.168.1.10:11434/v1")
        );
        assert!(resolved.allow_private_endpoint);
        assert_eq!(
            resolved.model_cache_dir,
            Some(PathBuf::from("/tmp/global-cache"))
        );
        // Explicit key wins over the global provider key.
        let mut explicit = options();
        explicit.api_key = Some("explicit-key".to_owned());
        let resolved = resolved_create_options(
            &reference("qwen/text-embedding-v4"),
            &explicit,
            &EmbeddingRuntimeConfig::default(),
            &config,
        )
        .unwrap();
        assert_eq!(resolved.api_key.as_deref(), Some("explicit-key"));
        // Local device comes from global config when unset (Auto).
        let local_config = GlobalConfig {
            models: Some(BTreeMap::from([(
                "local/bge-small-en-v1.5".to_owned(),
                crate::config::EmbeddingModelConfig {
                    endpoint: None,
                    device: Some(EmbeddingDevice::Cpu),
                    allow_private_endpoint: None,
                },
            )])),
            ..GlobalConfig::empty()
        };
        let resolved = resolved_create_options(
            &reference("local/bge-small-en-v1.5"),
            &options(),
            &EmbeddingRuntimeConfig::default(),
            &local_config,
        )
        .unwrap();
        assert_eq!(resolved.device, DeviceKind::Cpu);
        // The resolved key actually constructs the Qwen model (no MISSING_API_KEY).
        let resolved = resolved_create_options(
            &reference("qwen/text-embedding-v4"),
            &options(),
            &EmbeddingRuntimeConfig::default(),
            &config,
        )
        .unwrap();
        let model =
            create_embedding_model(&reference("qwen/text-embedding-v4"), &resolved).unwrap();
        assert_eq!(
            model.info().endpoint.as_deref(),
            Some("http://192.168.1.10:11434/v1")
        );
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

    /// Asserts one compiled-out entry reports `BackendUnavailable`.
    #[cfg(any(not(feature = "onnx"), not(feature = "llama")))]
    fn assert_backend_unavailable(name: &str) {
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

    #[cfg(not(feature = "llama"))]
    #[test]
    fn create_reports_llama_unavailable_when_compiled_out() {
        for name in ["local/embeddinggemma-300m", "local/qwen3-embedding-0.6b"] {
            assert_backend_unavailable(name);
        }
    }

    #[cfg(not(feature = "onnx"))]
    #[test]
    fn create_reports_onnx_unavailable_when_compiled_out() {
        for name in ["local/bge-small-en-v1.5", "local/all-minilm-l6-v2"] {
            assert_backend_unavailable(name);
        }
    }

    #[cfg(feature = "llama")]
    #[test]
    fn create_constructs_llama_when_compiled_in() {
        // Construction is pure (no download): dimensions and batch limits
        // come straight from the catalog.
        for (name, dimension, max_batch_size) in [
            ("local/embeddinggemma-300m", 768, 16),
            ("local/qwen3-embedding-0.6b", 1024, 8),
        ] {
            let model = create_embedding_model(&reference(name), &options()).unwrap();
            assert_eq!(model.info().dimension, dimension, "{name}");
            assert_eq!(model.max_batch_size(), max_batch_size, "{name}");
        }
    }

    #[cfg(feature = "onnx")]
    #[test]
    fn create_constructs_onnx_when_compiled_in() {
        for (name, dimension, max_batch_size) in [
            ("local/bge-small-en-v1.5", 384, 4),
            ("local/all-minilm-l6-v2", 384, 4),
        ] {
            let model = create_embedding_model(&reference(name), &options()).unwrap();
            assert_eq!(model.info().dimension, dimension, "{name}");
            assert_eq!(model.max_batch_size(), max_batch_size, "{name}");
        }
    }
}
