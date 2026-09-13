//! Typed model errors: every `MODELS.*` wire code as an enum variant.
//!
//! Mirrors the error construction in `src/engine/models/*` (factory,
//! backends, validation). The wire strings are unchanged from TypeScript;
//! the identity is an exhaustive enum, so adding a failure mode without a
//! code mapping fails to compile instead of shipping a typo.
//!
//! Qwen codes follow the TS `${prefix}_${suffix}` shape
//! (`backends/qwen.ts:226`), but each combination is an explicit match arm
//! below — never a runtime `format!`. The per-model prefixes
//! (`QWEN_TEXT_EMBEDDING_V4`, `QWEN37_TEXT_EMBEDDING`,
//! `QWEN3_VL_EMBEDDING`) are verified against the TS source; the
//! `QWEN_TEXT_EMBEDDING` fallback for future catalog entries is a Rust
//! addition (no TS equivalent) and is recorded in `docs/ts-divergence.md`.

use super::catalog::BackendKind;
use crate::error::{EngineError, EngineErrorCode};
use crate::paths::global_config_path;

/// Qwen text model identity: selects the error-code prefix and display name.
///
/// Mirrors the two TS subclasses (`QwenTextEmbeddingV4Model`,
/// `Qwen37TextEmbeddingModel`); `Other` covers future catalog entries with
/// the base prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum QwenTextModel {
    /// `text-embedding-v4` → `QWEN_TEXT_EMBEDDING_V4_*`.
    V4,
    /// `qwen3.7-text-embedding` → `QWEN37_TEXT_EMBEDDING_*`.
    V37,
    /// Any future text model → `QWEN_TEXT_EMBEDDING_*` (Rust addition).
    Other,
}

impl QwenTextModel {
    /// Resolves the model identity from a catalog model id.
    #[must_use]
    pub fn from_model_id(model: &str) -> Self {
        match model {
            "text-embedding-v4" => Self::V4,
            "qwen3.7-text-embedding" => Self::V37,
            _ => Self::Other,
        }
    }

    /// Human-readable name used in error messages (mirrors TS `displayName`).
    #[must_use]
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::V4 => "Qwen text-embedding-v4",
            Self::V37 => "Qwen3.7 text embedding",
            Self::Other => "Qwen text embedding",
        }
    }

    /// Error-code prefix (mirrors TS `errorCodePrefix`).
    #[must_use]
    pub const fn code_prefix(self) -> &'static str {
        match self {
            Self::V4 => "QWEN_TEXT_EMBEDDING_V4",
            Self::V37 => "QWEN37_TEXT_EMBEDDING",
            Self::Other => "QWEN_TEXT_EMBEDDING",
        }
    }

    /// Maps a text-backend failure to its fully-qualified code.
    #[must_use]
    pub const fn code(self, failure: QwenTextFailure) -> EngineErrorCode {
        match (self, failure) {
            (Self::V4, QwenTextFailure::RequestFailed) => {
                EngineErrorCode::from_static("MODELS.QWEN_TEXT_EMBEDDING_V4_REQUEST_FAILED")
            }
            (Self::V4, QwenTextFailure::InvalidJson) => {
                EngineErrorCode::from_static("MODELS.QWEN_TEXT_EMBEDDING_V4_INVALID_JSON")
            }
            (Self::V4, QwenTextFailure::ApiError) => {
                EngineErrorCode::from_static("MODELS.QWEN_TEXT_EMBEDDING_V4_API_ERROR")
            }
            (Self::V4, QwenTextFailure::MissingData) => {
                EngineErrorCode::from_static("MODELS.QWEN_TEXT_EMBEDDING_V4_MISSING_DATA")
            }
            (Self::V4, QwenTextFailure::InvalidIndex) => {
                EngineErrorCode::from_static("MODELS.QWEN_TEXT_EMBEDDING_V4_INVALID_INDEX")
            }
            (Self::V4, QwenTextFailure::IndexOutOfRange) => {
                EngineErrorCode::from_static("MODELS.QWEN_TEXT_EMBEDDING_V4_INDEX_OUT_OF_RANGE")
            }
            (Self::V4, QwenTextFailure::InvalidVector) => {
                EngineErrorCode::from_static("MODELS.QWEN_TEXT_EMBEDDING_V4_INVALID_VECTOR")
            }
            (Self::V4, QwenTextFailure::MissingApiKey) => {
                EngineErrorCode::from_static("MODELS.QWEN_TEXT_EMBEDDING_V4_MISSING_API_KEY")
            }
            (Self::V4, QwenTextFailure::MissingEndpoint) => {
                EngineErrorCode::from_static("MODELS.QWEN_TEXT_EMBEDDING_V4_MISSING_ENDPOINT")
            }
            (Self::V37, QwenTextFailure::RequestFailed) => {
                EngineErrorCode::from_static("MODELS.QWEN37_TEXT_EMBEDDING_REQUEST_FAILED")
            }
            (Self::V37, QwenTextFailure::InvalidJson) => {
                EngineErrorCode::from_static("MODELS.QWEN37_TEXT_EMBEDDING_INVALID_JSON")
            }
            (Self::V37, QwenTextFailure::ApiError) => {
                EngineErrorCode::from_static("MODELS.QWEN37_TEXT_EMBEDDING_API_ERROR")
            }
            (Self::V37, QwenTextFailure::MissingData) => {
                EngineErrorCode::from_static("MODELS.QWEN37_TEXT_EMBEDDING_MISSING_DATA")
            }
            (Self::V37, QwenTextFailure::InvalidIndex) => {
                EngineErrorCode::from_static("MODELS.QWEN37_TEXT_EMBEDDING_INVALID_INDEX")
            }
            (Self::V37, QwenTextFailure::IndexOutOfRange) => {
                EngineErrorCode::from_static("MODELS.QWEN37_TEXT_EMBEDDING_INDEX_OUT_OF_RANGE")
            }
            (Self::V37, QwenTextFailure::InvalidVector) => {
                EngineErrorCode::from_static("MODELS.QWEN37_TEXT_EMBEDDING_INVALID_VECTOR")
            }
            (Self::V37, QwenTextFailure::MissingApiKey) => {
                EngineErrorCode::from_static("MODELS.QWEN37_TEXT_EMBEDDING_MISSING_API_KEY")
            }
            (Self::V37, QwenTextFailure::MissingEndpoint) => {
                EngineErrorCode::from_static("MODELS.QWEN37_TEXT_EMBEDDING_MISSING_ENDPOINT")
            }
            (Self::Other, QwenTextFailure::RequestFailed) => {
                EngineErrorCode::from_static("MODELS.QWEN_TEXT_EMBEDDING_REQUEST_FAILED")
            }
            (Self::Other, QwenTextFailure::InvalidJson) => {
                EngineErrorCode::from_static("MODELS.QWEN_TEXT_EMBEDDING_INVALID_JSON")
            }
            (Self::Other, QwenTextFailure::ApiError) => {
                EngineErrorCode::from_static("MODELS.QWEN_TEXT_EMBEDDING_API_ERROR")
            }
            (Self::Other, QwenTextFailure::MissingData) => {
                EngineErrorCode::from_static("MODELS.QWEN_TEXT_EMBEDDING_MISSING_DATA")
            }
            (Self::Other, QwenTextFailure::InvalidIndex) => {
                EngineErrorCode::from_static("MODELS.QWEN_TEXT_EMBEDDING_INVALID_INDEX")
            }
            (Self::Other, QwenTextFailure::IndexOutOfRange) => {
                EngineErrorCode::from_static("MODELS.QWEN_TEXT_EMBEDDING_INDEX_OUT_OF_RANGE")
            }
            (Self::Other, QwenTextFailure::InvalidVector) => {
                EngineErrorCode::from_static("MODELS.QWEN_TEXT_EMBEDDING_INVALID_VECTOR")
            }
            (Self::Other, QwenTextFailure::MissingApiKey) => {
                EngineErrorCode::from_static("MODELS.QWEN_TEXT_EMBEDDING_MISSING_API_KEY")
            }
            (Self::Other, QwenTextFailure::MissingEndpoint) => {
                EngineErrorCode::from_static("MODELS.QWEN_TEXT_EMBEDDING_MISSING_ENDPOINT")
            }
        }
    }
}

/// Failure modes of the Qwen text backends (mirrors the `code(..)` suffixes
/// in `backends/qwen.ts`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum QwenTextFailure {
    /// HTTP transport failure.
    RequestFailed,
    /// Response body was not valid JSON.
    InvalidJson,
    /// Provider returned a non-2xx status.
    ApiError,
    /// Response JSON lacked the `data` array.
    MissingData,
    /// Response item had a non-integer index.
    InvalidIndex,
    /// Response index fell outside the input range.
    IndexOutOfRange,
    /// Response embedding was not a numeric array.
    InvalidVector,
    /// No API key was configured.
    MissingApiKey,
    /// No endpoint was configured.
    MissingEndpoint,
}

/// Failure modes of the Qwen3 VL backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum QwenVlFailure {
    /// HTTP transport failure.
    RequestFailed,
    /// Response body was not valid JSON.
    InvalidJson,
    /// Provider returned a non-2xx status.
    ApiError,
    /// Response JSON lacked the embeddings array.
    MissingEmbeddings,
    /// Response item was not an object.
    InvalidItem,
    /// Response index fell outside the input range.
    IndexOutOfRange,
    /// Response embedding was not a numeric array.
    InvalidVector,
    /// Image format outside jpeg/png/webp.
    UnsupportedImageFormat,
    /// More images than the model limit.
    TooManyImages,
    /// No API key was configured.
    MissingApiKey,
    /// No endpoint was configured.
    MissingEndpoint,
}

/// Maps a VL failure to its fully-qualified code.
#[must_use]
pub const fn qwen_vl_code(failure: QwenVlFailure) -> EngineErrorCode {
    match failure {
        QwenVlFailure::RequestFailed => {
            EngineErrorCode::from_static("MODELS.QWEN3_VL_EMBEDDING_REQUEST_FAILED")
        }
        QwenVlFailure::InvalidJson => {
            EngineErrorCode::from_static("MODELS.QWEN3_VL_EMBEDDING_INVALID_JSON")
        }
        QwenVlFailure::ApiError => {
            EngineErrorCode::from_static("MODELS.QWEN3_VL_EMBEDDING_API_ERROR")
        }
        QwenVlFailure::MissingEmbeddings => {
            EngineErrorCode::from_static("MODELS.QWEN3_VL_EMBEDDING_MISSING_EMBEDDINGS")
        }
        QwenVlFailure::InvalidItem => {
            EngineErrorCode::from_static("MODELS.QWEN3_VL_EMBEDDING_INVALID_ITEM")
        }
        QwenVlFailure::IndexOutOfRange => {
            EngineErrorCode::from_static("MODELS.QWEN3_VL_EMBEDDING_INDEX_OUT_OF_RANGE")
        }
        QwenVlFailure::InvalidVector => {
            EngineErrorCode::from_static("MODELS.QWEN3_VL_EMBEDDING_INVALID_VECTOR")
        }
        QwenVlFailure::UnsupportedImageFormat => {
            EngineErrorCode::from_static("MODELS.QWEN3_VL_EMBEDDING_UNSUPPORTED_IMAGE_FORMAT")
        }
        QwenVlFailure::TooManyImages => {
            EngineErrorCode::from_static("MODELS.QWEN3_VL_EMBEDDING_TOO_MANY_IMAGES")
        }
        QwenVlFailure::MissingApiKey => {
            EngineErrorCode::from_static("MODELS.QWEN3_VL_EMBEDDING_MISSING_API_KEY")
        }
        QwenVlFailure::MissingEndpoint => {
            EngineErrorCode::from_static("MODELS.QWEN3_VL_EMBEDDING_MISSING_ENDPOINT")
        }
    }
}

/// Display name used in VL error messages (mirrors TS).
pub const QWEN_VL_DISPLAY_NAME: &str = "Qwen3 VL embedding";

/// Typed model error: every `MODELS.*` wire code as an enum variant.
///
/// Backend response errors carry their message and context verbatim (those
/// embed per-request details); validation errors carry structured fields
/// and render canonical messages below. Either way the code is an
/// exhaustive `const` mapping — no runtime assembly.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ModelError {
    /// Catalog reference names no known model.
    #[error("unknown embedding model reference")]
    CatalogModelNotFound {
        /// The unknown reference the caller asked for.
        reference: String,
    },
    /// Catalog entry has no backend implementation yet.
    #[error("embedding backend not implemented")]
    NotImplemented {
        /// Catalog reference that was requested.
        reference: String,
        /// Backend name from the catalog entry.
        backend: &'static str,
    },
    /// Backend exists but was compiled out (see the `onnx`/`llama` features).
    #[error("embedding backend not compiled in")]
    BackendUnavailable {
        /// Catalog reference that was requested.
        reference: String,
        /// Backend that would serve it.
        backend: BackendKind,
    },
    /// Qwen text backend failure; message/context carry request details.
    #[error("{message}")]
    QwenText {
        /// Which text model failed (selects the code prefix).
        model: QwenTextModel,
        /// What went wrong (selects the code suffix).
        failure: QwenTextFailure,
        /// Human-readable message (mirrors the TS backend text).
        message: String,
        /// `key=value` detail block.
        context: String,
    },
    /// Qwen3 VL backend failure; message/context carry request details.
    #[error("{message}")]
    QwenVl {
        /// What went wrong (selects the code).
        failure: QwenVlFailure,
        /// Human-readable message (mirrors the TS backend text).
        message: String,
        /// `key=value` detail block.
        context: String,
    },
    /// No API key for a Qwen backend (TS-exact message and hint).
    #[error("{message}")]
    MissingApiKey {
        /// Catalog reference being planned.
        reference: String,
        /// Backend kind for the code prefix and display name.
        backend: QwenBackend,
        /// Rendered message including the display name.
        message: String,
        /// `model=` line plus the config hint.
        context: String,
    },
    /// No endpoint for a Qwen backend.
    #[error("{message}")]
    MissingEndpoint {
        /// Catalog reference being planned.
        reference: String,
        /// Backend kind for the code prefix.
        backend: QwenBackend,
        /// Rendered message including the display name.
        message: String,
        /// `model=` detail line.
        context: String,
    },
    /// Batch was empty.
    #[error("embedding input is empty")]
    EmptyInput {
        /// Model reference from validation.
        reference: String,
    },
    /// Batch exceeded the model limit.
    #[error("embedding batch exceeds model limit")]
    BatchTooLarge {
        /// Model reference from validation.
        reference: String,
        /// Inputs the caller passed.
        size: usize,
        /// Model maximum.
        max: usize,
    },
    /// A text input was blank.
    #[error("embedding text input is empty")]
    EmptyText {
        /// Model reference from validation.
        reference: String,
        /// Index of the offending input.
        index: usize,
    },
    /// Image input reached a backend that rejects images, or validation
    /// rejected one (`index` is `None` when the backend no longer knows it).
    #[error("model does not support image input")]
    UnsupportedImage {
        /// Model reference from validation.
        reference: String,
        /// Index of the offending input, when known.
        index: Option<usize>,
    },
    /// An image input was empty.
    #[error("embedding image input is empty")]
    EmptyImage {
        /// Model reference from validation.
        reference: String,
        /// Index of the offending input.
        index: usize,
    },
    /// Image bytes exceeded the model limit.
    #[error("embedding image content exceeds model limit")]
    ImageTooLarge {
        /// Model reference from validation.
        reference: String,
        /// Index of the offending input.
        index: usize,
        /// Image size in bytes.
        bytes: u64,
        /// Model maximum in bytes.
        max_bytes: u64,
    },
    /// Backend returned the wrong vector count.
    #[error("backend returned wrong vector count")]
    VectorCountMismatch {
        /// Model reference from validation.
        reference: String,
        /// Inputs the caller passed.
        expected: usize,
        /// Vectors the backend returned.
        actual: usize,
    },
    /// Backend returned a vector with the wrong dimension.
    #[error("backend returned wrong vector dimension")]
    DimensionMismatch {
        /// Model reference from validation.
        reference: String,
        /// Index of the offending vector.
        vector_index: usize,
        /// Model dimension.
        expected: usize,
        /// Returned dimension.
        actual: usize,
    },
    /// Backend returned a non-finite vector component.
    #[error("backend returned non-finite vector value")]
    NonFiniteValue {
        /// Model reference from validation.
        reference: String,
        /// Index of the offending vector.
        vector_index: usize,
        /// Index of the offending component.
        value_index: usize,
    },
    /// Backend returned an out-of-range truncation index.
    #[error("backend returned invalid truncated index")]
    InvalidTruncatedIndex {
        /// Model reference from validation.
        reference: String,
        /// The offending index.
        index: usize,
        /// Inputs the caller passed.
        input_count: usize,
    },
    /// Model2Vec weights/tokenizer failed to load.
    #[error("unable to load Model2Vec model")]
    Model2VecLoad {
        /// Catalog reference being loaded.
        reference: String,
        /// Repository the artifact comes from.
        repo: String,
        /// Pinned revision.
        revision: String,
        /// What failed.
        detail: String,
    },
    /// Model2Vec artifact download failed.
    #[error("unable to download Model2Vec model artifact")]
    Model2VecDownload {
        /// Catalog reference being loaded.
        reference: String,
        /// Repository the artifact comes from.
        repo: String,
        /// Pinned revision.
        revision: String,
        /// What failed.
        detail: String,
    },
    /// Model2Vec embedding run failed.
    #[error("Model2Vec embedding failed")]
    Model2VecEmbed {
        /// Catalog reference being embedded with.
        reference: String,
        /// Repository the weights come from.
        repo: String,
        /// What failed.
        detail: String,
    },
    /// Model2Vec tokenization failed.
    #[error("Model2Vec tokenization failed")]
    Model2VecTokenize {
        /// Catalog reference being embedded with.
        reference: String,
        /// What failed.
        detail: String,
    },
    /// Tokenizer returned an id outside the embedding table.
    #[error("tokenizer returned out-of-range token id")]
    TokenOutOfRange {
        /// Catalog reference being embedded with.
        reference: String,
        /// The offending token id.
        id: u32,
        /// Table row count.
        rows: usize,
    },
    /// A Model2Vec worker thread failed.
    #[error("Model2Vec worker thread failed")]
    WorkerFailed {
        /// Catalog reference being embedded with.
        reference: String,
    },
    /// Transformers.js (ONNX) load or inference failure.
    #[error("Transformers.js embedding failed")]
    TransformersJsEmbed {
        /// Catalog reference being embedded with.
        reference: String,
        /// Repository the ONNX artifact comes from.
        repo: String,
        /// What failed.
        detail: String,
    },
    /// Transformers.js tokenization failure.
    #[error("Transformers.js tokenization failed")]
    TransformersJsTokenize {
        /// Catalog reference being embedded with.
        reference: String,
        /// Repository the tokenizer comes from.
        repo: String,
        /// What failed.
        detail: String,
    },
    /// Transformers.js returned a misshapen or non-finite tensor.
    #[error("Transformers.js returned an unexpected tensor")]
    TransformersJsInvalidTensor {
        /// Catalog reference being embedded with.
        reference: String,
        /// Expected vs actual shape, or the offending index.
        detail: String,
    },
    /// Transformers.js model used after disposal.
    #[error("Transformers.js embedding model is disposed")]
    TransformersJsDisposed {
        /// Catalog reference being embedded with.
        reference: String,
    },
    /// llama.cpp load or inference failure.
    #[error("llama.cpp embedding failed")]
    LlamaCppEmbed {
        /// Catalog reference being embedded with.
        reference: String,
        /// What failed.
        detail: String,
    },
    /// llama.cpp model used after disposal.
    #[error("llama.cpp embedding model is disposed")]
    LlamaCppDisposed {
        /// Catalog reference being embedded with.
        reference: String,
    },
    /// Downloaded file is not a GGUF model (deleted on detection, like TS).
    #[error("Local embedding model is not a valid GGUF file")]
    LlamaCppInvalidGguf {
        /// Catalog reference being loaded.
        reference: String,
        /// Cache path of the rejected file.
        path: String,
        /// Expected vs actual magic plus size.
        detail: String,
    },
    /// Downloaded file is an HTML error page, not a GGUF model.
    #[error("Downloaded local embedding model is HTML, not GGUF")]
    LlamaCppInvalidGgufHtml {
        /// Catalog reference being loaded.
        reference: String,
        /// Cache path of the rejected file.
        path: String,
    },
    /// Model artifact download failed (detail carries url/repo context).
    #[error("model download failed")]
    DownloadFailed {
        /// `key=value` detail block.
        context: String,
    },
}

/// Qwen backend identity for configuration errors (key/endpoint).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum QwenBackend {
    /// Qwen text backend with its model-specific prefix.
    Text(QwenTextModel),
    /// Qwen3 VL backend.
    Vl,
}

impl QwenBackend {
    /// Display name used in messages (mirrors TS `displayName`).
    #[must_use]
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Text(model) => model.display_name(),
            Self::Vl => QWEN_VL_DISPLAY_NAME,
        }
    }
}

impl ModelError {
    /// Exhaustive mapping from variant to wire code.
    #[must_use]
    pub const fn code(&self) -> EngineErrorCode {
        match self {
            Self::CatalogModelNotFound { .. } => {
                EngineErrorCode::from_static("MODELS.EMBEDDING_CATALOG_MODEL_NOT_FOUND")
            }
            Self::NotImplemented { .. } => {
                EngineErrorCode::from_static("MODELS.EMBEDDING_MODEL_NOT_IMPLEMENTED")
            }
            Self::BackendUnavailable { .. } => {
                EngineErrorCode::from_static("MODELS.EMBEDDING_BACKEND_UNAVAILABLE")
            }
            Self::QwenText { model, failure, .. } => model.code(*failure),
            Self::QwenVl { failure, .. } => qwen_vl_code(*failure),
            Self::MissingApiKey { backend, .. } => match backend {
                QwenBackend::Text(model) => model.code(QwenTextFailure::MissingApiKey),
                QwenBackend::Vl => qwen_vl_code(QwenVlFailure::MissingApiKey),
            },
            Self::MissingEndpoint { backend, .. } => match backend {
                QwenBackend::Text(model) => model.code(QwenTextFailure::MissingEndpoint),
                QwenBackend::Vl => qwen_vl_code(QwenVlFailure::MissingEndpoint),
            },
            Self::EmptyInput { .. } => EngineErrorCode::from_static("MODELS.EMBEDDING_EMPTY_INPUT"),
            Self::BatchTooLarge { .. } => {
                EngineErrorCode::from_static("MODELS.EMBEDDING_BATCH_TOO_LARGE")
            }
            Self::EmptyText { .. } => EngineErrorCode::from_static("MODELS.EMBEDDING_EMPTY_TEXT"),
            Self::UnsupportedImage { .. } => {
                EngineErrorCode::from_static("MODELS.EMBEDDING_UNSUPPORTED_CONTENT")
            }
            Self::EmptyImage { .. } => EngineErrorCode::from_static("MODELS.EMBEDDING_EMPTY_IMAGE"),
            Self::ImageTooLarge { .. } => {
                EngineErrorCode::from_static("MODELS.EMBEDDING_IMAGE_TOO_LARGE")
            }
            Self::VectorCountMismatch { .. } => {
                EngineErrorCode::from_static("MODELS.EMBEDDING_VECTOR_COUNT_MISMATCH")
            }
            Self::DimensionMismatch { .. } => {
                EngineErrorCode::from_static("MODELS.EMBEDDING_DIMENSION_MISMATCH")
            }
            Self::NonFiniteValue { .. } => {
                EngineErrorCode::from_static("MODELS.EMBEDDING_NON_FINITE_VECTOR_VALUE")
            }
            Self::InvalidTruncatedIndex { .. } => {
                EngineErrorCode::from_static("MODELS.EMBEDDING_INVALID_TRUNCATED_INPUT_INDEX")
            }
            Self::Model2VecLoad { .. } => {
                EngineErrorCode::from_static("MODELS.MODEL2VEC_LOAD_FAILED")
            }
            Self::Model2VecDownload { .. } => {
                EngineErrorCode::from_static("MODELS.MODEL2VEC_DOWNLOAD_FAILED")
            }
            Self::Model2VecEmbed { .. } => {
                EngineErrorCode::from_static("MODELS.MODEL2VEC_EMBED_FAILED")
            }
            Self::Model2VecTokenize { .. } => {
                EngineErrorCode::from_static("MODELS.MODEL2VEC_EMBED_FAILED")
            }
            Self::TokenOutOfRange { .. } => {
                EngineErrorCode::from_static("MODELS.MODEL2VEC_EMBED_FAILED")
            }
            Self::WorkerFailed { .. } => {
                EngineErrorCode::from_static("MODELS.MODEL2VEC_EMBED_FAILED")
            }
            Self::TransformersJsEmbed { .. } => {
                EngineErrorCode::from_static("MODELS.TRANSFORMERS_JS_EMBED_FAILED")
            }
            Self::TransformersJsTokenize { .. } => {
                EngineErrorCode::from_static("MODELS.TRANSFORMERS_JS_TOKENIZATION_FAILED")
            }
            Self::TransformersJsInvalidTensor { .. } => {
                EngineErrorCode::from_static("MODELS.TRANSFORMERS_JS_INVALID_TENSOR")
            }
            Self::TransformersJsDisposed { .. } => {
                EngineErrorCode::from_static("MODELS.TRANSFORMERS_JS_DISPOSED")
            }
            Self::LlamaCppEmbed { .. } => {
                EngineErrorCode::from_static("MODELS.LLAMA_CPP_EMBED_FAILED")
            }
            Self::LlamaCppDisposed { .. } => {
                EngineErrorCode::from_static("MODELS.LLAMA_CPP_DISPOSED")
            }
            Self::LlamaCppInvalidGguf { .. } => {
                EngineErrorCode::from_static("MODELS.LLAMA_CPP_INVALID_GGUF")
            }
            Self::LlamaCppInvalidGgufHtml { .. } => {
                EngineErrorCode::from_static("MODELS.LLAMA_CPP_INVALID_GGUF_HTML")
            }
            Self::DownloadFailed { .. } => {
                EngineErrorCode::from_static("MODELS.MODEL_DOWNLOAD_FAILED")
            }
        }
    }

    /// Missing-key error with the TS-exact message and hint.
    #[must_use]
    pub fn missing_api_key(reference: &str, backend: QwenBackend) -> Self {
        let display = backend.display_name();
        Self::MissingApiKey {
            reference: reference.to_owned(),
            backend,
            message: format!("{display} model requires an API key"),
            context: format!(
                "model={reference}\nhint=Pass --api-key, set ZVEC_GREP_API_KEY, or configure providers.qwen.apiKey in {}.",
                global_config_path().display()
            ),
        }
    }

    /// Missing-endpoint error with the TS-exact message.
    #[must_use]
    pub fn missing_endpoint(reference: &str, backend: QwenBackend) -> Self {
        let display = backend.display_name();
        Self::MissingEndpoint {
            reference: reference.to_owned(),
            backend,
            message: format!("{display} model requires an endpoint"),
            context: format!("model={reference}"),
        }
    }
}

impl From<ModelError> for EngineError {
    fn from(error: ModelError) -> Self {
        let code = error.code();
        let message = error.to_string();
        let context: Option<String> = match error {
            ModelError::CatalogModelNotFound { reference } => {
                Some(format!("embedding={reference}"))
            }
            ModelError::NotImplemented { reference, backend } => {
                Some(format!("reference={reference} backend={backend}"))
            }
            ModelError::BackendUnavailable { reference, backend } => Some(format!(
                "reference={reference} backend={}",
                backend.as_str()
            )),
            ModelError::QwenText { context, .. } | ModelError::QwenVl { context, .. } => {
                Some(context)
            }
            ModelError::MissingApiKey { context, .. }
            | ModelError::MissingEndpoint { context, .. } => Some(context),
            ModelError::EmptyInput { reference } => Some(format!("model={reference}")),
            ModelError::BatchTooLarge {
                reference,
                size,
                max,
            } => Some(format!(
                "model={reference} batchSize={size} maxBatchSize={max}"
            )),
            ModelError::EmptyText { reference, index } => {
                Some(format!("model={reference} index={index}"))
            }
            ModelError::UnsupportedImage { reference, index } => Some(match index {
                Some(index) => format!("model={reference} index={index} kind=image"),
                None => format!("model={reference}"),
            }),
            ModelError::EmptyImage { reference, index } => {
                Some(format!("model={reference} index={index}"))
            }
            ModelError::ImageTooLarge {
                reference,
                index,
                bytes,
                max_bytes,
            } => Some(format!(
                "model={reference} index={index} imageBytes={bytes} maxImageBytes={max_bytes}"
            )),
            ModelError::VectorCountMismatch {
                reference,
                expected,
                actual,
            } => Some(format!(
                "model={reference} contentCount={expected} vectorCount={actual}"
            )),
            ModelError::DimensionMismatch {
                reference,
                vector_index,
                expected,
                actual,
            } => Some(format!(
                "model={reference} vectorIndex={vector_index} expectedDimension={expected} actualDimension={actual}"
            )),
            ModelError::NonFiniteValue {
                reference,
                vector_index,
                value_index,
            } => Some(format!(
                "model={reference} vectorIndex={vector_index} valueIndex={value_index}"
            )),
            ModelError::InvalidTruncatedIndex {
                reference,
                index,
                input_count,
            } => Some(format!(
                "model={reference} index={index} inputCount={input_count}"
            )),
            ModelError::Model2VecLoad {
                reference,
                repo,
                revision,
                detail,
            } => Some(format!(
                "model={reference} repo={repo} revision={revision} detail={detail}"
            )),
            ModelError::Model2VecDownload {
                reference,
                repo,
                revision,
                detail,
            } => Some(format!(
                "model={reference} repo={repo} revision={revision} detail={detail}"
            )),
            ModelError::Model2VecEmbed {
                reference,
                repo,
                detail,
            } => Some(format!("model={reference} repo={repo} detail={detail}")),
            ModelError::Model2VecTokenize { reference, detail } => {
                Some(format!("model={reference} detail={detail}"))
            }
            ModelError::TokenOutOfRange {
                reference,
                id,
                rows,
            } => Some(format!("model={reference} id={id} rows={rows}")),
            ModelError::WorkerFailed { reference } => Some(format!("model={reference}")),
            ModelError::TransformersJsEmbed {
                reference,
                repo,
                detail,
            } => Some(format!("model={reference} repo={repo} detail={detail}")),
            ModelError::TransformersJsTokenize {
                reference,
                repo,
                detail,
            } => Some(format!("model={reference} repo={repo} detail={detail}")),
            ModelError::TransformersJsInvalidTensor { reference, detail } => {
                Some(format!("model={reference} detail={detail}"))
            }
            ModelError::TransformersJsDisposed { reference } => Some(format!("model={reference}")),
            ModelError::LlamaCppEmbed { reference, detail } => {
                Some(format!("model={reference} detail={detail}"))
            }
            ModelError::LlamaCppDisposed { reference } => Some(format!("model={reference}")),
            ModelError::LlamaCppInvalidGguf {
                reference,
                path,
                detail,
            } => Some(format!("model={reference} path={path} detail={detail}")),
            ModelError::LlamaCppInvalidGgufHtml { reference, path } => {
                Some(format!("model={reference} path={path}"))
            }
            ModelError::DownloadFailed { context } => Some(context),
        };
        let mut engine = EngineError::new(code, message);
        if let Some(context) = context {
            engine = engine.with_context(context);
        }
        engine
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference() -> String {
        "local/test-model".to_owned()
    }

    /// Golden wire codes: every variant renders the exact TS-true
    /// `ZVEC_GREP.ENGINE.*` code and message. Any drift fails here.
    #[test]
    fn wire_codes_are_frozen() {
        let cases: Vec<(ModelError, &str, &str)> = vec![
            (
                ModelError::CatalogModelNotFound {
                    reference: reference(),
                },
                "ZVEC_GREP.ENGINE.MODELS.EMBEDDING_CATALOG_MODEL_NOT_FOUND",
                "unknown embedding model reference",
            ),
            (
                ModelError::NotImplemented {
                    reference: reference(),
                    backend: "qwen",
                },
                "ZVEC_GREP.ENGINE.MODELS.EMBEDDING_MODEL_NOT_IMPLEMENTED",
                "embedding backend not implemented",
            ),
            (
                ModelError::BackendUnavailable {
                    reference: reference(),
                    backend: BackendKind::Qwen,
                },
                "ZVEC_GREP.ENGINE.MODELS.EMBEDDING_BACKEND_UNAVAILABLE",
                "embedding backend not compiled in",
            ),
            (
                ModelError::QwenText {
                    model: QwenTextModel::V4,
                    failure: QwenTextFailure::RequestFailed,
                    message: "boom".to_owned(),
                    context: "model=x".to_owned(),
                },
                "ZVEC_GREP.ENGINE.MODELS.QWEN_TEXT_EMBEDDING_V4_REQUEST_FAILED",
                "boom",
            ),
            (
                ModelError::QwenVl {
                    failure: QwenVlFailure::ApiError,
                    message: "boom".to_owned(),
                    context: "model=x".to_owned(),
                },
                "ZVEC_GREP.ENGINE.MODELS.QWEN3_VL_EMBEDDING_API_ERROR",
                "boom",
            ),
            (
                ModelError::missing_api_key("r", QwenBackend::Text(QwenTextModel::V4)),
                "ZVEC_GREP.ENGINE.MODELS.QWEN_TEXT_EMBEDDING_V4_MISSING_API_KEY",
                "Qwen text-embedding-v4 model requires an API key",
            ),
            (
                ModelError::missing_endpoint("r", QwenBackend::Vl),
                "ZVEC_GREP.ENGINE.MODELS.QWEN3_VL_EMBEDDING_MISSING_ENDPOINT",
                "Qwen3 VL embedding model requires an endpoint",
            ),
            (
                ModelError::EmptyInput {
                    reference: reference(),
                },
                "ZVEC_GREP.ENGINE.MODELS.EMBEDDING_EMPTY_INPUT",
                "embedding input is empty",
            ),
            (
                ModelError::BatchTooLarge {
                    reference: reference(),
                    size: 40,
                    max: 32,
                },
                "ZVEC_GREP.ENGINE.MODELS.EMBEDDING_BATCH_TOO_LARGE",
                "embedding batch exceeds model limit",
            ),
            (
                ModelError::EmptyText {
                    reference: reference(),
                    index: 2,
                },
                "ZVEC_GREP.ENGINE.MODELS.EMBEDDING_EMPTY_TEXT",
                "embedding text input is empty",
            ),
            (
                ModelError::UnsupportedImage {
                    reference: reference(),
                    index: Some(1),
                },
                "ZVEC_GREP.ENGINE.MODELS.EMBEDDING_UNSUPPORTED_CONTENT",
                "model does not support image input",
            ),
            (
                ModelError::EmptyImage {
                    reference: reference(),
                    index: 0,
                },
                "ZVEC_GREP.ENGINE.MODELS.EMBEDDING_EMPTY_IMAGE",
                "embedding image input is empty",
            ),
            (
                ModelError::ImageTooLarge {
                    reference: reference(),
                    index: 0,
                    bytes: 9,
                    max_bytes: 8,
                },
                "ZVEC_GREP.ENGINE.MODELS.EMBEDDING_IMAGE_TOO_LARGE",
                "embedding image content exceeds model limit",
            ),
            (
                ModelError::VectorCountMismatch {
                    reference: reference(),
                    expected: 2,
                    actual: 1,
                },
                "ZVEC_GREP.ENGINE.MODELS.EMBEDDING_VECTOR_COUNT_MISMATCH",
                "backend returned wrong vector count",
            ),
            (
                ModelError::DimensionMismatch {
                    reference: reference(),
                    vector_index: 0,
                    expected: 4,
                    actual: 3,
                },
                "ZVEC_GREP.ENGINE.MODELS.EMBEDDING_DIMENSION_MISMATCH",
                "backend returned wrong vector dimension",
            ),
            (
                ModelError::NonFiniteValue {
                    reference: reference(),
                    vector_index: 0,
                    value_index: 3,
                },
                "ZVEC_GREP.ENGINE.MODELS.EMBEDDING_NON_FINITE_VECTOR_VALUE",
                "backend returned non-finite vector value",
            ),
            (
                ModelError::InvalidTruncatedIndex {
                    reference: reference(),
                    index: 7,
                    input_count: 2,
                },
                "ZVEC_GREP.ENGINE.MODELS.EMBEDDING_INVALID_TRUNCATED_INPUT_INDEX",
                "backend returned invalid truncated index",
            ),
            (
                ModelError::Model2VecLoad {
                    reference: reference(),
                    repo: "repo".to_owned(),
                    revision: "rev".to_owned(),
                    detail: "d".to_owned(),
                },
                "ZVEC_GREP.ENGINE.MODELS.MODEL2VEC_LOAD_FAILED",
                "unable to load Model2Vec model",
            ),
            (
                ModelError::Model2VecDownload {
                    reference: reference(),
                    repo: "repo".to_owned(),
                    revision: "rev".to_owned(),
                    detail: "d".to_owned(),
                },
                "ZVEC_GREP.ENGINE.MODELS.MODEL2VEC_DOWNLOAD_FAILED",
                "unable to download Model2Vec model artifact",
            ),
            (
                ModelError::Model2VecEmbed {
                    reference: reference(),
                    repo: "repo".to_owned(),
                    detail: "d".to_owned(),
                },
                "ZVEC_GREP.ENGINE.MODELS.MODEL2VEC_EMBED_FAILED",
                "Model2Vec embedding failed",
            ),
            (
                ModelError::Model2VecTokenize {
                    reference: reference(),
                    detail: "d".to_owned(),
                },
                "ZVEC_GREP.ENGINE.MODELS.MODEL2VEC_EMBED_FAILED",
                "Model2Vec tokenization failed",
            ),
            (
                ModelError::TokenOutOfRange {
                    reference: reference(),
                    id: 9,
                    rows: 4,
                },
                "ZVEC_GREP.ENGINE.MODELS.MODEL2VEC_EMBED_FAILED",
                "tokenizer returned out-of-range token id",
            ),
            (
                ModelError::WorkerFailed {
                    reference: reference(),
                },
                "ZVEC_GREP.ENGINE.MODELS.MODEL2VEC_EMBED_FAILED",
                "Model2Vec worker thread failed",
            ),
            (
                ModelError::TransformersJsEmbed {
                    reference: reference(),
                    repo: "repo".to_owned(),
                    detail: "d".to_owned(),
                },
                "ZVEC_GREP.ENGINE.MODELS.TRANSFORMERS_JS_EMBED_FAILED",
                "Transformers.js embedding failed",
            ),
            (
                ModelError::TransformersJsTokenize {
                    reference: reference(),
                    repo: "repo".to_owned(),
                    detail: "d".to_owned(),
                },
                "ZVEC_GREP.ENGINE.MODELS.TRANSFORMERS_JS_TOKENIZATION_FAILED",
                "Transformers.js tokenization failed",
            ),
            (
                ModelError::TransformersJsInvalidTensor {
                    reference: reference(),
                    detail: "d".to_owned(),
                },
                "ZVEC_GREP.ENGINE.MODELS.TRANSFORMERS_JS_INVALID_TENSOR",
                "Transformers.js returned an unexpected tensor",
            ),
            (
                ModelError::TransformersJsDisposed {
                    reference: reference(),
                },
                "ZVEC_GREP.ENGINE.MODELS.TRANSFORMERS_JS_DISPOSED",
                "Transformers.js embedding model is disposed",
            ),
            (
                ModelError::LlamaCppEmbed {
                    reference: reference(),
                    detail: "d".to_owned(),
                },
                "ZVEC_GREP.ENGINE.MODELS.LLAMA_CPP_EMBED_FAILED",
                "llama.cpp embedding failed",
            ),
            (
                ModelError::LlamaCppDisposed {
                    reference: reference(),
                },
                "ZVEC_GREP.ENGINE.MODELS.LLAMA_CPP_DISPOSED",
                "llama.cpp embedding model is disposed",
            ),
            (
                ModelError::LlamaCppInvalidGguf {
                    reference: reference(),
                    path: "p".to_owned(),
                    detail: "d".to_owned(),
                },
                "ZVEC_GREP.ENGINE.MODELS.LLAMA_CPP_INVALID_GGUF",
                "Local embedding model is not a valid GGUF file",
            ),
            (
                ModelError::LlamaCppInvalidGgufHtml {
                    reference: reference(),
                    path: "p".to_owned(),
                },
                "ZVEC_GREP.ENGINE.MODELS.LLAMA_CPP_INVALID_GGUF_HTML",
                "Downloaded local embedding model is HTML, not GGUF",
            ),
            (
                ModelError::DownloadFailed {
                    context: "url=u".to_owned(),
                },
                "ZVEC_GREP.ENGINE.MODELS.MODEL_DOWNLOAD_FAILED",
                "model download failed",
            ),
        ];
        for (error, code, message) in cases {
            let engine = EngineError::from(error);
            assert_eq!(engine.code().to_string(), code);
            assert_eq!(engine.message(), message);
        }
    }

    /// Every text failure has a frozen code under every model prefix.
    #[test]
    fn qwen_text_codes_cover_every_failure() {
        let failures = [
            (QwenTextFailure::RequestFailed, "REQUEST_FAILED"),
            (QwenTextFailure::InvalidJson, "INVALID_JSON"),
            (QwenTextFailure::ApiError, "API_ERROR"),
            (QwenTextFailure::MissingData, "MISSING_DATA"),
            (QwenTextFailure::InvalidIndex, "INVALID_INDEX"),
            (QwenTextFailure::IndexOutOfRange, "INDEX_OUT_OF_RANGE"),
            (QwenTextFailure::InvalidVector, "INVALID_VECTOR"),
            (QwenTextFailure::MissingApiKey, "MISSING_API_KEY"),
            (QwenTextFailure::MissingEndpoint, "MISSING_ENDPOINT"),
        ];
        for (failure, suffix) in failures {
            let code = QwenTextModel::V4.code(failure).to_string();
            assert_eq!(
                code,
                format!("ZVEC_GREP.ENGINE.MODELS.QWEN_TEXT_EMBEDDING_V4_{suffix}")
            );
            let code = QwenTextModel::V37.code(failure).to_string();
            assert_eq!(
                code,
                format!("ZVEC_GREP.ENGINE.MODELS.QWEN37_TEXT_EMBEDDING_{suffix}")
            );
            let code = QwenTextModel::Other.code(failure).to_string();
            assert_eq!(
                code,
                format!("ZVEC_GREP.ENGINE.MODELS.QWEN_TEXT_EMBEDDING_{suffix}")
            );
        }
    }

    /// Every VL failure has a frozen code.
    #[test]
    fn qwen_vl_codes_cover_every_failure() {
        let failures = [
            (QwenVlFailure::RequestFailed, "REQUEST_FAILED"),
            (QwenVlFailure::InvalidJson, "INVALID_JSON"),
            (QwenVlFailure::ApiError, "API_ERROR"),
            (QwenVlFailure::MissingEmbeddings, "MISSING_EMBEDDINGS"),
            (QwenVlFailure::InvalidItem, "INVALID_ITEM"),
            (QwenVlFailure::IndexOutOfRange, "INDEX_OUT_OF_RANGE"),
            (QwenVlFailure::InvalidVector, "INVALID_VECTOR"),
            (
                QwenVlFailure::UnsupportedImageFormat,
                "UNSUPPORTED_IMAGE_FORMAT",
            ),
            (QwenVlFailure::TooManyImages, "TOO_MANY_IMAGES"),
            (QwenVlFailure::MissingApiKey, "MISSING_API_KEY"),
            (QwenVlFailure::MissingEndpoint, "MISSING_ENDPOINT"),
        ];
        for (failure, suffix) in failures {
            let code = qwen_vl_code(failure).to_string();
            assert_eq!(
                code,
                format!("ZVEC_GREP.ENGINE.MODELS.QWEN3_VL_EMBEDDING_{suffix}")
            );
        }
    }

    #[test]
    fn text_model_follows_catalog_entry() {
        assert_eq!(
            QwenTextModel::from_model_id("text-embedding-v4"),
            QwenTextModel::V4
        );
        assert_eq!(
            QwenTextModel::from_model_id("qwen3.7-text-embedding"),
            QwenTextModel::V37
        );
        assert_eq!(
            QwenTextModel::from_model_id("future-model"),
            QwenTextModel::Other
        );
    }

    #[test]
    fn unsupported_image_context_names_index() {
        let engine = EngineError::from(ModelError::UnsupportedImage {
            reference: reference(),
            index: Some(1),
        });
        assert_eq!(
            engine.context(),
            Some("model=local/test-model index=1 kind=image")
        );
        let engine = EngineError::from(ModelError::UnsupportedImage {
            reference: reference(),
            index: None,
        });
        assert_eq!(engine.context(), Some("model=local/test-model"));
    }
}
