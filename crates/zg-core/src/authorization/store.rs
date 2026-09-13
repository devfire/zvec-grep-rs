//! HMAC-signed workspace grant store, mirroring
//! `src/authorization/store.ts`.
//!
//! Grants live in `<root>/.zvec-grep/authorization.json` (camelCase wire,
//! 0700 dirs / 0600 files, atomic tmp-rename writes) and are authenticated
//! with a 32-byte hex signing key (`$ZVEC_GREP_AUTHORIZATION_KEY_FILE`, else
//! `~/.zvec-grep/authorization-signing.key`). The signature covers the
//! stable (key-sorted) JSON of the unsigned grant, exactly like TS, so
//! grants written by either implementation verify in the other.
//!
//! Divergence: TS is async (`node:fs/promises`); this store is sync and
//! reuses [`crate::utils::lock`] plus [`crate::utils::json_io`]. There is no
//! ambient logger in `zg-core`, so an unreadable-but-present grant file
//! reads as an empty document (no grant) rather than logging a warning.

use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use hmac::{Hmac, Mac};
use sha2::Sha256;

use super::error::AuthError;
use super::types::{
    AuthorizationStatus, GrantStatus, REMOTE_EMBEDDING_CAPABILITY, RemoteEmbeddingDocument,
    RemoteEmbeddingGrant, RemoteEmbeddingScope, RemoteEmbeddingTarget,
};
use crate::error::{EngineError, EngineResult};
use crate::types::UnixMillis;
use crate::utils::json_io::{SECURE_MODES, write_json_file};
use crate::utils::lock::{LockMode, LockOptions, acquire_read_write_lock};

/// On-disk document version (wire contract).
pub const DOCUMENT_VERSION: u32 = 1;
/// Grant file name inside `<root>/.zvec-grep/`.
pub const GRANT_FILE: &str = "authorization.json";
/// Env var overriding the signing-key path.
pub const SIGNING_KEY_ENV_VAR: &str = "ZVEC_GREP_AUTHORIZATION_KEY_FILE";

type HmacSha256 = Hmac<Sha256>;

/// Lowercase hex of bytes (grant signatures, key files).
fn to_hex(bytes: &[u8]) -> String {
    let mut hex = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        hex.push_str(&format!("{byte:02x}"));
    }
    hex
}

fn from_hex(hex: &str) -> Option<Vec<u8>> {
    if !hex.len().is_multiple_of(2) {
        return None;
    }
    let mut bytes = Vec::with_capacity(hex.len() / 2);
    let raw = hex.as_bytes();
    let mut cursor = 0;
    while cursor < raw.len() {
        let pair = std::str::from_utf8(raw.get(cursor..cursor + 2)?).ok()?;
        bytes.push(u8::from_str_radix(pair, 16).ok()?);
        cursor += 2;
    }
    Some(bytes)
}

/// Constant-time equality for signature comparison.
fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in left.iter().zip(right.iter()) {
        diff |= a ^ b;
    }
    diff == 0
}
/// Stable (key-sorted) JSON, mirroring TS `stableStringify`: arrays keep
/// order, objects sort keys recursively, scalars render as JSON.
fn stable_stringify(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => "null".to_owned(),
        serde_json::Value::Bool(flag) => flag.to_string(),
        serde_json::Value::Number(number) => number.to_string(),
        serde_json::Value::String(text) => {
            serde_json::to_string(text).unwrap_or_else(|_| "\"\"".to_owned())
        }
        serde_json::Value::Array(items) => {
            let body: Vec<String> = items.iter().map(stable_stringify).collect();
            format!("[{body}]", body = body.join(","))
        }
        serde_json::Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let body: Vec<String> = keys
                .iter()
                .map(|key| {
                    let rendered_key =
                        serde_json::to_string(key).unwrap_or_else(|_| "\"\"".to_owned());
                    // SAFETY of indexing: `key` comes from `map.keys()`.
                    format!(
                        "{rendered_key}:{value}",
                        value = stable_stringify(&map[*key])
                    )
                })
                .collect();
            format!("{{{body}}}", body = body.join(","))
        }
    }
}

/// HMAC-SHA256 hex over the stable JSON of `unsigned`.
fn sign(unsigned: &serde_json::Value, key: &[u8]) -> Result<String, AuthError> {
    let mut mac = HmacSha256::new_from_slice(key).map_err(|_| AuthError::InvalidTarget {
        detail: "authorization signing key was rejected".to_owned(),
    })?;
    mac.update(stable_stringify(unsigned).as_bytes());
    Ok(to_hex(&mac.finalize().into_bytes()))
}

/// Unsigned grant object in TS field shape (signature excluded).
fn unsigned_grant(grant: &RemoteEmbeddingGrant) -> serde_json::Value {
    serde_json::json!({
        "version": grant.version,
        "id": grant.id,
        "capability": grant.capability,
        "scope": grant.scope.as_str(),
        "workspaceRoots": grant.workspace_roots,
        "workspaceFingerprint": grant.workspace_fingerprint,
        "provider": grant.provider,
        "model": grant.model,
        "endpoint": grant.endpoint,
        "targetFingerprint": grant.target_fingerprint,
        "grantedAt": grant.granted_at,
    })
}

/// Default signing-key path: `$ZVEC_GREP_AUTHORIZATION_KEY_FILE`, else
/// `~/.zvec-grep/authorization-signing.key` (real home, like TS `homedir`).
fn default_signing_key_path() -> PathBuf {
    if let Some(path) = std::env::var_os(SIGNING_KEY_ENV_VAR)
        && !path.is_empty()
    {
        return PathBuf::from(path);
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".zvec-grep").join("authorization-signing.key")
}
/// HMAC-signed workspace grant store.
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
    /// Opens the store with the default signing-key path.
    #[must_use]
    pub fn new() -> Self {
        Self {
            signing_key_path: default_signing_key_path(),
        }
    }

    /// Opens the store with an explicit signing-key path (tests).
    #[must_use]
    pub fn with_signing_key(path: PathBuf) -> Self {
        Self {
            signing_key_path: path,
        }
    }

    /// Grant file for the target's first workspace root.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::InvalidTarget`] when the target has no workspace roots.
    pub fn grant_path(&self, target: &RemoteEmbeddingTarget) -> Result<PathBuf, AuthError> {
        let Some(root) = target.workspace_roots.first() else {
            return Err(AuthError::InvalidTarget {
                detail: "Remote Embedding target has no workspace roots.".to_owned(),
            });
        };
        Ok(Path::new(root).join(".zvec-grep").join(GRANT_FILE))
    }

    /// True when a valid signed grant covers the target fingerprint.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::StoreFailed`] when the signing key or grant file cannot be read, or [`AuthError::InvalidTarget`] when the target has no workspace roots.
    pub fn has_grant(&self, target: &RemoteEmbeddingTarget) -> EngineResult<bool> {
        let Some(key) = self.read_signing_key()? else {
            return Ok(false);
        };
        let path = self.grant_path(target).map_err(EngineError::from)?;
        let document = self.read_document(&path)?;
        Ok(document.grants.iter().any(|grant| {
            grant.target_fingerprint == target.target_fingerprint.as_str()
                && self.verify_grant(grant, &key)
        }))
    }

    /// Signs and persists a workspace grant for every root of the target,
    /// replacing any grant with the same target fingerprint.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::StoreFailed`] when the signing key cannot be created or a grant file cannot be written, or [`AuthError::InvalidTarget`] when the signing key is rejected.
    pub fn grant(&self, target: &RemoteEmbeddingTarget) -> EngineResult<RemoteEmbeddingGrant> {
        let key = self.get_or_create_signing_key()?;
        let unsigned = RemoteEmbeddingGrant {
            version: DOCUMENT_VERSION,
            id: uuid::Uuid::new_v4().to_string(),
            capability: REMOTE_EMBEDDING_CAPABILITY.to_owned(),
            scope: RemoteEmbeddingScope::Workspace,
            workspace_roots: target.workspace_roots.clone(),
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
        for root in &target.workspace_roots {
            let path = Path::new(root).join(".zvec-grep").join(GRANT_FILE);
            let grant = grant.clone();
            self.with_document_write(&path, |document| {
                document
                    .grants
                    .retain(|candidate| candidate.target_fingerprint != grant.target_fingerprint);
                document.grants.push(grant.clone());
            })?;
        }
        Ok(grant)
    }

    /// Removes the target's grant from every root file; true when any was
    /// removed.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::StoreFailed`] when a grant file cannot be read or written, or `LOCK.BUSY` when another writer holds the lock.
    pub fn revoke(&self, target: &RemoteEmbeddingTarget) -> EngineResult<bool> {
        let mut revoked = false;
        for root in &target.workspace_roots {
            let path = Path::new(root).join(".zvec-grep").join(GRANT_FILE);
            let fingerprint = target.target_fingerprint.as_str().to_owned();
            self.with_document_write(&path, |document| {
                let before = document.grants.len();
                document
                    .grants
                    .retain(|grant| grant.target_fingerprint != fingerprint);
                if document.grants.len() != before {
                    revoked = true;
                }
            })?;
        }
        Ok(revoked)
    }

    /// Clears the root's grant file plus the same grants from sibling root
    /// files; returns the cleared file's grant count.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::StoreFailed`] when the signing key or a grant file cannot be read or written, or `LOCK.BUSY` when another writer holds the lock.
    pub fn revoke_all(&self, root: &str) -> EngineResult<usize> {
        let path = Path::new(root).join(".zvec-grep").join(GRANT_FILE);
        let key = self.read_signing_key()?;
        let mut revoked = 0usize;
        let mut valid: Vec<RemoteEmbeddingGrant> = Vec::new();
        self.with_document_write(&path, |document| {
            revoked = document.grants.len();
            if revoked == 0 {
                return;
            }
            if let Some(key) = &key {
                valid = document
                    .grants
                    .iter()
                    .filter(|grant| self.verify_grant(grant, key))
                    .cloned()
                    .collect();
            }
            document.grants.clear();
        })?;
        let mut siblings: BTreeMap<PathBuf, Vec<String>> = BTreeMap::new();
        for grant in &valid {
            for workspace_root in &grant.workspace_roots {
                let sibling = Path::new(workspace_root)
                    .join(".zvec-grep")
                    .join(GRANT_FILE);
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
            self.with_document_write(sibling, |document| {
                document
                    .grants
                    .retain(|grant| !fingerprints.contains(&grant.target_fingerprint));
            })?;
        }
        Ok(revoked)
    }

    /// Lists the root's grants with per-grant validity.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::StoreFailed`] when the grant file or signing key cannot be read.
    pub fn status(&self, root: &str) -> EngineResult<AuthorizationStatus> {
        let path = Path::new(root).join(".zvec-grep").join(GRANT_FILE);
        let document = self.read_document(&path)?;
        let key = self.read_signing_key()?;
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
                valid: key
                    .as_ref()
                    .is_some_and(|key| self.verify_grant(grant, key)),
            })
            .collect();
        Ok(AuthorizationStatus { path, grants })
    }
    fn read_document(&self, path: &Path) -> EngineResult<RemoteEmbeddingDocument> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == ErrorKind::NotFound => {
                return Ok(RemoteEmbeddingDocument {
                    version: DOCUMENT_VERSION,
                    grants: Vec::new(),
                });
            }
            Err(error) => {
                return Err(EngineError::from(AuthError::StoreFailed {
                    operation: "read".to_owned(),
                    detail: format!("path={} error={error}", path.display()),
                }));
            }
        };
        let value: serde_json::Value = serde_json::from_str(&text)
            .unwrap_or_else(|_| serde_json::json!({ "version": DOCUMENT_VERSION, "grants": [] }));
        if !is_authorization_document(&value) {
            return Ok(RemoteEmbeddingDocument {
                version: DOCUMENT_VERSION,
                grants: Vec::new(),
            });
        }
        serde_json::from_value(value).map_err(|error| {
            EngineError::from(AuthError::StoreFailed {
                operation: "parse".to_owned(),
                detail: format!("path={} error={error}", path.display()),
            })
        })
    }

    fn with_document_write(
        &self,
        path: &Path,
        update: impl FnOnce(&mut RemoteEmbeddingDocument),
    ) -> EngineResult<()> {
        let lock_path = path
            .parent()
            .map(|parent| parent.join("authorization-store"))
            .unwrap_or_else(|| PathBuf::from("authorization-store"));
        let _guard = acquire_read_write_lock(
            &lock_path,
            LockMode::Write,
            &LockOptions::new("remote-embedding-authorization"),
        )?;
        let mut document = self.read_document(path)?;
        update(&mut document);
        write_json_file(path, &document, SECURE_MODES)
    }

    fn verify_grant(&self, grant: &RemoteEmbeddingGrant, key: &[u8]) -> bool {
        let Ok(expected) = sign(&unsigned_grant(grant), key) else {
            return false;
        };
        let (Some(actual), Some(expected)) = (from_hex(&grant.signature), from_hex(&expected))
        else {
            return false;
        };
        constant_time_eq(&actual, &expected)
    }

    fn read_signing_key(&self) -> EngineResult<Option<Vec<u8>>> {
        match fs::read_to_string(&self.signing_key_path) {
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
                detail: format!("path={} error={error}", self.signing_key_path.display()),
            })),
        }
    }

    fn get_or_create_signing_key(&self) -> EngineResult<Vec<u8>> {
        if let Some(key) = self.read_signing_key()? {
            return Ok(key);
        }
        if let Some(parent) = self.signing_key_path.parent() {
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
        }
        let key: [u8; 32] = rand::random();
        let body = to_hex(&key);
        let created = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&self.signing_key_path);
        match created {
            Ok(mut file) => {
                use std::io::Write;
                if file.write_all(body.as_bytes()).is_ok() {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        let _ = fs::set_permissions(
                            &self.signing_key_path,
                            fs::Permissions::from_mode(0o600),
                        );
                    }
                    return Ok(key.to_vec());
                }
            }
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(EngineError::from(AuthError::StoreFailed {
                    operation: "write".to_owned(),
                    detail: format!("path={} error={error}", self.signing_key_path.display()),
                }));
            }
        }
        // Lost the create race (or the first write failed): whoever won owns
        // the key now.
        self.read_signing_key()?.ok_or_else(|| {
            EngineError::from(AuthError::StoreFailed {
                operation: "read".to_owned(),
                detail: format!(
                    "path={} error=key missing after create race",
                    self.signing_key_path.display()
                ),
            })
        })
    }
}

/// Mirrors TS `isAuthorizationDocument`: version 1 plus all-valid grants.
fn is_authorization_document(value: &serde_json::Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    if object.get("version").and_then(serde_json::Value::as_u64)
        != Some(u64::from(DOCUMENT_VERSION))
    {
        return false;
    }
    let Some(grants) = object.get("grants").and_then(serde_json::Value::as_array) else {
        return false;
    };
    grants.iter().all(is_workspace_grant)
}

/// Mirrors TS `isWorkspaceGrant`.
fn is_workspace_grant(value: &serde_json::Value) -> bool {
    let Some(grant) = value.as_object() else {
        return false;
    };
    if grant.get("version").and_then(serde_json::Value::as_u64) != Some(u64::from(DOCUMENT_VERSION))
    {
        return false;
    }
    if grant.get("capability").and_then(serde_json::Value::as_str)
        != Some(REMOTE_EMBEDDING_CAPABILITY)
    {
        return false;
    }
    if grant.get("scope").and_then(serde_json::Value::as_str) != Some("workspace") {
        return false;
    }
    let Some(roots) = grant
        .get("workspaceRoots")
        .and_then(serde_json::Value::as_array)
    else {
        return false;
    };
    if !roots.iter().all(|root| root.is_string()) {
        return false;
    }
    for key in [
        "id",
        "workspaceFingerprint",
        "provider",
        "model",
        "endpoint",
        "targetFingerprint",
        "signature",
    ] {
        if !grant.get(key).is_some_and(serde_json::Value::is_string) {
            return false;
        }
    }
    grant
        .get("grantedAt")
        .is_some_and(serde_json::Value::is_number)
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::authorization::target::create_remote_embedding_target;

    fn store_in(dir: &Path) -> RemoteEmbeddingAuthorizationStore {
        RemoteEmbeddingAuthorizationStore::with_signing_key(dir.join("signing.key"))
    }

    fn target_for(root: &Path) -> RemoteEmbeddingTarget {
        create_remote_embedding_target(
            &[root.to_string_lossy().into_owned()],
            "qwen",
            "text-embedding-v4",
            "https://example.invalid/embeddings",
        )
        .expect("target")
    }

    #[test]
    fn stable_stringify_sorts_keys_like_ts() {
        let value = serde_json::json!({
            "version": 1,
            "scope": "workspace",
            "workspaceRoots": ["/b", "/a"],
            "id": "x",
        });
        assert_eq!(
            stable_stringify(&value),
            r#"{"id":"x","scope":"workspace","version":1,"workspaceRoots":["/b","/a"]}"#
        );
    }

    #[test]
    fn grants_are_signed_target_bound_and_revocable() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root_a = dir.path().join("a");
        let root_b = dir.path().join("b");
        fs::create_dir_all(&root_a).expect("mkdir a");
        fs::create_dir_all(&root_b).expect("mkdir b");
        let store = store_in(dir.path());
        let target = create_remote_embedding_target(
            &[
                root_a.to_string_lossy().into_owned(),
                root_b.to_string_lossy().into_owned(),
            ],
            "qwen",
            "text-embedding-v4",
            "https://example.invalid/embeddings",
        )
        .expect("target");

        store.grant(&target).expect("grant");
        assert!(store.has_grant(&target).expect("has_grant"));
        let status = store.status(&root_a.to_string_lossy()).expect("status");
        assert_eq!(status.grants.len(), 1);
        assert!(status.grants[0].valid);
        assert_eq!(status.grants[0].scope, RemoteEmbeddingScope::Workspace);

        // Tampering with a signed field invalidates the grant.
        let text = fs::read_to_string(&status.path).expect("read grants");
        let tampered = text.replace(
            "https://example.invalid/embeddings",
            "https://tampered.invalid/embeddings",
        );
        assert_ne!(text, tampered);
        fs::write(&status.path, tampered).expect("tamper");
        assert!(!store.has_grant(&target).expect("has_grant after tamper"));

        // Re-grant, then revoke-all from the sibling root clears both files.
        store.grant(&target).expect("re-grant");
        assert_eq!(
            store
                .revoke_all(&root_b.to_string_lossy())
                .expect("revoke_all"),
            1
        );
        assert!(!store.has_grant(&target).expect("has_grant after revoke"));
        assert!(!store.revoke(&target).expect("revoke missing"));
    }

    #[test]
    fn empty_store_has_no_grants() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("repo");
        fs::create_dir_all(&root).expect("mkdir");
        let store = store_in(dir.path());
        let target = target_for(&root);
        assert!(!store.has_grant(&target).expect("has_grant"));
        let status = store.status(&root.to_string_lossy()).expect("status");
        assert!(status.grants.is_empty());
        assert_eq!(
            store
                .revoke_all(&root.to_string_lossy())
                .expect("revoke_all"),
            0
        );
    }
}
