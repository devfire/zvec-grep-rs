//! HMAC-signed workspace grant store, mirroring
//! `src/authorization/store.ts`.
//!
//! Grants live in `<root>/.zvec-grep/authorization.json` (camelCase wire,
//! 0700 dirs / 0600 files, atomic tmp-rename writes). Each workspace root
//! holds its own 32-byte hex signing key at
//! `<root>/.zvec-grep/authorization-signing.key` (0600); the global
//! `~/.zvec-grep/authorization-signing.key` (`$ZVEC_GREP_AUTHORIZATION_KEY_FILE`
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

use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use super::error::AuthError;
use super::target::canonicalize_workspace_roots;
use super::types::{
    AuthorizationStatus, GrantStatus, REMOTE_EMBEDDING_CAPABILITY, RemoteEmbeddingDocument,
    RemoteEmbeddingGrant, RemoteEmbeddingScope, RemoteEmbeddingTarget,
};
use crate::error::{EngineError, EngineResult};
use crate::types::UnixMillis;
use crate::utils::hash::to_hex;
use crate::utils::json_io::{SECURE_MODES, write_json_file};
use crate::utils::lock::{LockMode, LockOptions, acquire_read_write_lock};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;

/// On-disk document version (wire contract).
pub const DOCUMENT_VERSION: u32 = 1;
/// Grant file name inside `<root>/.zvec-grep/`.
pub const GRANT_FILE: &str = "authorization.json";
/// Per-workspace signing-key file name inside `<root>/.zvec-grep/`
/// (0600; parent dirs 0700).
pub const SIGNING_KEY_FILE: &str = "authorization-signing.key";
/// Env var overriding the signing-key path.
pub const SIGNING_KEY_ENV_VAR: &str = "ZVEC_GREP_AUTHORIZATION_KEY_FILE";

type HmacSha256 = Hmac<Sha256>;

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
///
/// In workspace-keys mode ([`Self::new`] without the env override, or
/// [`Self::with_workspace_keys`]) each workspace root signs with its own key
/// at `<root>/.zvec-grep/authorization-signing.key`, falling back to the
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
        Ok(Path::new(&root).join(".zvec-grep").join(GRANT_FILE))
    }

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
            let path = Path::new(root).join(".zvec-grep").join(GRANT_FILE);
            let document = self.read_document(&path)?;
            let covered = document.grants.iter().any(|grant| {
                grant.target_fingerprint == target.target_fingerprint.as_str()
                    && grant.workspace_fingerprint == target.workspace_fingerprint.as_str()
                    && keys.iter().any(|key| self.verify_grant(grant, key))
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
    /// Each root file's copy is signed with that root's own workspace key
    /// (single-key mode signs every copy with the shared key), so a copied
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
            let path = Path::new(root).join(".zvec-grep").join(GRANT_FILE);
            let stored = grant.clone();
            self.with_document_write(&path, |document| {
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
            let path = Path::new(root).join(".zvec-grep").join(GRANT_FILE);
            let changed = self.with_document_write(&path, |document| {
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
        let path = Path::new(&boundary).join(".zvec-grep").join(GRANT_FILE);
        let keys = self.candidate_keys(&boundary)?;
        let mut revoked = 0usize;
        let mut valid: Vec<RemoteEmbeddingGrant> = Vec::new();
        self.with_document_write(&path, |document| {
            revoked = document.grants.len();
            if revoked == 0 {
                return false;
            }
            if !keys.is_empty() {
                valid = document
                    .grants
                    .iter()
                    .filter(|grant| keys.iter().any(|key| self.verify_grant(grant, key)))
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
                let sibling = Path::new(&canonical).join(".zvec-grep").join(GRANT_FILE);
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
                let before = document.grants.len();
                document
                    .grants
                    .retain(|grant| !fingerprints.contains(&grant.target_fingerprint));
                document.grants.len() != before
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
        // Same canonical root set every other method operates on, so the
        // reported file is the one `grant` wrote even for symlinked roots.
        let canonical = canonicalize_workspace_roots(&[root.to_owned()])
            .into_iter()
            .next()
            .unwrap_or_else(|| root.to_owned());
        let path = Path::new(&canonical).join(".zvec-grep").join(GRANT_FILE);
        let document = self.read_document(&path)?;
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
                valid: !keys.is_empty() && keys.iter().any(|key| self.verify_grant(grant, key)),
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

    /// Reads the document under the write lock, applies `update`, and writes
    /// back only when it reports a change; returns whether the file was
    /// written. Skipping no-op writes keeps revokes over missing roots from
    /// creating directories or files.
    fn with_document_write(
        &self,
        path: &Path,
        update: impl FnOnce(&mut RemoteEmbeddingDocument) -> bool,
    ) -> EngineResult<bool> {
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
        if !update(&mut document) {
            return Ok(false);
        }
        write_json_file(path, &document, SECURE_MODES)?;
        Ok(true)
    }
    fn verify_grant(&self, grant: &RemoteEmbeddingGrant, key: &[u8]) -> bool {
        let Ok(expected) = sign(&unsigned_grant(grant), key) else {
            return false;
        };
        let (Some(actual), Some(expected)) = (from_hex(&grant.signature), from_hex(&expected))
        else {
            return false;
        };
        // Length is not secret: early exit, then constant-time content
        // comparison. `subtle` barriers the accumulator so LLVM cannot
        // short-circuit it (cf. `mac.verify_slice` in
        // `zg-server/src/mcp/request_state.rs`).
        if actual.len() != expected.len() {
            return false;
        }
        actual.ct_eq(&expected).into()
    }

    /// Reads one key file: hex body, `None` when missing, blank, or
    /// non-hex. Never panics on absent paths; only surfaces IO errors.
    fn read_key_file(path: &Path) -> EngineResult<Option<Vec<u8>>> {
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
    fn candidate_keys(&self, root: &str) -> EngineResult<Vec<Vec<u8>>> {
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
    fn get_or_create_key_for_root(&self, root: &str) -> EngineResult<Vec<u8>> {
        if self.per_workspace_keys {
            Self::get_or_create_key_file(&Self::workspace_key_path(root))
        } else {
            self.get_or_create_signing_key()
        }
    }

    fn get_or_create_signing_key(&self) -> EngineResult<Vec<u8>> {
        Self::get_or_create_key_file(&self.signing_key_path)
    }

    /// Reads the key at `path`, creating a fresh 32-byte hex key (0700 dirs,
    /// 0600 file, create-new so concurrent creators race safely) when absent.
    fn get_or_create_key_file(path: &Path) -> EngineResult<Vec<u8>> {
        if let Some(key) = Self::read_key_file(path)? {
            return Ok(key);
        }
        if let Some(parent) = path.parent() {
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
            .open(path);
        match created {
            Ok(mut file) => {
                use std::io::Write;
                if file.write_all(body.as_bytes()).is_ok() {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
                    }
                    return Ok(key.to_vec());
                }
            }
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(EngineError::from(AuthError::StoreFailed {
                    operation: "write".to_owned(),
                    detail: format!("path={} error={error}", path.display()),
                }));
            }
        }
        // Lost the create race (or the first write failed): whoever won owns
        // the key now.
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
    use crate::authorization::target::{
        canonicalize_workspace_roots, create_remote_embedding_target,
    };

    /// Canonical on-disk root: grant and key files live under the
    /// canonicalized path, so file assertions must join from there rather
    /// than the raw (possibly symlinked) input.
    fn canonical_path(root: &Path) -> PathBuf {
        let roots = canonicalize_workspace_roots(&[root.to_string_lossy().into_owned()]);
        PathBuf::from(roots.into_iter().next().expect("canonical root"))
    }

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

        // Re-grant, then revoke-all from the sibling root clears only that
        // file: the disjoint sibling copy survives on disk but no longer
        // authorizes, because every canonical root must hold a valid grant.
        store.grant(&target).expect("re-grant");
        assert_eq!(
            store
                .revoke_all(&root_b.to_string_lossy())
                .expect("revoke_all"),
            1
        );
        let status_a = store.status(&root_a.to_string_lossy()).expect("status a");
        assert_eq!(status_a.grants.len(), 1);
        assert!(!store.has_grant(&target).expect("has_grant after revoke"));
        // Revoking the target clears the stale sibling copy; afterwards
        // there is nothing left to revoke.
        assert!(store.revoke(&target).expect("revoke stale"));
        assert!(!store.has_grant(&target).expect("has_grant after revoke"));
        assert!(!store.revoke(&target).expect("revoke missing"));
    }
    #[test]
    fn truncated_signature_is_invalid() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("repo");
        fs::create_dir_all(&root).expect("mkdir");
        let store = store_in(dir.path());
        let target = target_for(&root);
        let grant = store.grant(&target).expect("grant");
        let keys = store
            .candidate_keys(&root.to_string_lossy())
            .expect("read keys");
        assert_eq!(keys.len(), 1);
        let key = keys.into_iter().next().expect("key present");
        assert!(store.verify_grant(&grant, &key));
        // Length mismatch fails before content comparison.
        let mut short = grant.clone();
        short.signature.truncate(short.signature.len() / 2);
        assert!(!store.verify_grant(&short, &key));
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

    #[test]
    fn workspace_keys_reject_cross_workspace_replay() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root_a = dir.path().join("a");
        let root_b = dir.path().join("b");
        fs::create_dir_all(&root_a).expect("mkdir a");
        fs::create_dir_all(&root_b).expect("mkdir b");
        // Legacy fallback points nowhere, so only workspace keys verify.
        let store =
            RemoteEmbeddingAuthorizationStore::with_workspace_keys(dir.path().join("legacy.key"));
        let target_a = target_for(&root_a);
        store.grant(&target_a).expect("grant a");
        assert!(store.has_grant(&target_a).expect("has_grant a"));
        // Each workspace mints its own key file.
        let canonical_a = canonical_path(&root_a);
        let canonical_b = canonical_path(&root_b);
        let key_a = canonical_a.join(".zvec-grep").join(SIGNING_KEY_FILE);
        assert!(key_a.is_file());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&key_a)
                .expect("key metadata")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        // Replaying A's grant file into B verifies under neither B's
        // workspace key nor the (absent) legacy key.
        let file_a = canonical_a.join(".zvec-grep").join(GRANT_FILE);
        let file_b = canonical_b.join(".zvec-grep").join(GRANT_FILE);
        if let Some(parent) = file_b.parent() {
            fs::create_dir_all(parent).expect("mkdir b authz");
        }
        fs::copy(&file_a, &file_b).expect("replay grant file");
        let status_b = store.status(&root_b.to_string_lossy()).expect("status b");
        assert_eq!(status_b.grants.len(), 1);
        assert!(!status_b.grants[0].valid);
        assert!(!store.has_grant(&target_for(&root_b)).expect("has_grant b"));
    }

    #[test]
    fn revoke_all_confines_sibling_fanout_to_boundary() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root_a = dir.path().join("a");
        let nested = root_a.join("sub");
        let outside = dir.path().join("outside");
        fs::create_dir_all(&nested).expect("mkdir nested");
        fs::create_dir_all(&outside).expect("mkdir outside");
        let store = store_in(dir.path());
        // One target spanning the boundary root, a nested root, and a
        // disjoint root: grant fans out to all three files.
        let target = create_remote_embedding_target(
            &[
                root_a.to_string_lossy().into_owned(),
                nested.to_string_lossy().into_owned(),
                outside.to_string_lossy().into_owned(),
            ],
            "qwen",
            "text-embedding-v4",
            "https://example.invalid/embeddings",
        )
        .expect("target");
        store.grant(&target).expect("grant");
        assert_eq!(
            store
                .revoke_all(&root_a.to_string_lossy())
                .expect("revoke_all"),
            1
        );
        // Nested sibling inside the boundary is cleaned; the disjoint
        // sibling outside it is skipped and keeps its grant.
        let nested_status = store
            .status(&nested.to_string_lossy())
            .expect("nested status");
        assert!(nested_status.grants.is_empty());
        let outside_status = store
            .status(&outside.to_string_lossy())
            .expect("outside status");
        assert_eq!(outside_status.grants.len(), 1);
        // Clearing the boundary root still deauthorizes the whole target.
        assert!(!store.has_grant(&target).expect("has_grant after revoke"));
    }

    #[test]
    fn revoke_all_on_missing_root_returns_zero_without_creating_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = store_in(dir.path());
        let missing = dir.path().join("does-not-exist");
        assert_eq!(
            store
                .revoke_all(&missing.to_string_lossy())
                .expect("revoke_all missing"),
            0
        );
        assert!(!missing.exists());
        assert!(!store.revoke(&target_for(&missing)).expect("revoke missing"));
        assert!(!missing.exists());
    }
}
