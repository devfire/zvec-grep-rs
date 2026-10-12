//! Grant-document IO: locked read-modify-write plus wire-shape validation.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use super::super::error::AuthError;
use super::super::types::{REMOTE_EMBEDDING_CAPABILITY, RemoteEmbeddingDocument};
use super::{DOCUMENT_VERSION, RemoteEmbeddingAuthorizationStore};
use crate::error::{EngineError, EngineResult};
use crate::utils::json_io::{SECURE_MODES, write_json_file};
use crate::utils::lock::{LockMode, LockOptions, acquire_read_write_lock};

impl RemoteEmbeddingAuthorizationStore {
    pub(super) fn read_document(path: &Path) -> EngineResult<RemoteEmbeddingDocument> {
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
    pub(super) fn with_document_write(
        path: &Path,
        update: impl FnOnce(&mut RemoteEmbeddingDocument) -> bool,
    ) -> EngineResult<bool> {
        let lock_path = path.parent().map_or_else(
            || PathBuf::from("authorization-store"),
            |parent| parent.join("authorization-store"),
        );
        let _guard = acquire_read_write_lock(
            &lock_path,
            LockMode::Write,
            &LockOptions::new("remote-embedding-authorization"),
        )?;
        let mut document = Self::read_document(path)?;
        if !update(&mut document) {
            return Ok(false);
        }
        write_json_file(path, &document, SECURE_MODES)?;
        Ok(true)
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
    if !roots.iter().all(serde_json::Value::is_string) {
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
