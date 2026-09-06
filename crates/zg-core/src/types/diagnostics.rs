//! Per-entity search diagnostics.

use serde::{Deserialize, Serialize};

use crate::types::{Entity, FileInfo};

/// Diagnostic bundle for a single entity search.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntitySearchDiagnosis {
    pub query: String,
    pub entity_id: String,
    pub file: FileInfo,
    pub entity: Entity,
    pub search: serde_json::Value,
}
