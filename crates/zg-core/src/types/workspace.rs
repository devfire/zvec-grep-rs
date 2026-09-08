//! Workspace index identity and embedding schema types.

use serde::{Deserialize, Serialize};

use crate::types::{RootPath, UnixMillis};

/// On-disk index layout version.
///
/// This build is standalone: it never reads TypeScript-generation indexes
/// and TS never reads its own. The value matches the TS constant by
/// coincidence of a shared origin, not by compatibility (see
/// `docs/ts-divergence.md`).
/// Bumped for the `Range` wire-shape alignment (`startLine` camelCase
/// fields): v1 indexes persist snake_case ranges and are rejected with
/// `WORKSPACE_INDEX.VERSION_MISMATCH`, directing a rebuild.
pub const CURRENT_INDEX_VERSION: i64 = 2;

/// Distance metric used by the embedding vector index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SearchMetric {
    Cosine,
    Dot,
    Euclidean,
}

/// Embedding model fingerprint recorded in the manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceIndexEmbeddingSchema {
    pub provider: String,
    pub model: String,
    pub dimension: usize,
    pub metric: SearchMetric,
}

/// Whether the index is actively maintained.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkspaceIndexPolicy {
    Enabled,
    Disabled,
}

/// Identity and configuration of one workspace index.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceIndexInfo {
    pub id: String,
    pub name: String,
    pub path: String,
    pub root_paths: Vec<RootPath>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index_policy: Option<WorkspaceIndexPolicy>,
    /// `None` when absent, `Some(None)` when explicitly null in JSON.
    #[serde(
        default,
        with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub embedding: Option<Option<WorkspaceIndexEmbeddingSchema>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index_version: Option<i64>,
    pub created_time: UnixMillis,
    pub updated_time: UnixMillis,
}

/// Serde helper: distinguishes an absent field from an explicit `null`.
pub mod double_option {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<T: Serialize, S: Serializer>(
        value: &Option<Option<T>>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match value {
            Some(inner) => inner.serialize(serializer),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, T: Deserialize<'de>, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Option<T>>, D::Error> {
        Ok(Some(Option::<T>::deserialize(deserializer)?))
    }
}
