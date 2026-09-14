//! Search diagnosis: why one entity (or the best entity in a file) matches.

use std::collections::HashMap;

use crate::error::{EngineError, EngineErrorCode, EngineResult};
use crate::ids::EntityId;
use crate::models::{EmbeddingInput, EmbeddingPurpose};
use crate::storage::{ListEntitiesOptions, StorageSearchFilter};
use crate::types::{
    EntitySearchDiagnosis, FileInfo, ResolvedSearchPlanRoute, SearchPlan, SearchPlanRoute,
    SearchPlanRouteMode,
};

use super::fusion::{Candidate, fuse_candidates};
use super::plan::require_embedding_model;
use super::recall::add_recall_hits;
use super::{SearchContext, search_workspace_index};

/// Diagnoses why one entity does or does not match (mirrors
/// `diagnoseEntitySearch`).
///
/// # Errors
///
/// Returns `SEARCH.ENTITY_NOT_FOUND` when the entity is absent,
/// `SEARCH.DIAGNOSIS_ENCODE_FAILED` when the diagnosis cannot be encoded, or the
/// nested plan-search error.
pub fn diagnose_entity_search(
    query: &str,
    entity_id: &EntityId,
    ctx: &SearchContext<'_>,
) -> EngineResult<EntitySearchDiagnosis> {
    let Some(stored) = ctx.storage.get_entity(entity_id) else {
        return Err(
            EngineError::new(EngineErrorCode::SearchEntityNotFound, "entity not found")
                .with_context(format!("entityId={}", entity_id.as_str())),
        );
    };
    let plan = SearchPlan {
        routes: vec![
            SearchPlanRoute {
                mode: SearchPlanRouteMode::Fts,
                query: query.to_owned(),
            },
            SearchPlanRoute {
                mode: SearchPlanRouteMode::Vector,
                query: query.to_owned(),
            },
        ],
        trace: Some(true),
        track_entity_id: Some(entity_id.clone()),
        ..SearchPlan::default()
    };
    let search = search_workspace_index(&plan, ctx)?;
    Ok(EntitySearchDiagnosis {
        query: query.to_owned(),
        entity_id: entity_id.as_str().to_owned(),
        file: stored.file,
        entity: stored.entity,
        search: serde_json::to_value(&search).map_err(|err| {
            EngineError::new(
                EngineErrorCode::SearchDiagnosisEncodeFailed,
                "search diagnosis could not be encoded",
            )
            .with_context(format!("detail={err}"))
        })?,
    })
}

/// Diagnoses the best entity in a file (mirrors `diagnoseFileSearch`).
///
/// # Errors
///
/// Returns an error when best-entity selection or the nested entity diagnosis fails.
pub fn diagnose_file_search(
    query: &str,
    absolute_path: &str,
    ctx: &SearchContext<'_>,
) -> EngineResult<Option<EntitySearchDiagnosis>> {
    let Some(file) = ctx.storage.get_file_by_path(absolute_path) else {
        return Ok(None);
    };
    let Some(entity_id) = choose_best_entity_in_file(query, &file, ctx)? else {
        return Ok(None);
    };
    diagnose_entity_search(query, &entity_id, ctx).map(Some)
}

fn choose_best_entity_in_file(
    query: &str,
    file: &FileInfo,
    ctx: &SearchContext<'_>,
) -> EngineResult<Option<EntityId>> {
    let mut candidates: HashMap<String, Candidate> = HashMap::new();
    let route = ResolvedSearchPlanRoute {
        id: "fts".to_owned(),
        mode: SearchPlanRouteMode::Fts,
        query: query.to_owned(),
    };
    let hits = ctx
        .storage
        .search_fts(
            query,
            10,
            Some(&StorageSearchFilter {
                file_ids: vec![file.id.clone()],
                ..StorageSearchFilter::default()
            }),
        )
        .unwrap_or_default();
    add_recall_hits(&mut candidates, &hits, &route, ctx.storage, 0);
    let model = require_embedding_model(ctx, "diagnose")?;
    let inputs = [EmbeddingInput::Text { text: query }];
    let result = model.embed(EmbeddingPurpose::Query, &inputs)?;
    let Some(query_vector) = result.vectors.into_iter().next() else {
        return Ok(None);
    };
    let route = ResolvedSearchPlanRoute {
        id: "vector".to_owned(),
        mode: SearchPlanRouteMode::Vector,
        query: query.to_owned(),
    };
    let hits = ctx
        .storage
        .search_vector(
            &query_vector,
            10,
            Some(&StorageSearchFilter {
                file_ids: vec![file.id.clone()],
                ..StorageSearchFilter::default()
            }),
        )
        .unwrap_or_default();
    add_recall_hits(&mut candidates, &hits, &route, ctx.storage, 0);
    let mut fused: Vec<Candidate> = candidates.into_values().collect();
    fuse_candidates(&mut fused);
    if let Some(best) = fused.into_iter().next() {
        return Ok(Some(EntityId::from_raw(best.id)));
    }
    Ok(ctx
        .storage
        .list_entities_by_file(
            &file.id,
            ListEntitiesOptions {
                limit: Some(1),
                offset: None,
            },
        )
        .into_iter()
        .next()
        .map(|stored| stored.entity.id))
}
