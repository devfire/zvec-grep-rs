//! Grant issuance, lookup, and status reporting.

use std::path::Path;

use super::super::error::AuthError;
use super::super::target::canonicalize_workspace_roots;
use super::super::types::{
    AuthorizationStatus, GrantStatus, REMOTE_EMBEDDING_CAPABILITY, RemoteEmbeddingGrant,
    RemoteEmbeddingScope, RemoteEmbeddingTarget,
};
use super::crypto::{sign, unsigned_grant};
use super::{DOCUMENT_VERSION, GRANT_FILE, RemoteEmbeddingAuthorizationStore};
use crate::error::{EngineError, EngineResult};
use crate::paths::STATE_DIR_NAME;
use crate::types::UnixMillis;

impl RemoteEmbeddingAuthorizationStore {
    /// True when every canonical root file holds a valid signed grant for
    /// the target's workspace and target fingerprints.
    ///
    /// All-roots (not first-only): a grant is issued to every root file, so
    /// clearing any single file deauthorizes the target, and a grant for
    /// root A can never cover root B.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::StoreFailed`] when a signing key or grant file cannot be read, or [`AuthError::InvalidTarget`] when the target has no workspace roots.
    pub fn has_grant(&self, target: &RemoteEmbeddingTarget) -> EngineResult<bool> {
        let roots = canonicalize_workspace_roots(&target.workspace_roots);
        if roots.is_empty() {
            return Err(EngineError::from(AuthError::InvalidTarget {
                detail: "Remote Embedding target has no workspace roots.".to_owned(),
            }));
        }
        for root in &roots {
            // Fail closed without a usable key: no key means no grant can
            // verify.
            let keys = self.candidate_keys(root)?;
            if keys.is_empty() {
                return Ok(false);
            }
            let path = Path::new(root).join(STATE_DIR_NAME).join(GRANT_FILE);
            let document = Self::read_document(&path)?;
            let covered = document.grants.iter().any(|grant| {
                grant.target_fingerprint == target.target_fingerprint.as_str()
                    && grant.workspace_fingerprint == target.workspace_fingerprint.as_str()
                    && keys.iter().any(|key| Self::verify_grant(grant, key))
            });
            if !covered {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Signs and persists a workspace grant for every canonical root of the
    /// target, replacing any grant with the same target fingerprint.
    ///
    /// Each root file's copy is signed with that root's derived key
    /// (derived from the global secret outside the workspace), so a copied
    /// grant file never verifies elsewhere. Returns the grant written to
    /// the last canonical root.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::StoreFailed`] when a signing key cannot be created or a grant file cannot be written, or [`AuthError::InvalidTarget`] when the target has no workspace roots or the signing key is rejected.
    pub fn grant(&self, target: &RemoteEmbeddingTarget) -> EngineResult<RemoteEmbeddingGrant> {
        let roots = canonicalize_workspace_roots(&target.workspace_roots);
        if roots.is_empty() {
            return Err(EngineError::from(AuthError::InvalidTarget {
                detail: "Remote Embedding target has no workspace roots.".to_owned(),
            }));
        }
        let mut issued: Option<RemoteEmbeddingGrant> = None;
        for root in &roots {
            let key = self.get_or_create_key_for_root(root)?;
            let unsigned = RemoteEmbeddingGrant {
                version: DOCUMENT_VERSION,
                id: uuid::Uuid::new_v4().to_string(),
                capability: REMOTE_EMBEDDING_CAPABILITY.to_owned(),
                scope: RemoteEmbeddingScope::Workspace,
                workspace_roots: roots.clone(),
                workspace_fingerprint: target.workspace_fingerprint.to_string(),
                provider: target.provider.clone(),
                model: target.model.clone(),
                endpoint: target.endpoint.clone(),
                target_fingerprint: target.target_fingerprint.to_string(),
                granted_at: UnixMillis::now().as_millis(),
                signature: String::new(),
            };
            let signature = sign(&unsigned_grant(&unsigned), &key).map_err(EngineError::from)?;
            let grant = RemoteEmbeddingGrant {
                signature,
                ..unsigned
            };
            let path = Path::new(root).join(STATE_DIR_NAME).join(GRANT_FILE);
            let stored = grant.clone();
            Self::with_document_write(&path, |document| {
                document
                    .grants
                    .retain(|candidate| candidate.target_fingerprint != stored.target_fingerprint);
                document.grants.push(stored.clone());
                true
            })?;
            issued = Some(grant);
        }
        let Some(grant) = issued else {
            return Err(EngineError::from(AuthError::InvalidTarget {
                detail: "Remote Embedding target has no workspace roots.".to_owned(),
            }));
        };
        Ok(grant)
    }

    /// Lists the root's grants with per-grant validity.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::StoreFailed`] when the grant file or signing key cannot be read.
    pub fn status(&self, root: &str) -> EngineResult<AuthorizationStatus> {
        // Same canonical root set every other method operates on, so the
        // reported file is the one `grant` wrote even for symlinked roots.
        let canonical = canonicalize_workspace_roots(&[root.to_owned()])
            .into_iter()
            .next()
            .unwrap_or_else(|| root.to_owned());
        let path = Path::new(&canonical).join(STATE_DIR_NAME).join(GRANT_FILE);
        let document = Self::read_document(&path)?;
        let keys = self.candidate_keys(&canonical)?;
        let grants = document
            .grants
            .iter()
            .map(|grant| GrantStatus {
                version: grant.version,
                id: grant.id.clone(),
                capability: grant.capability.clone(),
                scope: grant.scope,
                workspace_roots: grant.workspace_roots.clone(),
                workspace_fingerprint: grant.workspace_fingerprint.clone(),
                provider: grant.provider.clone(),
                model: grant.model.clone(),
                endpoint: grant.endpoint.clone(),
                target_fingerprint: grant.target_fingerprint.clone(),
                granted_at: grant.granted_at,
                valid: !keys.is_empty() && keys.iter().any(|key| Self::verify_grant(grant, key)),
            })
            .collect();
        Ok(AuthorizationStatus { path, grants })
    }
}
