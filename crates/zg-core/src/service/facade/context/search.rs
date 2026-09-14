//! Hybrid `context()` execution: one [`SearchPlan`](crate::types::SearchPlan)
//! per query group, then [`select_and_rank`](super::rank::select_and_rank).
//! `fuse` collapses every group into a single `Q1` plan.

use std::path::PathBuf;

use super::super::service::ZvecGrepService;
use super::normalize::{ContextGroup, NormalizedContextRequest, normalize_context_request};
use super::rank::{context_group_limit, select_and_rank};
use crate::error::EngineResult;
use crate::service::root::WorkspaceIndexLocation;
use crate::service::types::{
    ContentStatus, ContextCoverage, ContextDiagnostics, ContextFile, ContextItem, ContextItemKind,
    ContextSource, GroupResult, GroupRole, QueryGroupRef, ZvecGrepContextOptions,
    ZvecGrepContextResult, ZvecGrepIndexOptions,
};
use crate::service::workspace_index::{IndexMode, WorkspaceIndex, WorkspaceIndexOptions};
use crate::types::{SearchHit, SearchPlan, TimingEntry, WorkspaceIndexInfo};

impl ZvecGrepService {
    /// Hybrid search over the workspace index, mirroring `context()`.
    ///
    /// With `auto_update` set, a stale index is refreshed first via
    /// [`ensure_index`](Self::ensure_index); read sessions (phase G) always
    /// pass it cleared.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty query, when no built index is found, or when model
    /// resolution, index open, or search fails.
    pub fn context(
        &self,
        options: &ZvecGrepContextOptions<'_>,
    ) -> EngineResult<ZvecGrepContextResult> {
        let request = normalize_context_request(options)?;
        let location = self.require_indexed_location(options.root)?;
        let manifest = self.require_manifest(&location)?;
        if options.wants_auto_update() {
            self.refresh(&location)?;
            return self.context_after_refresh(options, &request);
        }
        let model = self.model_for_manifest(Some(&manifest))?;
        let info = manifest.info.clone();
        let index = WorkspaceIndex::open(
            info.clone(),
            WorkspaceIndexOptions {
                mode: IndexMode::Read,
                embedding_model: Some(model),
            },
        )?;
        run_context_search(&index, &location.root, &info, &request, options)
    }

    fn refresh(&self, location: &WorkspaceIndexLocation) -> EngineResult<()> {
        let root = PathBuf::from(&location.root);
        self.ensure_index(&ZvecGrepIndexOptions {
            root: Some(&root),
            ..ZvecGrepIndexOptions::default()
        })?;
        Ok(())
    }

    fn context_after_refresh(
        &self,
        options: &ZvecGrepContextOptions<'_>,
        request: &NormalizedContextRequest,
    ) -> EngineResult<ZvecGrepContextResult> {
        let location = self.require_indexed_location(options.root)?;
        let manifest = self.require_manifest(&location)?;
        let model = self.model_for_manifest(Some(&manifest))?;
        let info = manifest.info.clone();
        let index = WorkspaceIndex::open(
            info.clone(),
            WorkspaceIndexOptions {
                mode: IndexMode::Read,
                embedding_model: Some(model),
            },
        )?;
        run_context_search(&index, &location.root, &info, request, options)
    }
}

/// Executes one [`SearchPlan`] per query group and merges the results,
/// mirroring TS `contextFromOpenWorkspaceIndex`: `fuse` collapses every
/// group into a single `Q1` plan, otherwise each group searches with its own
/// per-group limit and the items merge through [`select_and_rank`].
///
/// # Errors
///
/// Returns an error when any per-group search fails.
pub(in crate::service::facade) fn run_context_search(
    index: &WorkspaceIndex,
    root: &str,
    info: &WorkspaceIndexInfo,
    request: &NormalizedContextRequest,
    options: &ZvecGrepContextOptions<'_>,
) -> EngineResult<ZvecGrepContextResult> {
    let groups: Vec<ContextGroup> = if options.fuse {
        vec![ContextGroup {
            id: "Q1".to_owned(),
            query: request.display_query.clone(),
            role: if request
                .groups
                .iter()
                .any(|group| group.role == GroupRole::Primary)
            {
                GroupRole::Primary
            } else {
                GroupRole::Supplemental
            },
            routes: request
                .groups
                .iter()
                .flat_map(|group| group.routes.iter().cloned())
                .collect(),
        }]
    } else {
        request.groups.clone()
    };
    let limit = context_group_limit(options.limit, groups.len());
    let mut searches = Vec::with_capacity(groups.len());
    for group in &groups {
        searches.push(index.search_plan(&group_search_plan(group, options, limit))?);
    }
    let group_items: Vec<Vec<ContextItem>> = searches
        .iter()
        .zip(groups.iter())
        .map(|(search, group)| {
            search
                .hits
                .iter()
                .map(|hit| hit_to_item(hit, group, options.trace))
                .collect()
        })
        .collect();
    let coverage_ids: Vec<String> = groups
        .iter()
        .filter(|group| group.role == GroupRole::Primary)
        .map(|group| group.id.clone())
        .collect();
    let items = select_and_rank(group_items.concat(), &coverage_ids);
    let group_results: Vec<GroupResult> = groups
        .into_iter()
        .zip(group_items)
        .map(|(group, items)| GroupResult {
            id: group.id,
            query: group.query,
            role: Some(group.role),
            items,
            timings: None,
        })
        .collect();
    let routes: Vec<serde_json::Value> = searches
        .iter()
        .flat_map(|search| search.plan.routes.iter())
        .filter_map(|route| serde_json::to_value(route).ok())
        .collect();
    let mut timings: Vec<TimingEntry> = Vec::new();
    for search in &searches {
        timings.extend(search.timings.iter().flatten().cloned());
    }
    let query_groups: Vec<serde_json::Value> = group_results
        .iter()
        .map(|group| {
            serde_json::json!({
                "id": group.id,
                "query": group.query,
                "role": group.role,
            })
        })
        .collect();
    let item_count = items.len();
    Ok(ZvecGrepContextResult {
        query: request.display_query.clone(),
        root: root.to_owned(),
        source: ContextSource::Index,
        coverage: ContextCoverage::RankedSample,
        workspace_index: Some(info.clone()),
        items,
        group_results: Some(group_results),
        diagnostics: ContextDiagnostics {
            empty_reason: if item_count == 0 {
                Some("no_matches".to_owned())
            } else {
                None
            },
            index: Some(serde_json::json!({
                "hitsReturned": item_count,
                "queryGroups": query_groups,
                "routes": routes,
            })),
            rg: None,
            structure: None,
            timings: if timings.is_empty() {
                None
            } else {
                serde_json::to_value(&timings).ok()
            },
        },
    })
}

/// Builds the per-group [`SearchPlan`]: the group's routes with the shared
/// per-group limit and filters. `track_entity_id` is deliberately not
/// forwarded — the TS context `searchPlan` call omits it (see
/// `docs/ts-divergence.md`).
fn group_search_plan(
    group: &ContextGroup,
    options: &ZvecGrepContextOptions<'_>,
    limit: usize,
) -> SearchPlan {
    SearchPlan {
        routes: group.routes.clone(),
        limit: Some(limit),
        trace: Some(options.trace),
        track_entity_id: None,
        prefer_symbol: Some(options.prefer_symbol),
        symbol_types: options.symbol_types.clone(),
        include_paths: options.include_paths.clone(),
        exclude_paths: options.exclude_paths.clone(),
        globs: options.globs.clone(),
        insensitive_globs: options.insensitive_globs.clone(),
        file_types: options.file_types.clone(),
        excluded_file_types: options.excluded_file_types.clone(),
        modified_after: options.modified_after,
        modified_before: options.modified_before,
    }
}

/// Converts one hit into an item tagged with its query group, mirroring TS
/// `searchPlanToContextItems`.
fn hit_to_item(hit: &SearchHit, group: &ContextGroup, trace: bool) -> ContextItem {
    ContextItem {
        kind: ContextItemKind::IndexedEntity,
        rank: hit.rank,
        file: ContextFile {
            absolute_path: hit.file.absolute_path.clone(),
            relative_path: hit.file.relative_path.clone(),
            root_path: hit.file.root_path.clone(),
        },
        range: hit
            .evidence
            .first()
            .map(|evidence| evidence.range.clone())
            .unwrap_or_else(|| hit.entity.range.clone()),
        excerpt_range: None,
        content: hit.entity.content.clone(),
        content_role: None,
        outline: None,
        status: ContentStatus::Fresh,
        score: Some(hit.score),
        matched_by: Some(hit.matched_by.as_str().to_owned()),
        metadata: hit.entity.metadata.clone(),
        entity_id: Some(hit.entity.id.clone()),
        trace: trace
            .then(|| hit.trace.clone())
            .flatten()
            .and_then(|trace| serde_json::to_value(trace).ok()),
        query_groups: vec![QueryGroupRef {
            id: group.id.clone(),
            query: group.query.clone(),
            role: group.role,
            rank: hit.rank,
            matched_by: hit.matched_by,
        }],
        container: None,
        selection_reason: None,
        coverage_group: None,
    }
}
