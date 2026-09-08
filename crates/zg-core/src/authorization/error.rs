//! Typed authorization errors: every `AUTH.*` wire code as an enum variant.
//!
//! Mirrors `src/authorization/operation.ts` (`authorizationRequiredError`)
//! and the target/store throw sites. The wire strings are unchanged from
//! TypeScript where TS defines one (`AUTH.REMOTE_EMBEDDING_REQUIRED`); the
//! target/store sites throw plain `Error`s in TS, so their Rust codes are
//! additions recorded in `docs/ts-divergence.md`.

use crate::error::{EngineError, EngineErrorCode};

/// Purpose of a remote embedding request (mirrors the TS `purpose` field).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RemoteEmbeddingPurpose {
    Query,
    Document,
}

impl RemoteEmbeddingPurpose {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Query => "query",
            Self::Document => "document",
        }
    }
}

/// Typed authorization error: every `AUTH.*` wire code as an enum variant.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AuthError {
    /// Remote embedding attempted without a valid operation permit.
    #[error("Remote Embedding authorization is required")]
    AuthorizationRequired {
        provider: String,
        model: String,
        endpoint: String,
        purpose: RemoteEmbeddingPurpose,
        detail: Option<String>,
    },
    /// Target construction or planning failed (empty roots, blank endpoint,
    /// model mismatch). TS throws plain `Error`s here; the code is new.
    #[error("Remote Embedding authorization target is invalid")]
    InvalidTarget { detail: String },
    /// Grant-file or signing-key IO failed. TS throws raw fs errors here;
    /// the code is new.
    #[error("Remote Embedding authorization store failed")]
    StoreFailed { operation: String, detail: String },
}

impl AuthError {
    /// Fully-qualified wire code for the variant (exhaustive `const` mapping).
    pub const fn code(&self) -> EngineErrorCode {
        match self {
            Self::AuthorizationRequired { .. } => {
                EngineErrorCode::from_static("AUTH.REMOTE_EMBEDDING_REQUIRED")
            }
            Self::InvalidTarget { .. } => EngineErrorCode::from_static("AUTH.INVALID_TARGET"),
            Self::StoreFailed { .. } => EngineErrorCode::from_static("AUTH.STORE_FAILED"),
        }
    }
}

impl From<AuthError> for EngineError {
    fn from(error: AuthError) -> Self {
        let code = error.code();
        let message = error.to_string();
        let context: Option<String> = match error {
            AuthError::AuthorizationRequired {
                provider,
                model,
                endpoint,
                purpose,
                detail,
            } => {
                let mut context = format!(
                    "provider={provider} model={model} endpoint={endpoint} purpose={}",
                    purpose.as_str()
                );
                if let Some(detail) = detail {
                    context.push_str(&format!(" detail={detail}"));
                }
                Some(context)
            }
            AuthError::InvalidTarget { detail } => Some(detail),
            AuthError::StoreFailed { operation, detail } => {
                Some(format!("operation={operation} detail={detail}"))
            }
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

    #[test]
    fn required_renders_ts_exact_code_and_context() {
        let engine = EngineError::from(AuthError::AuthorizationRequired {
            provider: "qwen".to_owned(),
            model: "text-embedding-v4".to_owned(),
            endpoint: "https://example.invalid/e".to_owned(),
            purpose: RemoteEmbeddingPurpose::Query,
            detail: None,
        });
        assert_eq!(
            engine.code().to_string(),
            "ZVEC_GREP.ENGINE.AUTH.REMOTE_EMBEDDING_REQUIRED"
        );
        assert_eq!(
            engine.message(),
            "Remote Embedding authorization is required"
        );
        assert_eq!(
            engine.context(),
            Some(
                "provider=qwen model=text-embedding-v4 endpoint=https://example.invalid/e purpose=query"
            )
        );
    }

    #[test]
    fn required_appends_detail() {
        let engine = EngineError::from(AuthError::AuthorizationRequired {
            provider: "qwen".to_owned(),
            model: "m".to_owned(),
            endpoint: "e".to_owned(),
            purpose: RemoteEmbeddingPurpose::Document,
            detail: Some("Workspace grant is missing, invalid, or revoked.".to_owned()),
        });
        assert!(
            engine
                .context()
                .unwrap_or_default()
                .contains("detail=Workspace grant is missing, invalid, or revoked.")
        );
    }

    #[test]
    fn invalid_target_has_frozen_code() {
        let engine = EngineError::from(AuthError::InvalidTarget {
            detail: "Remote Embedding authorization requires a workspace root.".to_owned(),
        });
        assert_eq!(
            engine.code().to_string(),
            "ZVEC_GREP.ENGINE.AUTH.INVALID_TARGET"
        );
    }
}
