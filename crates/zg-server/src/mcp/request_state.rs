//! HMAC-signed, replay-guarded request state for the remote-embedding
//! authorization round trip.
//!
//! Mirrors `../zvec-grep/src/mcp/request-state.ts`
//! (`RemoteEmbeddingRequestState`, the in-memory replay guard, the codec
//! binding state to method + principal, key helpers). The elicitation
//! caller lands with the interactive authorization flow; the codec and
//! guard are complete and unit-tested here (see `docs/ts-divergence.md`).
//! Field names stay TS-identical so minted tokens keep the same shape.

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hmac::{Hmac, Mac};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use zg_core::types::UnixMillis;

use crate::mcp::error::McpError;
use crate::sync::MutexExt;

/// MCP method a request-state token is bound to.
///
/// `#[non_exhaustive]` so binding a new method is a deliberate addition,
/// not a string typo widening the check: construction and verification
/// both go through [`BoundMethod::as_str`], never a literal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum BoundMethod {
    /// `tools/call` — the only bound method today.
    ToolsCall,
}

impl BoundMethod {
    /// Wire string for the bound method.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ToolsCall => "tools/call",
        }
    }
}

/// Request-state signing key length (32 bytes).
pub const REQUEST_STATE_KEY_BYTES: usize = 32;
/// Request-state lifetime (10 minutes).
pub const REQUEST_STATE_TTL: Duration = Duration::from_secs(10 * 60);
/// Cap on remembered nonces in the in-memory guard.
pub const MAX_IN_MEMORY_CONSUMED_STATES: usize = 4_096;

/// Authorization round-trip state, mirroring
/// `RemoteEmbeddingRequestState`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteEmbeddingRequestState {
    /// Schema version (always 1).
    pub version: u8,
    /// Anti-replay nonce (`{millis}-{uuid}`).
    pub nonce: String,
    /// Bound MCP method (`tools/call`).
    pub method: String,
    /// Tool the grant applies to.
    pub tool: String,
    /// Fingerprint of the tool arguments.
    pub arguments_fingerprint: String,
    /// Fingerprint of the authorization target.
    pub target_fingerprint: String,
    /// Fingerprint of the disclosure text.
    pub disclosure_fingerprint: String,
}

/// State fields the minter binds, without the nonce.
#[derive(Debug, Clone)]
pub struct RequestStateFields {
    /// Tool the grant applies to.
    pub tool: String,
    /// Fingerprint of the tool arguments.
    pub arguments_fingerprint: String,
    /// Fingerprint of the authorization target.
    pub target_fingerprint: String,
    /// Fingerprint of the disclosure text.
    pub disclosure_fingerprint: String,
}

impl RemoteEmbeddingRequestState {
    /// Matches expected fields ignoring the nonce (mirrors
    /// `matchesRemoteEmbeddingRequestState`).
    #[must_use]
    pub fn matches(&self, expected: &RequestStateFields) -> bool {
        self.version == 1
            && self.method == BoundMethod::ToolsCall.as_str()
            && self.tool == expected.tool
            && self.arguments_fingerprint == expected.arguments_fingerprint
            && self.target_fingerprint == expected.target_fingerprint
            && self.disclosure_fingerprint == expected.disclosure_fingerprint
    }
}

/// SHA-256 hex over canonical (sorted-key) JSON, mirroring TS
/// `fingerprint` (`stableJson` + sha256 hex).
#[must_use]
pub fn fingerprint(value: &serde_json::Value) -> String {
    let canonical = stable_json(value);
    let digest = Sha256::digest(canonical.as_bytes());
    hex_encode(&digest)
}

fn hex_encode(bytes: &[u8]) -> String {
    zg_core::utils::hash::to_hex(bytes)
}

fn stable_json(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Object(map) => {
            let sorted: BTreeMap<&String, &serde_json::Value> = map.iter().collect();
            let mut out = String::from("{");
            for (index, (key, value)) in sorted.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::to_string(key).unwrap_or_default());
                out.push(':');
                out.push_str(&stable_json(value));
            }
            out.push('}');
            out
        }
        serde_json::Value::Array(items) => {
            let mut out = String::from("[");
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(&stable_json(item));
            }
            out.push(']');
            out
        }
        serde_json::Value::Null
        | serde_json::Value::Bool(_)
        | serde_json::Value::Number(_)
        | serde_json::Value::String(_) => serde_json::to_string(value).unwrap_or_default(),
    }
}

/// In-memory replay guard: nonces are single-use, entries expire after
/// the state TTL, and the table caps at 4096 (mirrors
/// `InMemoryRemoteEmbeddingRequestStateReplayGuard`, including the
/// fail-closed-when-full rule).
#[derive(Debug, Default)]
pub struct InMemoryRequestStateReplayGuard {
    consumed: Mutex<HashMap<String, SystemTime>>,
}

impl InMemoryRequestStateReplayGuard {
    /// Creates an empty guard.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Issues state with a fresh nonce.
    pub fn issue(&self, fields: RequestStateFields) -> RemoteEmbeddingRequestState {
        let mut nonce_bytes = [0u8; 16];
        rand::rng().fill_bytes(&mut nonce_bytes);
        RemoteEmbeddingRequestState {
            version: 1,
            // Clock-unavailable direction: the `0` fallback is fail-safe —
            // nonce uniqueness comes from the 16 random bytes, not the clock.
            nonce: format!("{}-{}", UnixMillis::now_ms_or(0), hex_encode(&nonce_bytes)),
            method: BoundMethod::ToolsCall.as_str().to_owned(),
            tool: fields.tool,
            arguments_fingerprint: fields.arguments_fingerprint,
            target_fingerprint: fields.target_fingerprint,
            disclosure_fingerprint: fields.disclosure_fingerprint,
        }
    }

    /// Consumes a nonce: false when already seen or the table is full.
    pub fn consume(&self, state: &RemoteEmbeddingRequestState) -> bool {
        let now = SystemTime::now();
        let mut consumed = self.consumed.lock_ignore_poison();
        consumed.retain(|_, consumed_at| {
            // Fail closed: when the clock is unreadable every entry is kept,
            // so a replay still hits the table (or the fail-closed-when-full
            // rule) instead of slipping through an emptied guard.
            now.duration_since(*consumed_at)
                .map(|age| age < REQUEST_STATE_TTL)
                .unwrap_or(true)
        });
        if consumed.contains_key(&state.nonce) || consumed.len() >= MAX_IN_MEMORY_CONSUMED_STATES {
            return false;
        }
        consumed.insert(state.nonce.clone(), now);
        true
    }
}

/// HMAC codec binding minted state to the MCP method and the caller
/// principal (mirrors `createRemoteEmbeddingRequestStateCodec`).
#[derive(Debug, Clone)]
pub struct RequestStateCodec {
    key: [u8; REQUEST_STATE_KEY_BYTES],
    ttl: Duration,
}

impl RequestStateCodec {
    /// Builds a codec around a 32-byte key.
    #[must_use]
    pub fn new(key: [u8; REQUEST_STATE_KEY_BYTES], ttl: Duration) -> Self {
        Self { key, ttl }
    }

    /// Mints an opaque token for `state`, bound to `principal`.
    ///
    /// # Errors
    ///
    /// Returns [`McpError::InvalidParams`] when the state cannot be encoded, or
    /// [`McpError::Transport`] when request-state signing is unavailable.
    pub fn mint(
        &self,
        state: &RemoteEmbeddingRequestState,
        principal: &str,
    ) -> Result<String, McpError> {
        // Clock-unavailable direction: minting `0` is fail-closed —
        // `verify_at` rejects on an unreadable clock, and a healthy clock
        // reads age-from-`0` as ancient, exceeding any TTL.
        let envelope = StateEnvelope {
            issued_at_ms: UnixMillis::now_ms_or(0),
            method: state.method.clone(),
            principal: principal.to_owned(),
            state: state.clone(),
        };
        let payload = serde_json::to_vec(&envelope).map_err(|error| {
            McpError::invalid_params(format!("cannot encode request state: {error}"))
        })?;
        let tag = self.sign(&payload)?;
        Ok(format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(&payload),
            URL_SAFE_NO_PAD.encode(tag)
        ))
    }

    /// Verifies a token: signature, TTL, method/principal binding, and
    /// expected fields. Failures share one message (mirrors the TS
    /// `ProtocolError(-32602, "Invalid or expired requestState")`).
    ///
    /// # Errors
    ///
    /// Returns [`McpError::InvalidParams`] when the token is malformed, the signature
    /// or binding check fails, the token is expired, or the state does not match.
    pub fn verify(
        &self,
        token: &str,
        expected: &RequestStateFields,
        principal: &str,
    ) -> Result<RemoteEmbeddingRequestState, McpError> {
        // Fail closed: an unreadable clock expires every token (R2).
        self.verify_at(token, expected, principal, UnixMillis::try_now())
    }

    /// [`RequestStateCodec::verify`] with an injectable clock (tests pass
    /// `None` for a pre-epoch wall clock). A monotonic `Instant` cannot
    /// serve here: mint and verify are separated by a
    /// serialize/deserialize round-trip, so no in-memory instant survives —
    /// fail-closed wall-clock comparison is the sound option.
    ///
    /// # Errors
    ///
    /// Returns [`McpError::InvalidParams`] when the token is malformed, the signature
    /// or binding check fails, `now` is `None` or negative, the token is expired,
    /// or the state does not match.
    pub fn verify_at(
        &self,
        token: &str,
        expected: &RequestStateFields,
        principal: &str,
        now: Option<UnixMillis>,
    ) -> Result<RemoteEmbeddingRequestState, McpError> {
        let invalid = || McpError::invalid_params("Invalid or expired requestState");
        let (payload, tag) = token.split_once('.').ok_or_else(invalid)?;
        let payload = URL_SAFE_NO_PAD.decode(payload).map_err(|_| invalid())?;
        let tag: [u8; 32] = URL_SAFE_NO_PAD
            .decode(tag)
            .map_err(|_| invalid())?
            .try_into()
            .map_err(|_| invalid())?;
        self.verify_tag(&payload, &tag).map_err(|_| invalid())?;
        let envelope: StateEnvelope = serde_json::from_slice(&payload).map_err(|_| invalid())?;
        if envelope.method != BoundMethod::ToolsCall.as_str() || envelope.principal != principal {
            return Err(invalid());
        }
        // `None` (pre-epoch clock) or a negative stamp fails closed: the
        // token reads as expired rather than fresh.
        let Some(now) = now else {
            return Err(invalid());
        };
        let now_ms = u64::try_from(now.as_millis()).map_err(|_| invalid())?;
        if now_ms.saturating_sub(envelope.issued_at_ms) > self.ttl.as_millis() as u64 {
            return Err(invalid());
        }
        if !envelope.state.matches(expected) {
            return Err(invalid());
        }
        Ok(envelope.state)
    }

    fn mac(&self) -> Result<Hmac<Sha256>, McpError> {
        // HMAC accepts any key length, so this is infallible in practice;
        // the error arm exists so construction never panics.
        Hmac::<Sha256>::new_from_slice(&self.key).map_err(|_| McpError::Transport {
            message: "request-state signing is unavailable".to_owned(),
        })
    }

    fn sign(&self, payload: &[u8]) -> Result<[u8; 32], McpError> {
        let mut mac = self.mac()?;
        mac.update(payload);
        Ok(mac.finalize().into_bytes().into())
    }

    fn verify_tag(&self, payload: &[u8], tag: &[u8; 32]) -> Result<(), McpError> {
        let mut mac = self.mac()?;
        mac.update(payload);
        mac.verify_slice(tag)
            .map_err(|_| McpError::invalid_params("Invalid or expired requestState"))
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct StateEnvelope {
    issued_at_ms: u64,
    method: String,
    principal: String,
    state: RemoteEmbeddingRequestState,
}

/// Principal for state binding: the bearer token's fingerprint, or the
/// loopback-anonymous identity (mirrors TS `requestPrincipal`).
#[must_use]
pub fn request_principal(token: Option<&str>) -> String {
    match token {
        Some(token) => fingerprint(&serde_json::Value::String(token.to_owned())),
        None => "loopback-anonymous".to_owned(),
    }
}

/// Random 32-byte codec key (mirrors `randomBytes(32)` at server setup).
#[must_use]
pub fn random_state_key() -> [u8; REQUEST_STATE_KEY_BYTES] {
    let mut key = [0u8; REQUEST_STATE_KEY_BYTES];
    rand::rng().fill_bytes(&mut key);
    key
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fields() -> RequestStateFields {
        RequestStateFields {
            tool: "zvec_grep_search".to_owned(),
            arguments_fingerprint: fingerprint(&json!({"root": "/repo"})),
            target_fingerprint: "target".to_owned(),
            disclosure_fingerprint: "disclosure".to_owned(),
        }
    }

    #[test]
    fn codec_round_trip_and_tamper_rejection() {
        let codec = RequestStateCodec::new(random_state_key(), REQUEST_STATE_TTL);
        let guard = InMemoryRequestStateReplayGuard::new();
        let state = guard.issue(fields());
        let token = codec.mint(&state, "loopback-anonymous").unwrap();
        let verified = codec
            .verify(&token, &fields(), "loopback-anonymous")
            .unwrap();
        assert_eq!(verified.nonce, state.nonce);
        assert!(guard.consume(&verified));
        // Replay fails.
        assert!(!guard.consume(&verified));
        // Tampered payload fails.
        let mut tampered = token.clone();
        tampered.push('x');
        assert!(
            codec
                .verify(&tampered, &fields(), "loopback-anonymous")
                .is_err()
        );
        // Wrong principal fails.
        assert!(codec.verify(&token, &fields(), "someone-else").is_err());
    }

    #[test]
    fn expired_state_is_rejected() {
        let codec = RequestStateCodec::new(random_state_key(), Duration::from_millis(1));
        let guard = InMemoryRequestStateReplayGuard::new();
        let token = codec
            .mint(&guard.issue(fields()), "loopback-anonymous")
            .unwrap();
        std::thread::sleep(Duration::from_millis(5));
        assert!(
            codec
                .verify(&token, &fields(), "loopback-anonymous")
                .is_err()
        );
    }

    #[test]
    fn unavailable_or_garbage_clock_rejects() {
        let codec = RequestStateCodec::new(random_state_key(), REQUEST_STATE_TTL);
        let guard = InMemoryRequestStateReplayGuard::new();
        let token = codec
            .mint(&guard.issue(fields()), "loopback-anonymous")
            .unwrap();
        // Pre-epoch wall clock (`None`): fail closed, never fresh.
        assert!(
            codec
                .verify_at(&token, &fields(), "loopback-anonymous", None)
                .is_err()
        );
        // Negative stamp: fail closed too.
        assert!(
            codec
                .verify_at(
                    &token,
                    &fields(),
                    "loopback-anonymous",
                    Some(UnixMillis::from_millis(-1)),
                )
                .is_err()
        );
    }

    #[test]
    fn fingerprints_are_key_order_stable() {
        let left = json!({"b": 1, "a": 2});
        let right = json!({"a": 2, "b": 1});
        assert_eq!(fingerprint(&left), fingerprint(&right));
    }
}
