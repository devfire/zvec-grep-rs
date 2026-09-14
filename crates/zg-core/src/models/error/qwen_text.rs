//! Qwen text-backend errors: model identity, failure modes, and wire codes.
//!
//! Qwen codes follow the TS `${prefix}_${suffix}` shape
//! (`backends/qwen.ts:226`), but each combination is an explicit match arm
//! below — never a runtime `format!`. The per-model prefixes
//! (`QWEN_TEXT_EMBEDDING_V4`, `QWEN37_TEXT_EMBEDDING`,
//! `QWEN3_VL_EMBEDDING`) are verified against the TS source; the
//! `QWEN_TEXT_EMBEDDING` fallback for future catalog entries is a Rust
//! addition (no TS equivalent) and is recorded in `docs/ts-divergence.md`.
//!
//! Also owns the Qwen configuration-error constructors
//! ([`ModelError::missing_api_key`], [`ModelError::missing_endpoint`]).

use super::{ModelError, QwenBackend};
use crate::error::EngineErrorCode;
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

impl ModelError {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::EngineError;

    /// Frozen wire codes for the Qwen text group (split out of the golden
    /// table so each module guards its own codes; union covers all variants).
    #[test]
    fn qwen_text_wire_codes_are_frozen() {
        let cases: Vec<(ModelError, &str, &str)> = vec![
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
                ModelError::missing_api_key("r", QwenBackend::Text(QwenTextModel::V4)),
                "ZVEC_GREP.ENGINE.MODELS.QWEN_TEXT_EMBEDDING_V4_MISSING_API_KEY",
                "Qwen text-embedding-v4 model requires an API key",
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
}
