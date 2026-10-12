//! Signing-key files: per-workspace keys, legacy-global verification
//! fallback, and race-safe first creation.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use super::super::error::AuthError;
use super::crypto::from_hex;
use super::{RemoteEmbeddingAuthorizationStore, SIGNING_KEY_ENV_VAR, SIGNING_KEY_FILE};
use crate::error::{EngineError, EngineResult};
use crate::utils::hash::to_hex;

/// Default signing-key path: `$ZVEC_GREP_AUTHORIZATION_KEY_FILE`, else
/// `~/.zvec-grep/authorization-signing.key` (real home, like TS `homedir`).
pub(super) fn default_signing_key_path() -> PathBuf {
    if let Some(path) = std::env::var_os(SIGNING_KEY_ENV_VAR)
        && !path.is_empty()
    {
        return PathBuf::from(path);
    }
    let home = std::env::var_os("HOME").map_or_else(|| PathBuf::from("."), PathBuf::from);
    home.join(".zvec-grep").join("authorization-signing.key")
}

impl RemoteEmbeddingAuthorizationStore {
    /// Reads one key file: hex body, `None` when missing, blank, or
    /// non-hex. Never panics on absent paths; only surfaces IO errors.
    pub(super) fn read_key_file(path: &Path) -> EngineResult<Option<Vec<u8>>> {
        match fs::read_to_string(path) {
            Ok(text) => {
                let trimmed = text.trim();
                Ok(if trimmed.is_empty() {
                    None
                } else {
                    from_hex(trimmed)
                })
            }
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
            Err(error) => Err(EngineError::from(AuthError::StoreFailed {
                operation: "read".to_owned(),
                detail: format!("path={} error={error}", path.display()),
            })),
        }
    }

    /// Signing-key file inside one workspace root.
    fn workspace_key_path(root: &str) -> PathBuf {
        Path::new(root).join(".zvec-grep").join(SIGNING_KEY_FILE)
    }

    /// Candidate key paths for `root`, workspace key first and the legacy
    /// global second (single-key mode uses the shared path only).
    fn candidate_key_paths(&self, root: &str) -> Vec<PathBuf> {
        if !self.per_workspace_keys {
            return vec![self.signing_key_path.clone()];
        }
        let workspace = Self::workspace_key_path(root);
        if workspace == self.signing_key_path {
            vec![workspace]
        } else {
            vec![workspace, self.signing_key_path.clone()]
        }
    }

    /// Usable keys for `root`: the workspace key when present, else the
    /// legacy global fallback. Empty when no key file exists, so callers
    /// fail closed.
    pub(super) fn candidate_keys(&self, root: &str) -> EngineResult<Vec<Vec<u8>>> {
        let mut keys = Vec::with_capacity(2);
        for path in self.candidate_key_paths(root) {
            if let Some(key) = Self::read_key_file(&path)? {
                keys.push(key);
            }
        }
        Ok(keys)
    }

    /// Key new grants for `root` are signed with: the workspace key in
    /// workspace-keys mode (created on demand, 0600), the shared key
    /// otherwise.
    pub(super) fn get_or_create_key_for_root(&self, root: &str) -> EngineResult<Vec<u8>> {
        if self.per_workspace_keys {
            Self::get_or_create_key_file(&Self::workspace_key_path(root))
        } else {
            self.get_or_create_signing_key()
        }
    }

    fn get_or_create_signing_key(&self) -> EngineResult<Vec<u8>> {
        Self::get_or_create_key_file(&self.signing_key_path)
    }

    /// Reads the key at `path`, creating a fresh 32-byte hex key when absent.
    ///
    /// The key is written to a unique sibling temp file created with
    /// mode 0600 on unix (via `OpenOptionsExt::mode`, so it is never
    /// world-readable, even briefly), fsynced, then published with
    /// `hard_link`, which fails with `AlreadyExists` instead of
    /// overwriting a concurrent creator's key. A failed or partial write
    /// therefore only ever leaves a temp file (removed on the error path),
    /// never a broken key at `path`; race losers read the winner's key.
    pub(super) fn get_or_create_key_file(path: &Path) -> EngineResult<Vec<u8>> {
        if let Some(key) = Self::read_key_file(path)? {
            return Ok(key);
        }
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent).map_err(|error| {
            EngineError::from(AuthError::StoreFailed {
                operation: "create_dir".to_owned(),
                detail: format!("path={} error={error}", parent.display()),
            })
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(parent, fs::Permissions::from_mode(0o700));
        }
        let key: [u8; 32] = rand::random();
        let body = to_hex(&key);
        let file_name = path.file_name().map_or_else(
            || SIGNING_KEY_FILE.to_owned(),
            |name| name.to_string_lossy().into_owned(),
        );
        let nonce: u128 = rand::random();
        let tmp_path = parent.join(format!(
            ".{file_name}.tmp-{}-{nonce:032x}",
            std::process::id()
        ));
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let write_error = |error: std::io::Error| {
            EngineError::from(AuthError::StoreFailed {
                operation: "write".to_owned(),
                detail: format!("path={} error={error}", path.display()),
            })
        };
        match options.open(&tmp_path) {
            Ok(mut file) => {
                use std::io::Write;
                if let Err(error) = file
                    .write_all(body.as_bytes())
                    .and_then(|()| file.sync_all())
                {
                    drop(file);
                    let _ = fs::remove_file(&tmp_path);
                    return Err(write_error(error));
                }
            }
            Err(error) => return Err(write_error(error)),
        }
        match fs::hard_link(&tmp_path, path) {
            Ok(()) => {
                let _ = fs::remove_file(&tmp_path);
                return Ok(key.to_vec());
            }
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                let _ = fs::remove_file(&tmp_path);
            }
            Err(error) => {
                let _ = fs::remove_file(&tmp_path);
                return Err(write_error(error));
            }
        }
        // Lost the create race: whoever won owns the key now.
        Self::read_key_file(path)?.ok_or_else(|| {
            EngineError::from(AuthError::StoreFailed {
                operation: "read".to_owned(),
                detail: format!(
                    "path={} error=key missing after create race",
                    path.display()
                ),
            })
        })
    }
}
