//! Remote-embedding authorization domain records, mirroring
//! `src/authorization/types.ts` field-for-field on the wire.
//!
//! Fingerprints are newtypes with private fields (M2): a
//! `WorkspaceFingerprint` can only come from target hashing, never from a
//! raw string at a call site.

use serde::{Deserialize, Serialize};

use crate::types::UnixMillis;

/// Capability string every permit and grant carries (wire contract).
pub const REMOTE_EMBEDDING_CAPABILITY: &str = "remote_embedding";

/// SHA-256 hex over the canonical workspace-root list.
///
/// Private field: construct via `target` hashing.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WorkspaceFingerprint(String);

impl WorkspaceFingerprint {
    /// Wraps an already-hashed hex digest (hashing lives in `target.rs`).
    pub(crate) fn from_hex(hex: String) -> Self {
        Self(hex)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for WorkspaceFingerprint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// SHA-256 hex over `[workspaceFingerprint, provider, model, endpoint]`.
///
/// Private field: construct via `target` hashing.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TargetFingerprint(String);

impl TargetFingerprint {
    /// Wraps an already-hashed hex digest (hashing lives in `target.rs`).
    pub(crate) fn from_hex(hex: String) -> Self {
        Self(hex)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for TargetFingerprint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Which remote operation a plan authorizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteEmbeddingOperation {
    Query,
    Index,
    QueryAndIndex,
}

impl RemoteEmbeddingOperation {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Query => "query",
            Self::Index => "index",
            Self::QueryAndIndex => "query_and_index",
        }
    }
}

/// Grant scope (wire: `once` / `workspace`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RemoteEmbeddingScope {
    Once,
    Workspace,
}

impl RemoteEmbeddingScope {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Once => "once",
            Self::Workspace => "workspace",
        }
    }
}
/// How much workspace data a plan discloses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkspaceContentDisclosure {
    None,
    Selected,
    Changed,
    Full,
}

impl WorkspaceContentDisclosure {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Selected => "selected",
            Self::Changed => "changed",
            Self::Full => "full",
        }
    }
}

/// What a remote operation reveals.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RemoteEmbeddingDisclosure {
    pub query_text: bool,
    pub workspace_content: WorkspaceContentDisclosure,
}

/// Why a plan exists (wire: snake_case reason strings).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanReason {
    Query,
    IndexCreate,
    IndexUpdate,
    IndexRebuild,
}

/// A remote-embedding target: canonical roots plus provider identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteEmbeddingTarget {
    pub workspace_roots: Vec<String>,
    pub workspace_fingerprint: WorkspaceFingerprint,
    pub provider: String,
    pub model: String,
    pub endpoint: String,
    pub target_fingerprint: TargetFingerprint,
}

/// One authorization plan: operation plus target plus disclosure plus reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteEmbeddingPlan {
    pub operation: RemoteEmbeddingOperation,
    pub target: RemoteEmbeddingTarget,
    pub disclosure: RemoteEmbeddingDisclosure,
    pub reason: PlanReason,
    pub grant_path: std::path::PathBuf,
}

/// Short-lived capability handed to one operation call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteEmbeddingPermit {
    pub capability: &'static str,
    pub scope: RemoteEmbeddingScope,
    pub target: RemoteEmbeddingTarget,
    pub issued_at: UnixMillis,
    pub operation_id: String,
}
/// A remote-embedding request the guard checks.
///
/// The workspace binding is resolved server-side: `workspace_roots` names
/// the workspace the caller acts for, and both fingerprints MUST be the
/// canonical fingerprints over those roots (see
/// `create_remote_embedding_request`). The guard re-canonicalizes and
/// recomputes them, rejecting any request whose claimed fingerprints
/// disagree. A grant or permit for one workspace therefore never authorizes
/// traffic for another.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteEmbeddingRequest {
    pub provider: String,
    pub model: String,
    pub endpoint: String,
    pub purpose: super::error::RemoteEmbeddingPurpose,
    pub content_kinds: Vec<ContentKind>,
    pub content_count: usize,
    /// Canonical workspace roots this request acts for.
    pub workspace_roots: Vec<String>,
    /// Fingerprint over `workspace_roots` (must match a server-side recompute).
    pub workspace_fingerprint: WorkspaceFingerprint,
    /// Fingerprint over `[workspaceFingerprint, provider, model, endpoint]`.
    pub target_fingerprint: TargetFingerprint,
}

/// Content kind carried by a request (`text` / `image`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ContentKind {
    Text,
    Image,
}

/// On-disk workspace grant (`authorization.json` entry, camelCase wire).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteEmbeddingGrant {
    pub version: u32,
    pub id: String,
    pub capability: String,
    pub scope: RemoteEmbeddingScope,
    pub workspace_roots: Vec<String>,
    pub workspace_fingerprint: String,
    pub provider: String,
    pub model: String,
    pub endpoint: String,
    pub target_fingerprint: String,
    pub granted_at: i64,
    pub signature: String,
}

/// On-disk authorization document (`authorization.json`, camelCase wire).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteEmbeddingDocument {
    pub version: u32,
    #[serde(default)]
    pub grants: Vec<RemoteEmbeddingGrant>,
}

/// One grant with its signature stripped plus validity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrantStatus {
    pub version: u32,
    pub id: String,
    pub capability: String,
    pub scope: RemoteEmbeddingScope,
    pub workspace_roots: Vec<String>,
    pub workspace_fingerprint: String,
    pub provider: String,
    pub model: String,
    pub endpoint: String,
    pub target_fingerprint: String,
    pub granted_at: i64,
    pub valid: bool,
}

/// Grant listing for one workspace root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizationStatus {
    pub path: std::path::PathBuf,
    pub grants: Vec<GrantStatus>,
}
