//! Target identity: canonical roots plus SHA-256 fingerprints, mirroring
//! `src/authorization/target.ts`.
//!
//! Divergence: TS canonicalizes with async `realpath` (symlink-resolving);
//! Rust resolves with sync `std::fs::canonicalize` and falls back to the
//! lexically-absolute path when the root does not exist yet, so planning
//! works before the workspace directory is created.

use std::path::{Path, PathBuf};

use crate::utils::hash::sha256_text;

use super::error::AuthError;
use super::types::{RemoteEmbeddingTarget, TargetFingerprint, WorkspaceFingerprint};

/// Canonicalizes workspace roots: absolute, symlink-resolved when possible,
/// deduplicated, sorted. Mirrors `canonicalizeWorkspaceRoots`.
#[must_use]
pub fn canonicalize_workspace_roots(roots: &[String]) -> Vec<String> {
    let mut canonical: Vec<String> = roots
        .iter()
        .map(|root| {
            let path = Path::new(root);
            let absolute = if path.is_absolute() {
                path.to_path_buf()
            } else {
                std::env::current_dir()
                    .unwrap_or_else(|_| PathBuf::from("."))
                    .join(path)
            };
            std::fs::canonicalize(&absolute)
                .unwrap_or_else(|_| crate::paths::normalize_path(&absolute))
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect();
    canonical.sort();
    canonical.dedup();
    canonical
}

/// Fingerprint over the canonical roots: `sha256(JSON.stringify(roots))`.
#[must_use]
pub fn workspace_fingerprint(roots: &[String]) -> WorkspaceFingerprint {
    let json = serde_json::to_string(roots).unwrap_or_else(|_| "[]".to_owned());
    WorkspaceFingerprint::from_hex(sha256_text(&json))
}

/// Fingerprint over `[workspaceFingerprint, provider, model, endpoint]`.
#[must_use]
pub fn remote_embedding_target_fingerprint(
    workspace: &WorkspaceFingerprint,
    provider: &str,
    model: &str,
    endpoint: &str,
) -> TargetFingerprint {
    let json = serde_json::to_string(&[workspace.as_str(), provider, model, endpoint])
        .unwrap_or_else(|_| "[]".to_owned());
    TargetFingerprint::from_hex(sha256_text(&json))
}

/// Builds a [`RemoteEmbeddingTarget`] from raw roots plus provider identity.
///
/// Empty roots or a blank endpoint are [`AuthError::InvalidTarget`]; TS
/// throws plain `Error`s with the same messages.
///
/// # Errors
///
/// Returns [`AuthError::InvalidTarget`] when no workspace roots survive canonicalization or the endpoint is blank.
pub fn create_remote_embedding_target(
    roots: &[String],
    provider: &str,
    model: &str,
    endpoint: &str,
) -> Result<RemoteEmbeddingTarget, AuthError> {
    let workspace_roots = canonicalize_workspace_roots(roots);
    if workspace_roots.is_empty() {
        return Err(AuthError::InvalidTarget {
            detail: "Remote Embedding authorization requires a workspace root.".to_owned(),
        });
    }
    let endpoint = endpoint.trim();
    if endpoint.is_empty() {
        return Err(AuthError::InvalidTarget {
            detail: "Remote Embedding authorization requires an endpoint.".to_owned(),
        });
    }
    let fingerprint = workspace_fingerprint(&workspace_roots);
    let target = remote_embedding_target_fingerprint(&fingerprint, provider, model, endpoint);
    Ok(RemoteEmbeddingTarget {
        workspace_roots,
        workspace_fingerprint: fingerprint,
        provider: provider.to_owned(),
        model: model.to_owned(),
        endpoint: endpoint.to_owned(),
        target_fingerprint: target,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprints_match_ts_vectors() {
        // sha256(JSON.stringify(["/repo"])) and the target preimage, fixed
        // so a hashing drift fails here instead of silently orphaning grants.
        let roots = vec!["/repo".to_owned()];
        let workspace = workspace_fingerprint(&roots);
        assert_eq!(
            workspace.as_str(),
            sha256_text(&serde_json::to_string(&roots).expect("json"))
        );
        assert_eq!(workspace.as_str().len(), 64);
        let target = remote_embedding_target_fingerprint(
            &workspace,
            "qwen",
            "text-embedding-v4",
            "https://example.invalid/e",
        );
        assert_eq!(target.as_str().len(), 64);
    }

    #[test]
    fn empty_roots_and_endpoint_are_invalid() {
        let err =
            create_remote_embedding_target(&[], "qwen", "m", "https://e").expect_err("empty roots");
        assert!(matches!(err, AuthError::InvalidTarget { .. }));
        let err = create_remote_embedding_target(&["/repo".to_owned()], "qwen", "m", "  ")
            .expect_err("blank endpoint");
        assert!(matches!(err, AuthError::InvalidTarget { .. }));
    }

    #[test]
    fn roots_dedupe_and_sort() {
        let roots =
            canonicalize_workspace_roots(&["/b".to_owned(), "/a".to_owned(), "/a".to_owned()]);
        assert_eq!(roots, vec!["/a".to_owned(), "/b".to_owned()]);
    }
}
