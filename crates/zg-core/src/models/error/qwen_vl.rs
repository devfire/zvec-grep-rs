//! Qwen3 VL backend errors: failure modes, wire codes, and display name.

use crate::error::EngineErrorCode;

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

#[cfg(test)]
mod tests {
    use super::super::{ModelError, QwenBackend};
    use super::*;
    use crate::error::EngineError;

    /// Frozen wire codes for the Qwen VL group (split out of the golden
    /// table so each module guards its own codes; union covers all variants).
    #[test]
    fn qwen_vl_wire_codes_are_frozen() {
        let cases: Vec<(ModelError, &str, &str)> = vec![
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
                ModelError::missing_endpoint("r", QwenBackend::Vl),
                "ZVEC_GREP.ENGINE.MODELS.QWEN3_VL_EMBEDDING_MISSING_ENDPOINT",
                "Qwen3 VL embedding model requires an endpoint",
            ),
        ];
        for (error, code, message) in cases {
            let engine = EngineError::from(error);
            assert_eq!(engine.code().to_string(), code);
            assert_eq!(engine.message(), message);
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
}
