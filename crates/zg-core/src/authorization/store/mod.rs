//! HMAC-signed workspace grant store, mirroring
//! `src/authorization/store.ts`.
//!
//! Grants live in `<root>/.zvec-grep-rs/authorization.json` (camelCase wire,
//! 0700 dirs / 0600 files, atomic tmp-rename writes). The 32-byte hex signing
//! secret lives only outside every workspace, at
//! `~/.zvec-grep-rs/authorization-signing.key`
//! (`$ZVEC_GREP_AUTHORIZATION_KEY_FILE` override, 0600). Each root's signing
//! key is derived from that secret (HMAC over the canonical root), so a key
//! or grant shipped inside a cloned workspace can never verify, and (with
//! the fingerprints in every grant) a grant for root A never authorizes
//! root B. Verification also accepts the raw global secret so grants signed
//! before per-root derivation (including TS grants) still verify.
//! Every signature covers the stable (key-sorted) JSON of the unsigned
//! grant, exactly like TS, so grants written by either implementation verify
//! in the other.
//!
//! Divergence: TS is async (`node:fs/promises`); this store is sync and
//! reuses [`crate::utils::lock`] plus [`crate::utils::json_io`]. There is no
//! ambient logger in `zg-core`, so an unreadable-but-present grant file
//! reads as an empty document (no grant) rather than logging a warning.

mod crypto;
mod document;
mod grants;
mod keys;
mod revoke;

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests;

use std::path::{Path, PathBuf};

use super::error::AuthError;
use super::target::canonicalize_workspace_roots;
use super::types::RemoteEmbeddingTarget;
use crate::paths::STATE_DIR_NAME;
use keys::default_signing_key_path;

/// On-disk document version (wire contract).
pub const DOCUMENT_VERSION: u32 = 1;
/// Grant file name inside `<root>/.zvec-grep-rs/`.
pub const GRANT_FILE: &str = "authorization.json";
/// Signing-key file name at the global (outside-workspace) key path.
pub const SIGNING_KEY_FILE: &str = "authorization-signing.key";
/// Env var overriding the signing-key path.
pub const SIGNING_KEY_ENV_VAR: &str = "ZVEC_GREP_AUTHORIZATION_KEY_FILE";

/// HMAC-signed workspace grant store.
///
/// The signing secret lives only at `signing_key_path`, outside every
/// workspace; each root signs and verifies with a key derived from that
/// secret for its canonical root path.
#[derive(Debug, Clone)]
pub struct RemoteEmbeddingAuthorizationStore {
    signing_key_path: PathBuf,
}

impl Default for RemoteEmbeddingAuthorizationStore {
    fn default() -> Self {
        Self::new()
    }
}

impl RemoteEmbeddingAuthorizationStore {
    /// Opens the store with the global signing secret at the default
    /// path, or `$ZVEC_GREP_AUTHORIZATION_KEY_FILE` when set. The secret
    /// is never read from or written to a workspace directory.
    #[must_use]
    pub fn new() -> Self {
        Self {
            signing_key_path: default_signing_key_path(),
        }
    }

    /// Opens the store with an explicit global signing-key path (tests).
    /// Every root signs and verifies with a key derived from the secret
    /// at this path. Callers must keep the path outside workspace roots.
    #[must_use]
    pub fn with_signing_key(path: PathBuf) -> Self {
        Self {
            signing_key_path: path,
        }
    }

    /// Grant file for the target's first canonical workspace root.
    ///
    /// Canonicalizes like every other method, so the path always names a
    /// file in the same unified root set `grant` fans out to.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::InvalidTarget`] when the target has no workspace roots.
    pub fn grant_path(&self, target: &RemoteEmbeddingTarget) -> Result<PathBuf, AuthError> {
        let Some(root) = canonicalize_workspace_roots(&target.workspace_roots)
            .into_iter()
            .next()
        else {
            return Err(AuthError::InvalidTarget {
                detail: "Remote Embedding target has no workspace roots.".to_owned(),
            });
        };
        Ok(Path::new(&root).join(STATE_DIR_NAME).join(GRANT_FILE))
    }
}
