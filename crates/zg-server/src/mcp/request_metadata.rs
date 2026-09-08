//! Request metadata: the embedding-environment meta key and trace
//! context extraction for MCP handlers.
//!
//! Mirrors `../zvec-grep/src/mcp/request-metadata.ts`
//! (`EMBEDDING_ENVIRONMENT_META_KEY`,
//! `embeddingEnvironmentFromRequestMeta`). Trace-context propagation
//! rides on `zg-server::trace` (phase F); this module reads the raw meta
//! value so handlers stay transport-agnostic.

use std::collections::HashMap;

/// Request `_meta` key carrying the CLI embedding environment.
pub const EMBEDDING_ENVIRONMENT_META_KEY: &str = "io.zvec-grep/embedding-environment";

/// Reads the embedding environment from a request `_meta` object:
/// non-object, missing, non-string, and blank values yield `None`.
pub fn embedding_environment_from_meta(meta: Option<&serde_json::Value>) -> Option<String> {
    let value = meta?.as_object()?.get(EMBEDDING_ENVIRONMENT_META_KEY)?;
    let text = value.as_str()?.trim();
    if text.is_empty() {
        return None;
    }
    Some(text.to_owned())
}

/// Reads the embedding environment from a string-keyed meta map (the
/// shape rmcp exposes through request extensions).
pub fn embedding_environment_from_map(meta: &HashMap<String, String>) -> Option<String> {
    let text = meta.get(EMBEDDING_ENVIRONMENT_META_KEY)?.trim();
    if text.is_empty() {
        return None;
    }
    Some(text.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extracts_and_trims_the_environment() {
        let meta = json!({ EMBEDDING_ENVIRONMENT_META_KEY: "  staging  " });
        assert_eq!(
            embedding_environment_from_meta(Some(&meta)).as_deref(),
            Some("staging")
        );
    }

    #[test]
    fn rejects_blank_missing_and_non_string() {
        assert_eq!(embedding_environment_from_meta(None), None);
        assert_eq!(embedding_environment_from_meta(Some(&json!({}))), None);
        assert_eq!(
            embedding_environment_from_meta(Some(&json!({
                EMBEDDING_ENVIRONMENT_META_KEY: "  "
            }))),
            None
        );
        assert_eq!(
            embedding_environment_from_meta(Some(&json!({
                EMBEDDING_ENVIRONMENT_META_KEY: 7
            }))),
            None
        );
    }
}
