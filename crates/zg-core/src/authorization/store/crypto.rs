//! HMAC signing and verification over stable (key-sorted) grant JSON.

use hmac::{Hmac, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;

use super::super::error::AuthError;
use super::super::types::RemoteEmbeddingGrant;
use super::RemoteEmbeddingAuthorizationStore;
use crate::utils::hash::to_hex;

type HmacSha256 = Hmac<Sha256>;

pub(super) fn from_hex(hex: &str) -> Option<Vec<u8>> {
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
pub(super) fn stable_stringify(value: &serde_json::Value) -> String {
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
pub(super) fn sign(unsigned: &serde_json::Value, key: &[u8]) -> Result<String, AuthError> {
    let mut mac = HmacSha256::new_from_slice(key).map_err(|_| AuthError::InvalidTarget {
        detail: "authorization signing key was rejected".to_owned(),
    })?;
    mac.update(stable_stringify(unsigned).as_bytes());
    Ok(to_hex(&mac.finalize().into_bytes()))
}

/// Per-root signing key derived from the global secret, which lives
/// outside every workspace (`~/.zvec-grep-rs/` or
/// `$ZVEC_GREP_AUTHORIZATION_KEY_FILE`). HMAC-SHA256 keyed by the global
/// secret over a domain-separated canonical root, so a key/grant pair
/// shipped inside a cloned workspace can never verify, while a grant for
/// root A still cannot verify for root B.
pub(super) fn derive_root_key(global_key: &[u8], root: &str) -> Result<Vec<u8>, AuthError> {
    let mut mac = HmacSha256::new_from_slice(global_key).map_err(|_| AuthError::InvalidTarget {
        detail: "authorization signing key was rejected".to_owned(),
    })?;
    mac.update(b"zvec-grep-rs:authorization-signing-key:v1\0");
    mac.update(root.as_bytes());
    Ok(mac.finalize().into_bytes().to_vec())
}

/// Unsigned grant object in TS field shape (signature excluded).
pub(super) fn unsigned_grant(grant: &RemoteEmbeddingGrant) -> serde_json::Value {
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

impl RemoteEmbeddingAuthorizationStore {
    pub(super) fn verify_grant(grant: &RemoteEmbeddingGrant, key: &[u8]) -> bool {
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
}
