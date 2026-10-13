//! Grant revocation, including boundary-confined sibling fan-out.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[allow(unused_imports)]
use super::super::error::AuthError;
use super::super::target::canonicalize_workspace_roots;
use super::super::types::{RemoteEmbeddingGrant, RemoteEmbeddingTarget};
use super::{GRANT_FILE, RemoteEmbeddingAuthorizationStore};
use crate::error::EngineResult;
use crate::paths::STATE_DIR_NAME;

impl RemoteEmbeddingAuthorizationStore {
    /// Removes the target's grant from every canonical root file; true when
    /// any was removed.
    ///
    /// Writes stay inside the target's own canonical root set, and files
    /// that need no change are left untouched (missing roots resolve
    /// lexically and simply find no document, so nothing is created and
    /// nothing panics).
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::StoreFailed`] when a grant file cannot be read or written, or `LOCK.BUSY` when another writer holds the lock.
    pub fn revoke(&self, target: &RemoteEmbeddingTarget) -> EngineResult<bool> {
        let roots = canonicalize_workspace_roots(&target.workspace_roots);
        let mut revoked = false;
        for root in &roots {
            // Skip non-existent roots before locking: no grant file can
            // exist without its root dir, and the locked path would
            // otherwise create lock directories for typo'd paths. A root
            // created concurrently just reports `false` here; authorization
            // itself still fails closed on the next live check.
            if !Path::new(root).is_dir() {
                continue;
            }
            let path = Path::new(root).join(STATE_DIR_NAME).join(GRANT_FILE);
            let changed = Self::with_document_write(&path, |document| {
                let before = document.grants.len();
                document
                    .grants
                    .retain(|grant| grant.target_fingerprint != target.target_fingerprint.as_str());
                document.grants.len() != before
            })?;
            revoked = revoked || changed;
        }
        Ok(revoked)
    }

    /// Clears the requested root's grant file plus the same grants from
    /// sibling root files inside the requested workspace boundary; returns
    /// the cleared file's grant count.
    ///
    /// Sibling roots come from grant data, so each one is canonicalized and
    /// confined to the requested boundary (the root itself or a path nested
    /// under it): anything outside is skipped, and crafted grant data can
    /// never pull writes elsewhere. Missing paths resolve lexically and
    /// change nothing, so non-existent roots return `0` without creating
    /// files or panicking.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::StoreFailed`] when a signing key or a grant file cannot be read or written, or `LOCK.BUSY` when another writer holds the lock.
    pub fn revoke_all(&self, root: &str) -> EngineResult<usize> {
        let Some(boundary) = canonicalize_workspace_roots(&[root.to_owned()])
            .into_iter()
            .next()
        else {
            return Ok(0);
        };
        // Missing roots hold no grants: return before locking so no lock
        // directories are created for non-existent paths.
        if !Path::new(&boundary).is_dir() {
            return Ok(0);
        }
        let path = Path::new(&boundary).join(STATE_DIR_NAME).join(GRANT_FILE);
        let keys = self.candidate_keys(&boundary)?;
        let mut revoked = 0usize;
        let mut valid: Vec<RemoteEmbeddingGrant> = Vec::new();
        Self::with_document_write(&path, |document| {
            revoked = document.grants.len();
            if revoked == 0 {
                return false;
            }
            if !keys.is_empty() {
                valid = document
                    .grants
                    .iter()
                    .filter(|grant| keys.iter().any(|key| Self::verify_grant(grant, key)))
                    .cloned()
                    .collect();
            }
            document.grants.clear();
            true
        })?;
        let mut siblings: BTreeMap<PathBuf, Vec<String>> = BTreeMap::new();
        for grant in &valid {
            for workspace_root in &grant.workspace_roots {
                let Some(canonical) =
                    canonicalize_workspace_roots(std::slice::from_ref(workspace_root))
                        .into_iter()
                        .next()
                else {
                    continue;
                };
                if canonical != boundary && !within_workspace_boundary(&canonical, &boundary) {
                    continue;
                }
                // Skip non-existent siblings before locking: no file can
                // exist without its root dir, and locking would otherwise
                // create lock directories outside the requested workspace.
                if !Path::new(&canonical).is_dir() {
                    continue;
                }
                let sibling = Path::new(&canonical).join(STATE_DIR_NAME).join(GRANT_FILE);
                if sibling == path {
                    continue;
                }
                siblings
                    .entry(sibling)
                    .or_default()
                    .push(grant.target_fingerprint.clone());
            }
        }
        for (sibling, fingerprints) in &siblings {
            Self::with_document_write(sibling, |document| {
                let before = document.grants.len();
                document
                    .grants
                    .retain(|grant| !fingerprints.contains(&grant.target_fingerprint));
                document.grants.len() != before
            })?;
        }
        Ok(revoked)
    }
}

/// True when the already-canonical `candidate` root is the `boundary` root
/// itself or nested beneath it (`/repo` contains `/repo/sub`, never
/// `/repo-other`). Both inputs must be canonicalized first so `..`,
/// symlinks-as-text, and separator tricks cannot smuggle a path inside.
fn within_workspace_boundary(candidate: &str, boundary: &str) -> bool {
    if candidate == boundary {
        return true;
    }
    if boundary == "/" {
        return candidate.starts_with('/');
    }
    candidate
        .strip_prefix(boundary)
        .is_some_and(|rest| rest.starts_with('/'))
}
