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

pub mod qwen_text;
pub mod qwen_vl;
pub mod validation;

pub use qwen_text::{QwenTextFailure, QwenTextModel};
pub use qwen_vl::{QWEN_VL_DISPLAY_NAME, QwenVlFailure, qwen_vl_code};

use super::catalog::BackendKind;
use crate::error::EngineErrorCode;

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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::EngineError;

    fn reference() -> String {
        "local/test-model".to_owned()
    }

    /// Frozen wire codes for the catalog group (split out of the golden
    /// table so each module guards its own codes; union covers all variants).
    #[test]
    fn catalog_wire_codes_are_frozen() {
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
        ];
        for (error, code, message) in cases {
            let engine = EngineError::from(error);
            assert_eq!(engine.code().to_string(), code);
            assert_eq!(engine.message(), message);
        }
    }
}
