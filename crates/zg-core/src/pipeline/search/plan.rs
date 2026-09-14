//! Search-plan validation and normalization.

use std::collections::{HashMap, HashSet};

use crate::error::{
    DetailEntry, DetailValue, EngineError, EngineErrorCode, EngineResult, error_details,
    workspace_index_detail,
};
use crate::models::EmbeddingModel;
use crate::types::{
    ResolvedSearchPlan, ResolvedSearchPlanRoute, SearchPlan, SearchPlanRouteMode, UnixMillis,
};
use crate::utils::glob::normalize_path_pattern;

use super::SearchContext;

pub(crate) fn validate_search_plan(plan: &SearchPlan) -> EngineResult<ResolvedSearchPlan> {
    if plan.routes.is_empty() {
        return Err(EngineError::new(
            EngineErrorCode::SearchPlanEmptyRoutes,
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
                EngineErrorCode::SearchPlanEmptyRouteQuery,
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
    if let (Some(after), Some(before)) = (modified_after, modified_before)
        && after.as_millis() > before.as_millis()
    {
        return Err(EngineError::new(
            EngineErrorCode::SearchPlanInvalidModifiedTimeRange,
            "search plan modified-after filter must not be later than modified-before",
        )
        .with_context(format!(
            "modifiedAfter={} modifiedBefore={}",
            after.as_millis(),
            before.as_millis()
        )));
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
            insensitive_globs: normalize_string_filters(
                &plan.insensitive_globs,
                "insensitiveGlobs",
            )?,
            file_types: normalize_string_filters(&plan.file_types, "fileTypes")?,
            excluded_file_types: normalize_string_filters(
                &plan.excluded_file_types,
                "excludedFileTypes",
            )?,
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

pub(crate) fn plan_uses_vector(plan: &ResolvedSearchPlan) -> bool {
    plan.routes
        .iter()
        .any(|route| route.mode == SearchPlanRouteMode::Vector)
}

pub(crate) fn require_embedding_model<'a>(
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
            EngineErrorCode::SearchEmbeddingModelRequired,
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
                EngineErrorCode::SearchPlanInvalidPathFilter,
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
                EngineErrorCode::SearchPlanInvalidFilter,
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
        Some(time) if time.as_millis() >= 0 => Ok(Some(time)),
        Some(time) => Err(EngineError::new(
            EngineErrorCode::SearchPlanInvalidModifiedTimeFilter,
            "search plan modified time filters must be non-negative epoch milliseconds",
        )
        .with_context(format!("field={field} value={}", time.as_millis()))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::SearchPlanRoute;

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
        let ids: Vec<&str> = resolved
            .routes
            .iter()
            .map(|route| route.id.as_str())
            .collect();
        assert_eq!(ids, vec!["fts", "vector", "fts-2"]);
    }
}
