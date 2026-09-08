//! Failure → snapshot mapping: retryability and redacted error info.
//!
//! TS `isRetryable` / `safeErrorCode` live here, next to the enum they
//! classify. Every [`JobFailure`](super::failure::JobFailure) variant is
//! spelled out explicitly (no wildcard arm): adding a variant breaks this
//! `match` until retryability and redaction are decided for it.

use super::failure::JobFailure;
use super::snapshot::IndexJobError;
use crate::errors::DaemonError;

/// Retryable exactly like TS `isRetryable`: a retryable `DaemonError`, or
/// an engine error carrying `ZVEC_GREP.ENGINE.LOCK.BUSY`.
pub(crate) fn is_retryable(failure: &JobFailure) -> bool {
    match failure {
        JobFailure::Cancelled => false,
        JobFailure::Daemon(error) => error.retryable(),
        JobFailure::Engine(error) => error.code().to_string() == "ZVEC_GREP.ENGINE.LOCK.BUSY",
        JobFailure::Failed(_) => false,
    }
}

pub(crate) fn error_info(failure: &JobFailure) -> IndexJobError {
    match failure {
        JobFailure::Cancelled => IndexJobError {
            code: DaemonError::IndexCancelled.code().to_owned(),
            message: "indexing was cancelled".to_owned(),
            context: None,
            cause: None,
        },
        JobFailure::Daemon(error) => IndexJobError {
            code: safe_code(error.code()).unwrap_or("INDEX_FAILED").to_owned(),
            message: zg_core::error::redact_error_text(&error.to_string(), 512),
            context: None,
            cause: None,
        },
        JobFailure::Engine(error) => IndexJobError {
            code: safe_engine_code(&error.code().to_string()),
            message: zg_core::error::redact_error_text(error.message(), 512),
            context: error
                .context()
                .map(|context| zg_core::error::redact_error_text(context, 4096)),
            cause: None,
        },
        JobFailure::Failed(message) => IndexJobError {
            code: "INDEX_FAILED".to_owned(),
            message: zg_core::error::redact_error_text(message, 512),
            context: None,
            cause: None,
        },
    }
}

/// TS `safeErrorCode`: uppercase-led `A-Z0-9_.-`, at most 128 chars.
fn safe_code(code: &str) -> Option<&str> {
    let trimmed = code.trim();
    if trimmed.is_empty() || trimmed.len() > 128 {
        return None;
    }
    let mut chars = trimmed.bytes();
    if !chars.next().is_some_and(|byte| byte.is_ascii_uppercase()) {
        return None;
    }
    if chars.all(|byte| {
        byte.is_ascii_uppercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'.' | b'-')
    }) {
        Some(trimmed)
    } else {
        None
    }
}

fn safe_engine_code(code: &str) -> String {
    let redacted = zg_core::error::redact_error_text(code.trim(), 128);
    if redacted.chars().all(|char| {
        char.is_ascii_uppercase() || char.is_ascii_digit() || matches!(char, '_' | '.' | '-')
    }) && redacted.starts_with(|char: char| char.is_ascii_uppercase())
    {
        redacted
    } else {
        "INDEX_FAILED".to_owned()
    }
}
