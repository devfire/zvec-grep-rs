//! Forced entity tracking: guarantee a tracked entity appears with traces.

use std::collections::{HashMap, HashSet};

use crate::ids::{EntityId, FileId};
use crate::storage::{StorageSearchFilter, StorageSearchHit, WorkspaceIndexStorage};
use crate::types::{ResolvedSearchPlanRoute, SearchPlanRouteMode, SearchRecallTrace};

use super::filter::ResolvedPlanFilter;
use super::fusion::{Candidate, CandidateEvidence, RecallPath, public_entity_id};

pub(crate) fn force_track_entity(
    entity_id: &EntityId,
    routes: &[ResolvedSearchPlanRoute],
    vector_by_route: &HashMap<String, Vec<f32>>,
    recall_depth: usize,
    plan_filter: &ResolvedPlanFilter,
    storage: &dyn WorkspaceIndexStorage,
    candidates: &mut HashMap<String, Candidate>,
) {
    let Some(tracked) = storage.get_entity(entity_id) else {
        return;
    };
    let key = entity_id.as_str().to_owned();
    candidates.entry(key.clone()).or_insert_with(|| {
        Candidate::new(
            key.clone(),
            tracked.entity.clone(),
            tracked.file.clone(),
            true,
        )
    });
    let seen_routes: HashSet<Option<String>> = candidates
        .get(&key)
        .map(|candidate| {
            candidate
                .recall
                .iter()
                .map(|trace| trace.route_id.clone())
                .collect()
        })
        .unwrap_or_default();
    for route in routes {
        if seen_routes.contains(&Some(route.id.clone())) {
            continue;
        }
        let Some(candidate) = candidates.get_mut(&key) else {
            continue;
        };
        if plan_filter.matches_no_files {
            candidate.recall.push(SearchRecallTrace {
                path: route_mode_str(route.mode).to_owned(),
                route_id: Some(route.id.clone()),
                query: Some(route.query.clone()),
                found: false,
                forced: Some(true),
                rank: None,
                score: None,
                reason: Some("No files matched the path filters".to_owned()),
            });
            continue;
        }
        if filter_excludes_file(plan_filter.filter.as_ref(), &tracked.file.id) {
            candidate.recall.push(SearchRecallTrace {
                path: route_mode_str(route.mode).to_owned(),
                route_id: Some(route.id.clone()),
                query: Some(route.query.clone()),
                found: false,
                forced: Some(true),
                rank: None,
                score: None,
                reason: Some("Target entity file was excluded by the path filters".to_owned()),
            });
            continue;
        }
        if route.mode == SearchPlanRouteMode::Fts {
            force_track_fts_route(
                candidate,
                entity_id,
                &tracked.file.id,
                recall_depth,
                plan_filter.filter.as_ref(),
                storage,
                route,
            );
        } else {
            force_track_vector_route(
                candidate,
                &tracked.file.id,
                recall_depth,
                plan_filter.filter.as_ref(),
                storage,
                vector_by_route,
                route,
            );
        }
    }
}

fn route_mode_str(mode: SearchPlanRouteMode) -> &'static str {
    match mode {
        SearchPlanRouteMode::Fts => "fts",
        SearchPlanRouteMode::Vector => "vector",
    }
}

fn filter_excludes_file(filter: Option<&StorageSearchFilter>, file_id: &FileId) -> bool {
    match filter {
        Some(filter) if !filter.file_ids.is_empty() => !filter.file_ids.contains(file_id),
        _ => false,
    }
}

fn force_track_fts_route(
    candidate: &mut Candidate,
    entity_id: &EntityId,
    target_file_id: &FileId,
    recall_depth: usize,
    filter: Option<&StorageSearchFilter>,
    storage: &dyn WorkspaceIndexStorage,
    route: &ResolvedSearchPlanRoute,
) {
    let hit = search_tracked_entity_fts(
        entity_id,
        target_file_id,
        recall_depth,
        filter,
        storage,
        route,
    );
    match hit {
        Some(hit) => {
            candidate.sources.insert(RecallPath::Fts);
            candidate.evidence.push(CandidateEvidence {
                fragment: hit.fragment.clone(),
                path: RecallPath::Fts,
                route_id: Some(route.id.clone()),
                query: Some(route.query.clone()),
                rank: Some(recall_depth + 1),
                score: Some(hit.score),
                forced: true,
            });
            candidate.add_or_update_recall(SearchRecallTrace {
                path: "fts".to_owned(),
                route_id: Some(route.id.clone()),
                query: Some(route.query.clone()),
                found: true,
                forced: Some(true),
                rank: Some(recall_depth + 1),
                score: Some(hit.score),
                reason: None,
            });
        }
        None => candidate.recall.push(SearchRecallTrace {
            path: "fts".to_owned(),
            route_id: Some(route.id.clone()),
            query: Some(route.query.clone()),
            found: false,
            forced: Some(true),
            rank: None,
            score: None,
            reason: Some("Target entity did not match the FTS query".to_owned()),
        }),
    }
}

fn force_track_vector_route(
    candidate: &mut Candidate,
    target_file_id: &FileId,
    recall_depth: usize,
    filter: Option<&StorageSearchFilter>,
    storage: &dyn WorkspaceIndexStorage,
    vector_by_route: &HashMap<String, Vec<f32>>,
    route: &ResolvedSearchPlanRoute,
) {
    let Some(vector) = vector_by_route.get(&route.id) else {
        candidate.recall.push(SearchRecallTrace {
            path: "vector".to_owned(),
            route_id: Some(route.id.clone()),
            query: Some(route.query.clone()),
            found: false,
            forced: Some(true),
            rank: None,
            score: None,
            reason: Some("Vector route was not available for this query".to_owned()),
        });
        return;
    };
    let hit = search_tracked_entity_vector(
        &candidate.id,
        target_file_id,
        recall_depth,
        filter,
        storage,
        vector,
    );
    match hit {
        Some(hit) => {
            candidate.sources.insert(RecallPath::Vector);
            candidate.evidence.push(CandidateEvidence {
                fragment: hit.fragment.clone(),
                path: RecallPath::Vector,
                route_id: Some(route.id.clone()),
                query: Some(route.query.clone()),
                rank: Some(recall_depth + 1),
                score: Some(hit.score),
                forced: true,
            });
            candidate.add_or_update_recall(SearchRecallTrace {
                path: "vector".to_owned(),
                route_id: Some(route.id.clone()),
                query: Some(route.query.clone()),
                found: true,
                forced: Some(true),
                rank: Some(recall_depth + 1),
                score: Some(hit.score),
                reason: None,
            });
        }
        None => candidate.recall.push(SearchRecallTrace {
            path: "vector".to_owned(),
            route_id: Some(route.id.clone()),
            query: Some(route.query.clone()),
            found: false,
            forced: Some(true),
            rank: None,
            score: None,
            reason: Some("Target entity could not be scored by vector search".to_owned()),
        }),
    }
}

fn search_tracked_entity_fts(
    entity_id: &EntityId,
    target_file_id: &FileId,
    recall_depth: usize,
    filter: Option<&StorageSearchFilter>,
    storage: &dyn WorkspaceIndexStorage,
    route: &ResolvedSearchPlanRoute,
) -> Option<StorageSearchHit> {
    let mut group_filter = filter.cloned().unwrap_or_default();
    group_filter.group_ids = vec![entity_id.as_str().to_owned()];
    if let Some(hit) = storage
        .search_fts(&route.query, 1, Some(&group_filter))
        .unwrap_or_default()
        .into_iter()
        .next()
    {
        return Some(hit);
    }
    let mut file_filter = filter.cloned().unwrap_or_default();
    file_filter.file_ids = vec![target_file_id.clone()];
    storage
        .search_fts(&route.query, recall_depth, Some(&file_filter))
        .unwrap_or_default()
        .into_iter()
        .find(|hit| public_entity_id(&hit.fragment) == entity_id.as_str())
}

fn search_tracked_entity_vector(
    entity_id: &str,
    target_file_id: &FileId,
    recall_depth: usize,
    filter: Option<&StorageSearchFilter>,
    storage: &dyn WorkspaceIndexStorage,
    vector: &[f32],
) -> Option<StorageSearchHit> {
    let mut group_filter = filter.cloned().unwrap_or_default();
    group_filter.group_ids = vec![entity_id.to_owned()];
    if let Some(hit) = storage
        .search_vector(vector, 1, Some(&group_filter))
        .unwrap_or_default()
        .into_iter()
        .next()
    {
        return Some(hit);
    }
    let mut file_filter = filter.cloned().unwrap_or_default();
    file_filter.file_ids = vec![target_file_id.clone()];
    storage
        .search_vector(vector, recall_depth, Some(&file_filter))
        .unwrap_or_default()
        .into_iter()
        .find(|hit| public_entity_id(&hit.fragment) == entity_id)
}
