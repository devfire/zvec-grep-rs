//! Hybrid search: plan validation, adaptive recall, force-tracking, fusion.
//!
//! Port of `engine/pipeline/search/index.ts` (`searchWorkspaceIndex`,
//! `diagnoseEntitySearch`, `diagnoseFileSearch`). Synchronous; recall depth
//! adapts from 200 to 2000 exactly like the TS loop.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::error::{
    error_details, workspace_index_detail, DetailEntry, DetailValue, EngineError, EngineErrorCode,
    EngineResult,
};
use crate::ids::EntityId;
use crate::models::{EmbeddingInput, EmbeddingModel, EmbeddingPurpose};
use crate::storage::{
    ListEntitiesOptions, StorageSearchFilter, StorageSearchHit, StoredEntity, WorkspaceIndexStorage,
};
use crate::types::{
    Entity, EntitySearchDiagnosis, FileInfo, ResolvedSearchPlan, ResolvedSearchPlanRoute,
    SearchHit, SearchPlan, SearchPlanResult, SearchPlanRoute, SearchPlanRouteMode,
    SearchRecallTrace, UnixMillis, WorkspaceIndexInfo,
};
use crate::utils::file_selection::{resolve_file_types, FileTypesMatcher, OrderedGlobs};
use crate::utils::glob::{
    has_path_glob, is_absolute_path_pattern, normalize_path_for_match, normalize_path_pattern,
    path_pattern_matches,
};
use crate::utils::timing::TimingCollector;

use self::fusion::{
    candidate_to_hit, fuse_candidates, public_entity_id, Candidate, CandidateEvidence, RecallPath,
};

pub mod fusion;

/// Search inputs (mirrors `SearchContext`).
pub struct SearchContext<'a> {
    pub workspace_index: WorkspaceIndexInfo,
    pub storage: &'a dyn WorkspaceIndexStorage,
    pub embedding_model: Option<Arc<dyn EmbeddingModel>>,
}

const DEFAULT_LIMIT: usize = 7;
const RECALL_INITIAL_DEPTH: usize = 200;
const RECALL_MAX_DEPTH: usize = 2000;
const RECALL_GROWTH_FACTOR: usize = 2;
const RECALL_TARGET_FACTOR: usize = 5;
const RECALL_MIN_TARGET_CANDIDATES: usize = 50;

const SYMBOL_QUERY_KEYWORDS: &[&str] = &[
    "class", "struct", "enum", "interface", "function", "method", "type", "const", "let",
    "var", "namespace", "where", "find", "explain",
];

/// Storage filter plus whether the file-id dimension resolved to an empty
/// set. The TS filter distinguishes "absent `fileIds`" from "present but
/// empty" (match nothing); the Rust filter uses plain vectors, so the flag
/// carries that bit (mirrors `filterMatchesNoFiles`).
struct ResolvedPlanFilter {
    filter: Option<StorageSearchFilter>,
    matches_no_files: bool,
}

/// Executes a validated search plan (mirrors `searchWorkspaceIndex`).
pub fn search_workspace_index(
    plan: &SearchPlan,
    ctx: &SearchContext<'_>,
) -> EngineResult<SearchPlanResult> {
    let mut timings = TimingCollector::new();
    let normalized = timings.time("search_plan", || validate_search_plan(plan))?;
    let limit = normalized.plan.limit.unwrap_or(DEFAULT_LIMIT);
    let trace = normalized.plan.trace == Some(true) || normalized.plan.track_entity_id.is_some();
    let file_type_matcher = timings.time("search_file_types", || {
        resolve_file_types(&normalized.plan.file_types, &normalized.plan.excluded_file_types)
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
            embed_vector_routes(&normalized.routes, require_embedding_model(ctx, "searchPlan")?)
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
    let tracked = normalized
        .plan
        .track_entity_id
        .as_ref()
        .and_then(|id| fused.iter().find(|candidate| candidate.id.as_str() == id.as_str()).cloned());
    if let Some(tracked) = tracked {
        if !visible.iter().any(|candidate| candidate.id == tracked.id) {
            visible.push(tracked);
        }
    }
    let hits: Vec<SearchHit> = timings.time("materialize", || {
        Ok::<_, EngineError>(
            visible.iter().map(|candidate| candidate_to_hit(candidate, limit, trace)).collect(),
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

/// Diagnoses why one entity does or does not match (mirrors
/// `diagnoseEntitySearch`).
pub fn diagnose_entity_search(
    query: &str,
    entity_id: &EntityId,
    ctx: &SearchContext<'_>,
) -> EngineResult<EntitySearchDiagnosis> {
    let Some(stored) = ctx.storage.get_entity(entity_id) else {
        return Err(EngineError::new(
            EngineErrorCode::new("SEARCH.ENTITY_NOT_FOUND"),
            "entity not found",
        )
        .with_context(format!("entityId={}", entity_id.as_str())));
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
                EngineErrorCode::new("SEARCH.DIAGNOSIS_ENCODE_FAILED"),
                "search diagnosis could not be encoded",
            )
            .with_context(format!("detail={err}"))
        })?,
    })
}

/// Diagnoses the best entity in a file (mirrors `diagnoseFileSearch`).
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

fn validate_search_plan(plan: &SearchPlan) -> EngineResult<ResolvedSearchPlan> {
    if plan.routes.is_empty() {
        return Err(EngineError::new(
            EngineErrorCode::new("SEARCH_PLAN.EMPTY_ROUTES"),
            "search plan requires at least one route",
        ));
    }
    let mut used_ids = HashSet::new();
    let mut counts: HashMap<SearchPlanRouteMode, usize> = HashMap::new();
    let mut routes = Vec::with_capacity(plan.routes.len());
    for route in &plan.routes {
        let query = route.query.trim().to_owned();
        let id = make_default_route_id(route.mode, &mut counts, &used_ids);
        if query.is_empty() {
            return Err(EngineError::new(
                EngineErrorCode::new("SEARCH_PLAN.EMPTY_ROUTE_QUERY"),
                "search plan route requires a non-empty query",
            )
            .with_context(format!("routeId={id}")));
        }
        used_ids.insert(id.clone());
        routes.push(ResolvedSearchPlanRoute {
            id,
            mode: route.mode,
            query,
        });
    }
    let modified_after = normalize_modified_time(plan.modified_after, "modifiedAfter")?;
    let modified_before = normalize_modified_time(plan.modified_before, "modifiedBefore")?;
    if let (Some(after), Some(before)) = (modified_after, modified_before) {
        if after.0 > before.0 {
            return Err(EngineError::new(
                EngineErrorCode::new("SEARCH_PLAN.INVALID_MODIFIED_TIME_RANGE"),
                "search plan modified-after filter must not be later than modified-before",
            )
            .with_context(format!("modifiedAfter={} modifiedBefore={}", after.0, before.0)));
        }
    }
    Ok(ResolvedSearchPlan {
        plan: SearchPlan {
            routes: plan.routes.clone(),
            limit: plan.limit,
            trace: plan.trace,
            track_entity_id: plan.track_entity_id.clone(),
            prefer_symbol: plan.prefer_symbol,
            symbol_types: plan.symbol_types.clone(),
            include_paths: normalize_path_filters(&plan.include_paths, "includePaths")?,
            exclude_paths: normalize_path_filters(&plan.exclude_paths, "excludePaths")?,
            globs: normalize_string_filters(&plan.globs, "globs")?,
            insensitive_globs: normalize_string_filters(&plan.insensitive_globs, "insensitiveGlobs")?,
            file_types: normalize_string_filters(&plan.file_types, "fileTypes")?,
            excluded_file_types: normalize_string_filters(&plan.excluded_file_types, "excludedFileTypes")?,
            modified_after,
            modified_before,
        },
        routes,
    })
}

fn make_default_route_id(
    mode: SearchPlanRouteMode,
    counts: &mut HashMap<SearchPlanRouteMode, usize>,
    used: &HashSet<String>,
) -> String {
    let base = match mode {
        SearchPlanRouteMode::Fts => "fts",
        SearchPlanRouteMode::Vector => "vector",
    };
    let mut count = counts.get(&mode).copied().unwrap_or(0);
    loop {
        count += 1;
        let id = if count == 1 {
            base.to_owned()
        } else {
            format!("{base}-{count}")
        };
        if !used.contains(&id) {
            counts.insert(mode, count);
            return id;
        }
    }
}

fn plan_uses_vector(plan: &ResolvedSearchPlan) -> bool {
    plan.routes.iter().any(|route| route.mode == SearchPlanRouteMode::Vector)
}

fn require_embedding_model<'a>(
    ctx: &'a SearchContext<'_>,
    operation: &str,
) -> EngineResult<&'a dyn EmbeddingModel> {
    ctx.embedding_model.as_deref().ok_or_else(|| {
        let detail = error_details(vec![
            DetailEntry::Line(&workspace_index_detail(&ctx.workspace_index.name)),
            DetailEntry::Pair("operation", DetailValue::Str(operation)),
        ])
        .unwrap_or_default();
        EngineError::new(
            EngineErrorCode::new("SEARCH.EMBEDDING_MODEL_REQUIRED"),
            "search operation requires an embedding model",
        )
        .with_context(detail)
    })
}

fn normalize_path_filters(values: &[String], field: &str) -> EngineResult<Vec<String>> {
    let mut patterns = Vec::new();
    for (index, item) in values.iter().enumerate() {
        if item.trim().is_empty() {
            return Err(EngineError::new(
                EngineErrorCode::new("SEARCH_PLAN.INVALID_PATH_FILTER"),
                "search plan path filters must contain strings",
            )
            .with_context(format!("field={field} index={index}")));
        }
        let pattern = normalize_path_pattern(item.trim());
        if !pattern.is_empty() {
            patterns.push(pattern);
        }
    }
    Ok(patterns)
}

fn normalize_string_filters(values: &[String], field: &str) -> EngineResult<Vec<String>> {
    let mut out = Vec::new();
    for (index, item) in values.iter().enumerate() {
        if item.trim().is_empty() {
            return Err(EngineError::new(
                EngineErrorCode::new("SEARCH_PLAN.INVALID_FILTER"),
                "search plan filters must contain strings",
            )
            .with_context(format!("field={field} index={index}")));
        }
        out.push(item.trim().to_owned());
    }
    Ok(out)
}

fn normalize_modified_time(
    value: Option<UnixMillis>,
    field: &str,
) -> EngineResult<Option<UnixMillis>> {
    match value {
        None => Ok(None),
        Some(time) if time.0 >= 0 => Ok(Some(time)),
        Some(time) => Err(EngineError::new(
            EngineErrorCode::new("SEARCH_PLAN.INVALID_MODIFIED_TIME_FILTER"),
            "search plan modified time filters must be non-negative epoch milliseconds",
        )
        .with_context(format!("field={field} value={}", time.0))),
    }
}

fn embed_vector_routes(
    routes: &[ResolvedSearchPlanRoute],
    model: &dyn EmbeddingModel,
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
        let result = model.embed(EmbeddingPurpose::Query, &inputs)?;
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

fn collect_adaptive_recall(
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
        let saturated =
            collect_recall_pass(&recall_routes, vector_by_route, depth, previous_depth, storage, candidates);
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
    let id = route.vector_route_id.as_deref().unwrap_or(route.route.id.as_str());
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

fn add_recall_hits(
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
        let candidate = candidates.get_mut(&entity_id).expect("candidate just inserted");
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
    let group = hit.fragment.group.as_deref();
    if group.is_none_or(|group| group == hit.fragment.entity.id.as_str()) {
        return Some(StoredEntity {
            entity: Entity {
                id: crate::ids::EntityId::from_raw(public_entity_id(&hit.fragment).to_owned()),
                file_id: hit.fragment.entity.file_id.clone(),
                range: hit.fragment.entity.range.clone(),
                content: hit.fragment.entity.content.clone(),
                metadata: hit.fragment.entity.metadata.clone(),
            },
            file: hit.file.clone(),
        });
    }
    let group_id = crate::ids::EntityId::from_raw(group.expect("group checked above").to_owned());
    storage.get_entity(&group_id)
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
    let owner = parts[parts.len() - 2];
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

fn force_track_entity(
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
        Candidate::new(key.clone(), tracked.entity.clone(), tracked.file.clone(), true)
    });
    let seen_routes: HashSet<Option<String>> = candidates
        .get(&key)
        .map(|candidate| candidate.recall.iter().map(|trace| trace.route_id.clone()).collect())
        .unwrap_or_default();
    for route in routes {
        if seen_routes.contains(&Some(route.id.clone())) {
            continue;
        }
        let candidate = candidates.get_mut(&key).expect("candidate just inserted");
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

fn filter_excludes_file(filter: Option<&StorageSearchFilter>, file_id: &crate::ids::FileId) -> bool {
    match filter {
        Some(filter) if !filter.file_ids.is_empty() => !filter.file_ids.contains(file_id),
        _ => false,
    }
}

fn force_track_fts_route(
    candidate: &mut Candidate,
    entity_id: &EntityId,
    target_file_id: &crate::ids::FileId,
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
    target_file_id: &crate::ids::FileId,
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
    target_file_id: &crate::ids::FileId,
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
    target_file_id: &crate::ids::FileId,
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

fn search_plan_to_storage_filter(
    plan: &ResolvedSearchPlan,
    storage: &dyn WorkspaceIndexStorage,
    file_type_matcher: &FileTypesMatcher,
) -> ResolvedPlanFilter {
    let file_ids = resolve_filtered_file_ids(plan, &storage.list_files(), file_type_matcher);
    let symbol_types = if plan.plan.symbol_types.is_empty() {
        None
    } else {
        Some(plan.plan.symbol_types.clone())
    };
    match (file_ids, symbol_types) {
        (None, None) => ResolvedPlanFilter {
            filter: None,
            matches_no_files: false,
        },
        (file_ids, symbol_types) => {
            let file_ids = file_ids.unwrap_or_default();
            // A resolved-but-empty file set matches nothing, even when symbol
            // dimensions are present (mirrors `filterMatchesNoFiles`).
            let matches_no_files = file_ids.is_empty() && file_id_constrained(plan);
            ResolvedPlanFilter {
                filter: Some(StorageSearchFilter {
                    file_ids,
                    group_ids: Vec::new(),
                    symbol_names: Vec::new(),
                    symbol_types: symbol_types.unwrap_or_default(),
                }),
                matches_no_files,
            }
        }
    }
}

/// True when the plan constrains the file-id dimension at all, so an empty
/// resolution means "match nothing" rather than "unfiltered".
fn file_id_constrained(plan: &ResolvedSearchPlan) -> bool {
    !plan.plan.include_paths.is_empty()
        || !plan.plan.exclude_paths.is_empty()
        || plan.plan.modified_after.is_some()
        || plan.plan.modified_before.is_some()
        || !plan.plan.globs.is_empty()
        || !plan.plan.insensitive_globs.is_empty()
        || !plan.plan.file_types.is_empty()
        || !plan.plan.excluded_file_types.is_empty()
}

fn resolve_filtered_file_ids(
    plan: &ResolvedSearchPlan,
    files: &[FileInfo],
    file_type_matcher: &FileTypesMatcher,
) -> Option<Vec<crate::ids::FileId>> {
    let include_matchers: Vec<_> =
        plan.plan.include_paths.iter().map(|pattern| compile_path_filter(pattern)).collect();
    let exclude_matchers: Vec<_> =
        plan.plan.exclude_paths.iter().map(|pattern| compile_path_filter(pattern)).collect();
    let has_modified = plan.plan.modified_after.is_some() || plan.plan.modified_before.is_some();
    let has_shared = !plan.plan.globs.is_empty()
        || !plan.plan.insensitive_globs.is_empty()
        || !file_type_matcher.is_empty();
    if include_matchers.is_empty() && exclude_matchers.is_empty() && !has_modified && !has_shared {
        return None;
    }
    let globs = OrderedGlobs::new(&plan.plan.globs, &plan.plan.insensitive_globs);
    Some(
        files
            .iter()
            .filter(|file| {
                let included = include_matchers.is_empty()
                    || include_matchers.iter().any(|matcher| matcher(file));
                let excluded = exclude_matchers.iter().any(|matcher| matcher(file));
                included
                    && !excluded
                    && globs.matches(&file.relative_path)
                    && file_type_matcher.matches(std::path::Path::new(&file.relative_path))
                    && matches_modified_time_filter(file, &plan.plan)
            })
            .map(|file| file.id.clone())
            .collect(),
    )
}

fn matches_modified_time_filter(file: &FileInfo, plan: &SearchPlan) -> bool {
    if plan.modified_after.is_some_and(|after| file.last_modified_time.0 < after.0) {
        return false;
    }
    if plan.modified_before.is_some_and(|before| file.last_modified_time.0 > before.0) {
        return false;
    }
    true
}

fn compile_path_filter(pattern: &str) -> impl Fn(&FileInfo) -> bool + '_ {
    let absolute = is_absolute_path_pattern(pattern);
    move |file: &FileInfo| {
        let path = if absolute {
            normalize_path_for_match(&file.absolute_path)
        } else {
            normalize_path_for_match(&file.relative_path)
        };
        path_pattern_matches(pattern, &path)
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_routes_rejected() {
        assert!(validate_search_plan(&SearchPlan::default()).is_err());
    }

    #[test]
    fn route_ids_number_per_mode() {
        let plan = SearchPlan {
            routes: vec![
                SearchPlanRoute {
                    mode: SearchPlanRouteMode::Fts,
                    query: "a".to_owned(),
                },
                SearchPlanRoute {
                    mode: SearchPlanRouteMode::Vector,
                    query: "b".to_owned(),
                },
                SearchPlanRoute {
                    mode: SearchPlanRouteMode::Fts,
                    query: "c".to_owned(),
                },
            ],
            ..SearchPlan::default()
        };
        let resolved = validate_search_plan(&plan).expect("valid plan");
        let ids: Vec<&str> = resolved.routes.iter().map(|route| route.id.as_str()).collect();
        assert_eq!(ids, vec!["fts", "vector", "fts-2"]);
    }

    #[test]
    fn symbol_names_skip_keywords() {
        assert_eq!(extract_symbol_names("find class Foo"), vec!["Foo".to_owned()]);
        assert_eq!(
            extract_symbol_names("Owner::method"),
            vec!["Owner::method".to_owned()]
        );
    }
}
