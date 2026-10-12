//! Validation, shape-check, backend-artifact, and download failures.
//!
//! Owns the [`ModelError`] → [`EngineError`]
//! rendering: every variant's canonical `key=value` context block. The
//! wire strings are unchanged from TypeScript; the context match is
//! exhaustive, so a new variant without rendering fails to compile.

use super::ModelError;
// The four width-critical single-line arms below match bare; the rest stay
// qualified. (A full variant list would push this module past its LOC budget.)
use super::ModelError::{LlamaCppEmbed, LlamaCppInvalidGgufHtml, NotImplemented, UnsupportedImage};
use crate::error::EngineError;

fn model_only(reference: &str) -> String {
    format!("model={reference}")
}

fn model_detail(reference: &str, detail: &str) -> String {
    format!("model={reference} detail={detail}")
}

fn model_repo_detail(reference: &str, repo: &str, detail: &str) -> String {
    format!("model={reference} repo={repo} detail={detail}")
}

fn model_revision_detail(reference: &str, repo: &str, revision: &str, detail: &str) -> String {
    format!("model={reference} repo={repo} revision={revision} detail={detail}")
}

fn model_index(reference: &str, index: usize) -> String {
    format!("model={reference} index={index}")
}

fn unsupported_image(reference: &str, index: Option<usize>) -> String {
    match index {
        Some(index) => format!("model={reference} index={index} kind=image"),
        None => format!("model={reference}"),
    }
}

fn embedding_context(reference: &str) -> String {
    format!("embedding={reference}")
}

fn backend_context(reference: &str, backend: &str) -> String {
    format!("reference={reference} backend={backend}")
}

fn batch_context(reference: &str, size: usize, max: usize) -> String {
    format!("model={reference} batchSize={size} maxBatchSize={max}")
}

fn image_too_large_context(reference: &str, index: usize, bytes: u64, max_bytes: u64) -> String {
    format!("model={reference} index={index} imageBytes={bytes} maxImageBytes={max_bytes}")
}

fn vector_count_context(reference: &str, expected: usize, actual: usize) -> String {
    format!("model={reference} contentCount={expected} vectorCount={actual}")
}

fn dim_context(reference: &str, vector_index: usize, expected: usize, actual: usize) -> String {
    format!(
        "model={reference} vectorIndex={vector_index} expectedDimension={expected} actualDimension={actual}"
    )
}

fn non_finite_context(reference: &str, vector_index: usize, value_index: usize) -> String {
    format!("model={reference} vectorIndex={vector_index} valueIndex={value_index}")
}

fn truncated_index_context(reference: &str, index: usize, input_count: usize) -> String {
    format!("model={reference} index={index} inputCount={input_count}")
}

fn gguf_html(reference: &str, path: &str) -> String {
    format!("model={reference} path={path}")
}

impl From<ModelError> for EngineError {
    fn from(error: ModelError) -> Self {
        let (code, message, source) = (error.code(), error.to_string(), error.clone());
        // The typed enum is the cause: io/serde/HTTP details are already
        // stringified into `context` upstream (e.g. `download.rs`), so the
        // chain `EngineError -> ModelError` stays walkable without
        // re-threading every leaf. `Clone` (not a borrow) because the match
        // below consumes `error` for the context rendering.
        let context: Option<String> = match error {
            ModelError::CatalogModelNotFound { reference } => Some(embedding_context(&reference)),
            NotImplemented { reference, backend } => Some(backend_context(&reference, backend)),
            ModelError::BackendUnavailable { reference, backend } => {
                Some(backend_context(&reference, backend.as_str()))
            }
            ModelError::QwenText { context, .. }
            | ModelError::QwenVl { context, .. }
            | ModelError::DownloadFailed { context }
            | ModelError::MissingApiKey { context, .. }
            | ModelError::MissingEndpoint { context, .. } => Some(context),
            ModelError::EmptyInput { reference }
            | ModelError::WorkerFailed { reference }
            | ModelError::TransformersJsDisposed { reference }
            | ModelError::LlamaCppDisposed { reference } => Some(model_only(&reference)),
            ModelError::BatchTooLarge {
                reference,
                size,
                max,
            } => Some(batch_context(&reference, size, max)),
            ModelError::EmptyText { reference, index }
            | ModelError::EmptyImage { reference, index } => Some(model_index(&reference, index)),
            UnsupportedImage { reference, index } => Some(unsupported_image(&reference, index)),
            ModelError::ImageTooLarge {
                reference,
                index,
                bytes,
                max_bytes,
            } => Some(image_too_large_context(&reference, index, bytes, max_bytes)),
            ModelError::VectorCountMismatch {
                reference,
                expected,
                actual,
            } => Some(vector_count_context(&reference, expected, actual)),
            ModelError::DimensionMismatch {
                reference,
                vector_index,
                expected,
                actual,
            } => Some(dim_context(&reference, vector_index, expected, actual)),
            ModelError::NonFiniteValue {
                reference,
                vector_index,
                value_index,
            } => Some(non_finite_context(&reference, vector_index, value_index)),
            ModelError::InvalidTruncatedIndex {
                reference,
                index,
                input_count,
            } => Some(truncated_index_context(&reference, index, input_count)),
            ModelError::Model2VecLoad {
                reference,
                repo,
                revision,
                detail,
            }
            | ModelError::Model2VecDownload {
                reference,
                repo,
                revision,
                detail,
            } => Some(model_revision_detail(&reference, &repo, &revision, &detail)),
            ModelError::Model2VecEmbed {
                reference,
                repo,
                detail,
            }
            | ModelError::TransformersJsEmbed {
                reference,
                repo,
                detail,
            }
            | ModelError::TransformersJsTokenize {
                reference,
                repo,
                detail,
            } => Some(model_repo_detail(&reference, &repo, &detail)),
            ModelError::TokenOutOfRange {
                reference,
                id,
                rows,
            } => Some(format!("model={reference} id={id} rows={rows}")),
            ModelError::Model2VecTokenize { reference, detail }
            | ModelError::TransformersJsInvalidTensor { reference, detail }
            | LlamaCppEmbed { reference, detail } => Some(model_detail(&reference, &detail)),
            ModelError::LlamaCppInvalidGguf {
                reference,
                path,
                detail,
            } => Some(format!("model={reference} path={path} detail={detail}")),
            LlamaCppInvalidGgufHtml { reference, path } => Some(gguf_html(&reference, &path)),
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

    fn embedding_input_cases() -> Vec<(ModelError, &'static str, &'static str)> {
        vec![
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
                UnsupportedImage {
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
        ]
    }

    fn embedding_shape_cases() -> Vec<(ModelError, &'static str, &'static str)> {
        vec![
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
        ]
    }

    fn model2vec_cases() -> Vec<(ModelError, &'static str, &'static str)> {
        vec![
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
        ]
    }

    fn transformers_cases() -> Vec<(ModelError, &'static str, &'static str)> {
        vec![
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
        ]
    }

    fn llama_download_cases() -> Vec<(ModelError, &'static str, &'static str)> {
        vec![
            (
                LlamaCppEmbed {
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
                LlamaCppInvalidGgufHtml {
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
        ]
    }

    /// Frozen wire codes for the validation / shape-check / backend-artifact
    /// / download groups (split out of the golden table so each module guards
    /// its own codes; union covers all variants).
    #[test]
    fn validation_wire_codes_are_frozen() {
        let mut cases = embedding_input_cases();
        cases.extend(embedding_shape_cases());
        cases.extend(model2vec_cases());
        cases.extend(transformers_cases());
        cases.extend(llama_download_cases());
        for (error, code, message) in cases {
            let engine = EngineError::from(error);
            assert_eq!(engine.code().to_string(), code);
            assert_eq!(engine.message(), message);
        }
    }

    #[test]
    fn unsupported_image_names_index() {
        let engine = EngineError::from(UnsupportedImage {
            reference: reference(),
            index: Some(1),
        });
        assert_eq!(
            engine.context(),
            Some("model=local/test-model index=1 kind=image")
        );
        let engine = EngineError::from(UnsupportedImage {
            reference: reference(),
            index: None,
        });
        assert_eq!(engine.context(), Some("model=local/test-model"));
    }
}
