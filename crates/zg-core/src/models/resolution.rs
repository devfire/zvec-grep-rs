//! Embedding reference resolution, mirroring
//! `src/engine/models/resolution.ts`.
//!
//! Precedence: explicit CLI flag > already-configured workspace value >
//! `ZVEC_GREP_EMBEDDING` environment variable > global default > fallback.
//! An environment value naming an unknown model is a hard configuration
//! error, not a silent fallback.

use std::collections::HashMap;

use super::catalog::{ModelReference, get_embedding_model_catalog_entry};
use crate::error::{EngineError, EngineErrorCode, EngineResult};

/// Name of the environment variable selecting the embedding model.
pub const EMBEDDING_ENV_VAR: &str = "ZVEC_GREP_EMBEDDING";

/// Inputs to [`resolve_embedding_reference`]; every field is optional.
#[derive(Debug, Clone, Default)]
pub struct ResolveEmbeddingReferenceOptions {
    /// Explicit user selection (e.g. `--embedding` flag).
    pub explicit: Option<ModelReference>,
    /// Model already recorded for the workspace.
    pub existing: Option<ModelReference>,
    /// Global default from configuration.
    pub global_default: Option<ModelReference>,
    /// Override for the process environment. `None` reads the real
    /// environment; `Some(map)` looks `ZVEC_GREP_EMBEDDING` up in `map`
    /// (used by tests and embedders that manage their own env).
    pub environment: Option<HashMap<String, String>>,
    /// Last-resort default when nothing else selects a model.
    pub fallback: Option<ModelReference>,
}

/// Resolves which embedding model to use. Returns `Ok(None)` only when no
/// source names a model; returns `Err` when the environment names a model
/// outside the catalog.
pub fn resolve_embedding_reference(
    options: &ResolveEmbeddingReferenceOptions,
) -> EngineResult<Option<ModelReference>> {
    if let Some(reference) = &options.explicit {
        return Ok(Some(reference.clone()));
    }
    if let Some(reference) = &options.existing {
        return Ok(Some(reference.clone()));
    }
    let environment_reference = match &options.environment {
        Some(map) => map.get(EMBEDDING_ENV_VAR).cloned(),
        None => std::env::var(EMBEDDING_ENV_VAR).ok(),
    };
    if let Some(reference) = non_empty_environment_value(environment_reference) {
        if get_embedding_model_catalog_entry(&reference).is_none() {
            return Err(EngineError::new(
                EngineErrorCode::from_static("CONFIG.EMBEDDING_ENVIRONMENT_INVALID"),
                "invalid embedding reference in environment",
            )
            .with_context(format!("source={EMBEDDING_ENV_VAR} value={reference}")));
        }
        return Ok(Some(ModelReference::from(reference)));
    }
    if let Some(reference) = &options.global_default {
        return Ok(Some(reference.clone()));
    }
    Ok(options.fallback.clone())
}

fn non_empty_environment_value(value: Option<String>) -> Option<String> {
    let trimmed = value?.trim().to_owned();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts() -> ResolveEmbeddingReferenceOptions {
        ResolveEmbeddingReferenceOptions {
            environment: Some(HashMap::new()),
            ..Default::default()
        }
    }

    #[test]
    fn explicit_wins_over_everything() {
        let mut o = opts();
        o.explicit = Some(ModelReference::from("local/potion-retrieval-32m"));
        o.existing = Some(ModelReference::from("qwen/text-embedding-v4"));
        o.fallback = Some(ModelReference::from("qwen/text-embedding-v4"));
        assert_eq!(
            resolve_embedding_reference(&o).unwrap(),
            Some(ModelReference::from("local/potion-retrieval-32m"))
        );
    }

    #[test]
    fn blank_env_falls_through_to_fallback() {
        let mut o = opts();
        o.environment = Some(HashMap::from([(
            EMBEDDING_ENV_VAR.to_owned(),
            "   ".to_owned(),
        )]));
        o.fallback = Some(ModelReference::from("qwen/text-embedding-v4"));
        assert_eq!(
            resolve_embedding_reference(&o).unwrap(),
            Some(ModelReference::from("qwen/text-embedding-v4"))
        );
    }

    #[test]
    fn unknown_env_model_is_an_error() {
        let mut o = opts();
        o.environment = Some(HashMap::from([(
            EMBEDDING_ENV_VAR.to_owned(),
            "nope/unknown".to_owned(),
        )]));
        let err = resolve_embedding_reference(&o).unwrap_err();
        assert_eq!(
            err.code().to_string(),
            "ZVEC_GREP.ENGINE.CONFIG.EMBEDDING_ENVIRONMENT_INVALID"
        );
    }
}
