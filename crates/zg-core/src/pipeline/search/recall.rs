//! Adaptive recall: query embedding, FTS/vector passes, symbol extraction.

use std::collections::{HashMap, HashSet};

use crate::error::EngineResult;
use crate::ids::EntityId;
use crate::models::{EmbeddingInput, EmbeddingModel, EmbeddingPurpose};
use crate::storage::{StorageSearchFilter, StorageSearchHit, StoredEntity, WorkspaceIndexStorage};
use crate::types::{Entity, ResolvedSearchPlanRoute, SearchPlanRouteMode, SearchRecallTrace};

use super::fusion::{Candidate, CandidateEvidence, RecallPath, public_entity_id};

pub(crate) const RECALL_INITIAL_DEPTH: usize = 200;
const RECALL_MAX_DEPTH: usize = 2000;
const RECALL_GROWTH_FACTOR: usize = 2;
const RECALL_TARGET_FACTOR: usize = 5;
const RECALL_MIN_TARGET_CANDIDATES: usize = 50;

const SYMBOL_QUERY_KEYWORDS: &[&str] = &[
    "class",
    "struct",
    "enum",
    "interface",
    "function",
    "method",
    "type",
    "const",
    "let",
    "var",
    "namespace",
    "where",
    "find",
    "explain",
];

pub(crate) fn embed_vector_routes(
    routes: &[ResolvedSearchPlanRoute],
    model: &dyn EmbeddingModel,
    workspace_roots: &[String],
) -> EngineResult<HashMap<String, Vec<f32>>> {
    let vector_routes: Vec<&ResolvedSearchPlanRoute> = routes
        .iter()
        .filter(|route| route.mode == SearchPlanRouteMode::Vector)
        .collect();
    let mut by_route = HashMap::with_capacity(vector_routes.len());
    let max_batch = model.max_batch_size().max(1);
    for batch in vector_routes.chunks(max_batch) {
        let inputs: Vec<EmbeddingInput<'_>> = batch
            .iter()
            .map(|route| EmbeddingInput::Text {
                text: route.query.as_str(),
            })
            .collect();
        let result = model.embed_scoped(EmbeddingPurpose::Query, &inputs, workspace_roots)?;
        for (route, vector) in batch.iter().zip(result.vectors) {
            by_route.insert(route.id.clone(), vector);
        }
    }
    Ok(by_route)
}

struct RecallRoute {
    route: ResolvedSearchPlanRoute,
    filter: Option<StorageSearchFilter>,
    vector_route_id: Option<String>,
}

pub(crate) fn collect_adaptive_recall(
    routes: &[ResolvedSearchPlanRoute],
    filter: Option<&StorageSearchFilter>,
    prefer_symbol: bool,
    vector_by_route: &HashMap<String, Vec<f32>>,
    limit: usize,
    storage: &dyn WorkspaceIndexStorage,
    candidates: &mut HashMap<String, Candidate>,
) -> usize {
    let recall_routes = build_recall_routes(routes, filter, prefer_symbol);
    let target = recall_target_candidate_count(limit);
    let mut previous_depth = 0usize;
    let mut depth = RECALL_INITIAL_DEPTH;
    loop {
        let saturated = collect_recall_pass(
            &recall_routes,
            vector_by_route,
            depth,
            previous_depth,
            storage,
            candidates,
        );
        if candidates.len() >= target || !saturated || depth >= RECALL_MAX_DEPTH {
            return depth;
        }
        previous_depth = depth;
        depth = (depth * RECALL_GROWTH_FACTOR).min(RECALL_MAX_DEPTH);
    }
}

fn build_recall_routes(
    routes: &[ResolvedSearchPlanRoute],
    filter: Option<&StorageSearchFilter>,
    prefer_symbol: bool,
) -> Vec<RecallRoute> {
    let mut output: Vec<RecallRoute> = routes
        .iter()
        .map(|route| RecallRoute {
            route: route.clone(),
            filter: filter.cloned(),
            vector_route_id: if route.mode == SearchPlanRouteMode::Vector {
                Some(route.id.clone())
            } else {
                None
            },
        })
        .collect();
    if !prefer_symbol {
        return output;
    }
    let mut seen = HashSet::new();
    for route in routes {
        let symbol_names = extract_symbol_names(&route.query);
        if symbol_names.is_empty() {
            continue;
        }
        let key = format!("{}\0{}", route.id, symbol_names.join("\0"));
        if !seen.insert(key) {
            continue;
        }
        let mut prefer_filter = filter.cloned().unwrap_or_default();
        prefer_filter.symbol_names = symbol_names;
        output.push(RecallRoute {
            route: ResolvedSearchPlanRoute {
                id: format!("{}.prefer-symbol", route.id),
                mode: SearchPlanRouteMode::Fts,
                query: route.query.clone(),
            },
            filter: Some(prefer_filter),
            vector_route_id: None,
        });
    }
    output
}

fn collect_recall_pass(
    routes: &[RecallRoute],
    vector_by_route: &HashMap<String, Vec<f32>>,
    depth: usize,
    previous_depth: usize,
    storage: &dyn WorkspaceIndexStorage,
    candidates: &mut HashMap<String, Candidate>,
) -> bool {
    let mut saturated = false;
    for route in routes {
        let hits = recall_route_hits(route, vector_by_route, depth, storage);
        saturated = saturated || hits.len() >= depth;
        add_recall_hits(candidates, &hits, &route.route, storage, previous_depth);
    }
    saturated
}

fn recall_route_hits(
    route: &RecallRoute,
    vector_by_route: &HashMap<String, Vec<f32>>,
    depth: usize,
    storage: &dyn WorkspaceIndexStorage,
) -> Vec<StorageSearchHit> {
    if route.route.mode == SearchPlanRouteMode::Fts {
        return storage
            .search_fts(&route.route.query, depth, route.filter.as_ref())
            .unwrap_or_default();
    }
    let id = route
        .vector_route_id
        .as_deref()
        .unwrap_or(route.route.id.as_str());
    match vector_by_route.get(id) {
        Some(vector) => storage
            .search_vector(vector, depth, route.filter.as_ref())
            .unwrap_or_default(),
        None => Vec::new(),
    }
}

fn recall_target_candidate_count(limit: usize) -> usize {
    (limit * RECALL_TARGET_FACTOR).max(RECALL_MIN_TARGET_CANDIDATES)
}

pub(crate) fn add_recall_hits(
    candidates: &mut HashMap<String, Candidate>,
    hits: &[StorageSearchHit],
    route: &ResolvedSearchPlanRoute,
    storage: &dyn WorkspaceIndexStorage,
    start_index: usize,
) {
    let path = match route.mode {
        SearchPlanRouteMode::Fts => RecallPath::Fts,
        SearchPlanRouteMode::Vector => RecallPath::Vector,
    };
    for (index, hit) in hits.iter().enumerate().skip(start_index) {
        let rank = index + 1;
        let entity_id = public_entity_id(&hit.fragment).to_owned();
        if !candidates.contains_key(&entity_id) {
            let Some(stored) = resolve_hit_entity(hit, storage) else {
                continue;
            };
            candidates.insert(
                entity_id.clone(),
                Candidate::new(entity_id.clone(), stored.entity, stored.file, false),
            );
        }
        let Some(candidate) = candidates.get_mut(&entity_id) else {
            continue;
        };
        candidate.sources.insert(path);
        candidate.evidence.push(CandidateEvidence {
            fragment: hit.fragment.clone(),
            path,
            route_id: Some(route.id.clone()),
            query: Some(route.query.clone()),
            rank: Some(rank),
            score: Some(hit.score),
            forced: false,
        });
        candidate.add_or_update_recall(SearchRecallTrace {
            path: path.as_str().to_owned(),
            route_id: Some(route.id.clone()),
            query: Some(route.query.clone()),
            found: true,
            forced: None,
            rank: Some(rank),
            score: Some(hit.score),
            reason: None,
        });
    }
}

fn resolve_hit_entity(
    hit: &StorageSearchHit,
    storage: &dyn WorkspaceIndexStorage,
) -> Option<StoredEntity> {
    match hit.fragment.group.as_deref() {
        Some(group) if group != hit.fragment.entity.id.as_str() => {
            storage.get_entity(&EntityId::from_raw(group.to_owned()))
        }
        _ => Some(StoredEntity {
            entity: Entity {
                id: EntityId::from_raw(public_entity_id(&hit.fragment).to_owned()),
                file_id: hit.fragment.entity.file_id.clone(),
                range: hit.fragment.entity.range.clone(),
                content: hit.fragment.entity.content.clone(),
                metadata: hit.fragment.entity.metadata.clone(),
            },
            file: hit.file.clone(),
        }),
    }
}

fn extract_symbol_names(query: &str) -> Vec<String> {
    let mut names = HashSet::new();
    let mut token = String::new();
    for ch in query.chars().chain(std::iter::once(' ')) {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '~' || ch == ':' {
            token.push(ch);
        } else if !token.is_empty() {
            if let Some(name) = symbol_name_from_token(&token) {
                names.insert(name);
            }
            token.clear();
        }
    }
    let mut names: Vec<String> = names.into_iter().collect();
    names.sort();
    names
}

fn symbol_name_from_token(token: &str) -> Option<String> {
    if SYMBOL_QUERY_KEYWORDS.contains(&token.to_lowercase().as_str()) {
        return None;
    }
    let parts: Vec<&str> = token.split("::").filter(|part| !part.is_empty()).collect();
    let name = parts.last().copied().unwrap_or(token);
    if !is_symbol_name(name) {
        return None;
    }
    if parts.len() < 2 {
        return Some(name.to_owned());
    }
    let Some(owner) = parts.get(parts.len() - 2) else {
        return Some(name.to_owned());
    };
    if owner.starts_with(|ch: char| ch.is_ascii_uppercase() || ch == '_' || ch == '~') {
        return Some(format!("{owner}::{name}"));
    }
    Some(name.to_owned())
}

fn is_symbol_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(ch) if ch.is_ascii_alphabetic() || ch == '_' || ch == '~' => {}
        _ => return false,
    }
    chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '~')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbol_names_skip_keywords() {
        assert_eq!(
            extract_symbol_names("find class Foo"),
            vec!["Foo".to_owned()]
        );
        assert_eq!(
            extract_symbol_names("Owner::method"),
            vec!["Owner::method".to_owned()]
        );
    }
}
