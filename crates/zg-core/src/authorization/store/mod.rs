//! HMAC-signed workspace grant store, mirroring
//! `src/authorization/store.ts`.
//!
//! Grants live in `<root>/.zvec-grep-rs/authorization.json` (camelCase wire,
//! 0700 dirs / 0600 files, atomic tmp-rename writes). Each workspace root
//! holds its own 32-byte hex signing key at
//! `<root>/.zvec-grep-rs/authorization-signing.key` (0600); the global
//! `~/.zvec-grep-rs/authorization-signing.key` (`$ZVEC_GREP_AUTHORIZATION_KEY_FILE`
//! override) is kept as a legacy fallback for verification only, so grants
//! minted before per-workspace keys still verify. New grants are always
//! signed with the workspace key, which (together with the fingerprints in
//! every grant) keeps a grant for root A from ever authorizing root B.
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
/// Per-workspace signing-key file name inside `<root>/.zvec-grep-rs/`
pub const SIGNING_KEY_FILE: &str = "authorization-signing.key";
/// Env var overriding the signing-key path.
pub const SIGNING_KEY_ENV_VAR: &str = "ZVEC_GREP_AUTHORIZATION_KEY_FILE";

/// HMAC-signed workspace grant store.
///
/// In workspace-keys mode ([`Self::new`] without the env override, or
/// [`Self::with_workspace_keys`]) each workspace root signs with its own key
/// at `<root>/.zvec-grep-rs/authorization-signing.key`, falling back to the
/// legacy global key for verification only. With an explicit key path
/// ([`Self::with_signing_key`], or the env override) that single key signs
/// and verifies every root.
#[derive(Debug, Clone)]
pub struct RemoteEmbeddingAuthorizationStore {
    signing_key_path: PathBuf,
    per_workspace_keys: bool,
}

impl Default for RemoteEmbeddingAuthorizationStore {
    fn default() -> Self {
        Self::new()
    }
}

impl RemoteEmbeddingAuthorizationStore {
    /// Opens the store with per-workspace signing keys and the default
    /// legacy-global fallback, unless `$ZVEC_GREP_AUTHORIZATION_KEY_FILE`
    /// overrides the key path (single-key mode, as before).
    #[must_use]
    pub fn new() -> Self {
        let overridden =
            std::env::var_os(SIGNING_KEY_ENV_VAR).is_some_and(|value| !value.is_empty());
        Self {
            signing_key_path: default_signing_key_path(),
            per_workspace_keys: !overridden,
        }
    }

    /// Opens the store with an explicit single signing-key path (tests).
    /// Every root signs and verifies with this key.
    #[must_use]
    pub fn with_signing_key(path: PathBuf) -> Self {
        Self {
            signing_key_path: path,
            per_workspace_keys: false,
        }
    }

    /// Opens the store with per-workspace signing keys and an explicit
    /// legacy-global fallback path used for verification only (tests,
    /// daemon wiring). New grants are always signed with the workspace key.
    #[must_use]
    pub fn with_workspace_keys(legacy_signing_key_path: PathBuf) -> Self {
        Self {
            signing_key_path: legacy_signing_key_path,
            per_workspace_keys: true,
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
