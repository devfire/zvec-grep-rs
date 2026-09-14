//! Validation, shape-check, backend-artifact, and download failures.
//!
//! Owns the [`ModelError`] → [`EngineError`]
//! rendering: every variant's canonical `key=value` context block. The
//! wire strings are unchanged from TypeScript; the context match is
//! exhaustive, so a new variant without rendering fails to compile.

use super::ModelError;
use crate::error::EngineError;

impl From<ModelError> for EngineError {
    fn from(error: ModelError) -> Self {
        let code = error.code();
        let message = error.to_string();
        // The typed enum is the cause: io/serde/HTTP details are already
        // stringified into `context` upstream (e.g. `download.rs`), so the
        // chain `EngineError -> ModelError` stays walkable without
        // re-threading every leaf. `Clone` (not a borrow) because the match
        // below consumes `error` for the context rendering.
        let source = error.clone();
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
        let mut engine = EngineError::new(code, message).with_source(source);
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

    /// Frozen wire codes for the validation / shape-check / backend-artifact
    /// / download groups (split out of the golden table so each module guards
    /// its own codes; union covers all variants).
    #[test]
    fn validation_wire_codes_are_frozen() {
        let cases: Vec<(ModelError, &str, &str)> = vec![
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
