//! Search plan, hit, and trace types.

use serde::{Deserialize, Serialize};

use crate::ids::EntityId;
use crate::types::{
    CodeSymbolType, Content, Entity, EntityMetadata, FileInfo, Range, TimingEntry, UnixMillis,
};

/// Which recall paths contributed to a hit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SearchMatchedBy {
    Fts,
    Vector,
    #[serde(rename = "fts+vector")]
    FtsVector,
}

impl SearchMatchedBy {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Fts => "fts",
            Self::Vector => "vector",
            Self::FtsVector => "fts+vector",
        }
    }
}

/// Recall trace for a single route.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchRecallTrace {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    pub found: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forced: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rank: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Per-stage rank/score trace.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchStageTrace {
    pub rank: usize,
    pub score: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forced: Option<bool>,
}

/// Final-stage trace.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchFinalTrace {
    pub returned_by_limit: bool,
    pub cutoff_rank: usize,
}

/// Full trace for one hit.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchHitTrace {
    #[serde(default)]
    pub recall: Vec<SearchRecallTrace>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fusion: Option<SearchStageTrace>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ranking: Option<SearchStageTrace>,
    #[serde(rename = "final", default)]
    pub final_stage: SearchFinalTrace,
}

/// One piece of evidence backing a search hit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchHitEvidence {
    pub range: Range,
    pub content: Content,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<EntityMetadata>,
    pub is_entity: bool,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rank: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forced: Option<bool>,
}

/// A ranked search result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchHit {
    pub entity: Entity,
    pub file: FileInfo,
    pub evidence: Vec<SearchHitEvidence>,
    pub rank: usize,
    pub score: f64,
    pub matched_by: SearchMatchedBy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace: Option<SearchHitTrace>,
}

/// Recall mode for one search route.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SearchPlanRouteMode {
    Fts,
    Vector,
}

/// One query route before resolution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchPlanRoute {
    pub mode: SearchPlanRouteMode,
    pub query: String,
}

/// A route with its assigned identity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedSearchPlanRoute {
    pub id: String,
    pub mode: SearchPlanRouteMode,
    pub query: String,
}

/// Validated search request.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchPlan {
    #[serde(default)]
    pub routes: Vec<SearchPlanRoute>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track_entity_id: Option<EntityId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefer_symbol: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub symbol_types: Vec<CodeSymbolType>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub include_paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude_paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub globs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub insensitive_globs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub file_types: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub excluded_file_types: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified_after: Option<UnixMillis>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified_before: Option<UnixMillis>,
}

/// A search plan with resolved route identities.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedSearchPlan {
    #[serde(flatten)]
    pub plan: SearchPlan,
    pub routes: Vec<ResolvedSearchPlanRoute>,
}

/// Result of executing a search plan.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchPlanResult {
    pub plan: ResolvedSearchPlan,
    #[serde(default)]
    pub hits: Vec<SearchHit>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tracked_hit: Option<SearchHit>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timings: Option<Vec<TimingEntry>>,
}
