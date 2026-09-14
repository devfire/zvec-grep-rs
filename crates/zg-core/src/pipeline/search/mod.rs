//! Hybrid search: plan validation, adaptive recall, force-tracking, fusion.
//!
//! Port of `engine/pipeline/search/index.ts` (`searchWorkspaceIndex`,
//! `diagnoseEntitySearch`, `diagnoseFileSearch`). Synchronous; recall depth
//! adapts from 200 to 2000 exactly like the TS loop.

use std::collections::HashMap;
use std::sync::Arc;

use crate::error::{EngineError, EngineResult};
use crate::models::EmbeddingModel;
use crate::storage::WorkspaceIndexStorage;
use crate::types::{SearchHit, SearchPlan, SearchPlanResult, WorkspaceIndexInfo};
use crate::utils::file_selection::resolve_file_types;
use crate::utils::timing::TimingCollector;

use self::filter::search_plan_to_storage_filter;
use self::force_track::force_track_entity;
use self::fusion::{Candidate, candidate_to_hit, fuse_candidates};
use self::plan::{plan_uses_vector, require_embedding_model, validate_search_plan};
use self::recall::{RECALL_INITIAL_DEPTH, collect_adaptive_recall, embed_vector_routes};

pub mod diagnose;
pub mod filter;
pub mod force_track;
pub mod fusion;
pub mod plan;
pub mod recall;

pub use diagnose::{diagnose_entity_search, diagnose_file_search};

/// Search inputs (mirrors `SearchContext`).
pub struct SearchContext<'a> {
    pub workspace_index: WorkspaceIndexInfo,
    pub storage: &'a dyn WorkspaceIndexStorage,
    pub embedding_model: Option<Arc<dyn EmbeddingModel>>,
}

const DEFAULT_LIMIT: usize = 7;

/// Executes a validated search plan (mirrors `searchWorkspaceIndex`).
///
/// # Errors
///
/// Returns `SEARCH_PLAN.EMPTY_ROUTES` when the plan has no routes, or an error when
/// file-type resolution, query embedding, recall, or forced entity tracking fails.
pub fn search_workspace_index(
    plan: &SearchPlan,
    ctx: &SearchContext<'_>,
) -> EngineResult<SearchPlanResult> {
    let mut timings = TimingCollector::new();
    let normalized = timings.time("search_plan", || validate_search_plan(plan))?;
    let limit = normalized.plan.limit.unwrap_or(DEFAULT_LIMIT);
    let trace = normalized.plan.trace == Some(true) || normalized.plan.track_entity_id.is_some();
    let file_type_matcher = timings.time("search_file_types", || {
        resolve_file_types(
            &normalized.plan.file_types,
            &normalized.plan.excluded_file_types,
        )
    })?;
    let plan_filter = timings.time("search_filter", || {
        Ok::<_, EngineError>(search_plan_to_storage_filter(
            &normalized,
            ctx.storage,
            &file_type_matcher,
        ))
    })?;
    let mut candidates: HashMap<String, Candidate> = HashMap::new();
    let mut vector_by_route: HashMap<String, Vec<f32>> = HashMap::new();
    if !plan_filter.matches_no_files && plan_uses_vector(&normalized) {
        let embedded = timings.time("query_embedding", || {
            embed_vector_routes(
                &normalized.routes,
                require_embedding_model(ctx, "searchPlan")?,
            )
        })?;
        vector_by_route = embedded;
    }
    let mut recall_depth = RECALL_INITIAL_DEPTH;
    if !plan_filter.matches_no_files {
        recall_depth = timings.time("recall", || {
            Ok::<_, EngineError>(collect_adaptive_recall(
                &normalized.routes,
                plan_filter.filter.as_ref(),
                normalized.plan.prefer_symbol == Some(true),
                &vector_by_route,
                limit,
                ctx.storage,
                &mut candidates,
            ))
        })?;
    }
    if let Some(tracked_id) = normalized.plan.track_entity_id.clone() {
        timings.time("force_track", || {
            force_track_entity(
                &tracked_id,
                &normalized.routes,
                &vector_by_route,
                recall_depth,
                &plan_filter,
                ctx.storage,
                &mut candidates,
            );
            Ok::<_, EngineError>(())
        })?;
    }
    let mut fused: Vec<Candidate> = timings.time("fusion", || {
        let mut fused: Vec<Candidate> = candidates.into_values().collect();
        fuse_candidates(&mut fused);
        Ok::<_, EngineError>(fused)
    })?;
    let mut visible: Vec<Candidate> = fused.drain(..limit.min(fused.len())).collect();
    let tracked = normalized.plan.track_entity_id.as_ref().and_then(|id| {
        fused
            .iter()
            .find(|candidate| candidate.id.as_str() == id.as_str())
            .cloned()
    });
    if let Some(tracked) = tracked
        && !visible.iter().any(|candidate| candidate.id == tracked.id)
    {
        visible.push(tracked);
    }
    let hits: Vec<SearchHit> = timings.time("materialize", || {
        Ok::<_, EngineError>(
            visible
                .iter()
                .map(|candidate| candidate_to_hit(candidate, limit, trace))
                .collect(),
        )
    })?;
    let tracked_hit = normalized
        .plan
        .track_entity_id
        .as_ref()
        .and_then(|id| hits.iter().find(|hit| &hit.entity.id == id).cloned());
    let mut result = timings.time("search_total", || {
        Ok::<_, EngineError>(SearchPlanResult {
            plan: normalized,
            hits,
            tracked_hit,
            timings: None,
        })
    })?;
    result.timings = Some(timings.entries());
    Ok(result)
}
