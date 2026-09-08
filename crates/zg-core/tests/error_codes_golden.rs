//! Golden wire-code registry test (M1).
//!
//! Renders every `ZVEC_GREP.ENGINE.*` code the crate can produce — one live
//! value per `ModelError` variant (all Qwen model × failure combinations,
//! all backend kinds), every `codes::*` constructor, and the service error
//! helpers — sorts and dedupes them, and compares against
//! `tests/golden/error-codes.txt`.
//!
//! Update rule: adding an error variant or code without extending both this
//! list and the golden file fails the build. The `code()` matches themselves
//! are compiler-exhaustive, so a new variant also forces a mapping update.

use zg_core::authorization::{AuthError, RemoteEmbeddingPurpose};
use zg_core::error::codes;
use zg_core::models::catalog::BackendKind;
use zg_core::models::error::{
    ModelError, QwenBackend, QwenTextFailure, QwenTextModel, QwenVlFailure, qwen_vl_code,
};
use zg_core::service::types::{
    empty_query_error, workspace_index_disabled, workspace_index_not_found,
};

fn reference() -> String {
    "local/test-model".to_owned()
}

fn all_codes() -> Vec<String> {
    let mut out = Vec::new();
    let mut push = |code: zg_core::error::EngineErrorCode| out.push(code.qualified());

    // Every non-Qwen `ModelError` variant, in enum order.
    push(
        ModelError::CatalogModelNotFound {
            reference: reference(),
        }
        .code(),
    );
    push(
        ModelError::NotImplemented {
            reference: reference(),
            backend: "model2vec",
        }
        .code(),
    );
    for backend in [
        BackendKind::LlamaCpp,
        BackendKind::Qwen,
        BackendKind::TransformersJs,
        BackendKind::Model2Vec,
    ] {
        push(
            ModelError::BackendUnavailable {
                reference: reference(),
                backend,
            }
            .code(),
        );
    }
    push(ModelError::EmptyInput { reference: reference() }.code());
    push(
        ModelError::BatchTooLarge {
            reference: reference(),
            size: 40,
            max: 32,
        }
        .code(),
    );
    push(
        ModelError::EmptyText {
            reference: reference(),
            index: 2,
        }
        .code(),
    );
    push(
        ModelError::UnsupportedImage {
            reference: reference(),
            index: Some(1),
        }
        .code(),
    );
    push(
        ModelError::EmptyImage {
            reference: reference(),
            index: 0,
        }
        .code(),
    );
    push(
        ModelError::ImageTooLarge {
            reference: reference(),
            index: 0,
            bytes: 9,
            max_bytes: 8,
        }
        .code(),
    );
    push(
        ModelError::VectorCountMismatch {
            reference: reference(),
            expected: 2,
            actual: 1,
        }
        .code(),
    );
    push(
        ModelError::DimensionMismatch {
            reference: reference(),
            vector_index: 0,
            expected: 4,
            actual: 3,
        }
        .code(),
    );
    push(
        ModelError::NonFiniteValue {
            reference: reference(),
            vector_index: 0,
            value_index: 3,
        }
        .code(),
    );
    push(
        ModelError::InvalidTruncatedIndex {
            reference: reference(),
            index: 5,
            input_count: 2,
        }
        .code(),
    );
    push(
        ModelError::Model2VecLoad {
            reference: reference(),
            repo: "org/repo".to_owned(),
            revision: "rev".to_owned(),
            detail: "boom".to_owned(),
        }
        .code(),
    );
    push(
        ModelError::Model2VecDownload {
            reference: reference(),
            repo: "org/repo".to_owned(),
            revision: "rev".to_owned(),
            detail: "boom".to_owned(),
        }
        .code(),
    );
    push(
        ModelError::Model2VecEmbed {
            reference: reference(),
            repo: "org/repo".to_owned(),
            detail: "boom".to_owned(),
        }
        .code(),
    );
    push(
        ModelError::Model2VecTokenize {
            reference: reference(),
            detail: "boom".to_owned(),
        }
        .code(),
    );
    push(
        ModelError::TokenOutOfRange {
            reference: reference(),
            id: 99,
            rows: 10,
        }
        .code(),
    );
    push(
        ModelError::WorkerFailed {
            reference: reference(),
        }
        .code(),
    );
    push(
        ModelError::DownloadFailed {
            context: "url=https://example.invalid/x".to_owned(),
        }
        .code(),
    );

    // Every Qwen text model × failure combination.
    for model in [QwenTextModel::V4, QwenTextModel::V37, QwenTextModel::Other] {
        for failure in [
            QwenTextFailure::RequestFailed,
            QwenTextFailure::InvalidJson,
            QwenTextFailure::ApiError,
            QwenTextFailure::MissingData,
            QwenTextFailure::InvalidIndex,
            QwenTextFailure::IndexOutOfRange,
            QwenTextFailure::InvalidVector,
            QwenTextFailure::MissingApiKey,
            QwenTextFailure::MissingEndpoint,
        ] {
            push(model.code(failure));
            push(
                ModelError::QwenText {
                    model,
                    failure,
                    message: "boom".to_owned(),
                    context: "model=x".to_owned(),
                }
                .code(),
            );
        }
    }
    // Every VL failure.
    for failure in [
        QwenVlFailure::RequestFailed,
        QwenVlFailure::InvalidJson,
        QwenVlFailure::ApiError,
        QwenVlFailure::MissingEmbeddings,
        QwenVlFailure::InvalidItem,
        QwenVlFailure::IndexOutOfRange,
        QwenVlFailure::InvalidVector,
        QwenVlFailure::UnsupportedImageFormat,
        QwenVlFailure::TooManyImages,
        QwenVlFailure::MissingApiKey,
        QwenVlFailure::MissingEndpoint,
    ] {
        push(qwen_vl_code(failure));
    }
    // Configuration-error constructors across all Qwen backends.
    for backend in [
        QwenBackend::Text(QwenTextModel::V4),
        QwenBackend::Text(QwenTextModel::V37),
        QwenBackend::Text(QwenTextModel::Other),
        QwenBackend::Vl,
    ] {
        push(ModelError::missing_api_key("r", backend).code());
        push(ModelError::missing_endpoint("r", backend).code());
    }

    // Every `AuthError` variant.
    push(
        AuthError::AuthorizationRequired {
            provider: "qwen".to_owned(),
            model: "text-embedding-v4".to_owned(),
            endpoint: "https://example.invalid/e".to_owned(),
            purpose: RemoteEmbeddingPurpose::Query,
            detail: None,
        }
        .code(),
    );
    push(
        AuthError::InvalidTarget {
            detail: "bad target".to_owned(),
        }
        .code(),
    );
    push(
        AuthError::StoreFailed {
            operation: "read".to_owned(),
            detail: "boom".to_owned(),
        }
        .code(),
    );

    // Every `codes::*` constructor.
    push(codes::config_invalid());
    push(codes::config_invalid_embedding_runtime());
    push(codes::manifest_invalid());
    push(codes::lock_busy());
    push(codes::daemon_lease_active());
    push(codes::service_read_session_closed());
    push(codes::extractor_code_invalid_chunk_size());
    push(codes::extractor_code_invalid_chunk_overlap());
    push(codes::extractor_markdown_invalid_chunk_size());
    push(codes::extractor_markdown_invalid_chunk_overlap());
    push(codes::extractor_text_invalid_chunk_size());
    push(codes::extractor_text_invalid_chunk_overlap());
    push(codes::extractor_empty_file_id());
    push(codes::extractor_empty_absolute_path());
    push(codes::extractor_empty_relative_path());
    push(codes::extractor_image_empty_data());

    // Service error helpers.
    push(*empty_query_error().code());
    push(*workspace_index_not_found("root").code());
    push(*workspace_index_disabled("root").code());

    out.sort();
    out.dedup();
    out
}

#[test]
fn wire_codes_match_golden_registry() {
    let expected: Vec<String> = include_str!("golden/error-codes.txt")
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_owned)
        .collect();
    assert_eq!(all_codes(), expected);
}
