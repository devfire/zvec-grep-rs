//! Request normalization: trims queries/routes and expands every primary
//! query into a hybrid FTS+vector group, mirroring TS
//! `normalizeContextRequest`/`normalizePrimaryQueries`/`contextGroups`.

use crate::error::{EngineError, EngineResult, codes};
use crate::service::types::{GroupRole, ZvecGrepContextOptions, empty_query_error};
use crate::types::{SearchPlanRoute, SearchPlanRouteMode};

/// One normalized query group (`Q1`… with its display query, role, and
/// routes), mirroring TS `NormalizedContextGroup`.
#[derive(Debug, Clone)]
pub(in crate::service::facade) struct ContextGroup {
    pub(in crate::service::facade) id: String,
    pub(in crate::service::facade) query: String,
    pub(in crate::service::facade) role: GroupRole,
    pub(in crate::service::facade) routes: Vec<SearchPlanRoute>,
}

/// Normalized context request: the display query plus per-group routes,
/// mirroring TS `NormalizedContextRequest`.
#[derive(Debug, Clone)]
pub(in crate::service::facade) struct NormalizedContextRequest {
    pub(in crate::service::facade) display_query: String,
    pub(in crate::service::facade) groups: Vec<ContextGroup>,
}

/// Normalizes context options into per-group hybrid routes, mirroring TS
/// `normalizeContextRequest`/`normalizePrimaryQueries`/`contextGroups`:
/// every primary query becomes one primary group with an FTS+vector route
/// pair, and every extra route becomes one supplemental group.
///
/// # Errors
///
/// Returns [`empty_query_error`] when no primary query and no extra route is
/// present, or `SERVICE.EMPTY_ROUTE_QUERY` for a blank extra route.
pub(in crate::service::facade) fn normalize_context_request(
    options: &ZvecGrepContextOptions<'_>,
) -> EngineResult<NormalizedContextRequest> {
    let primaries: Vec<String> = options
        .query
        .iter()
        .chain(options.queries.iter())
        .map(|query| query.trim().to_owned())
        .filter(|query| !query.is_empty())
        .collect();
    let extras = normalize_context_routes(options)?;
    if primaries.is_empty() && extras.is_empty() {
        return Err(empty_query_error());
    }
    let display_query = if primaries.is_empty() {
        extras
            .iter()
            .map(|route| route.query.as_str())
            .collect::<Vec<_>>()
            .join(" | ")
    } else {
        primaries.join(" | ")
    };
    let mut groups = Vec::with_capacity(primaries.len() + extras.len());
    for (index, query) in primaries.iter().enumerate() {
        groups.push(ContextGroup {
            id: format!("Q{}", index + 1),
            query: query.clone(),
            role: GroupRole::Primary,
            routes: vec![
                SearchPlanRoute {
                    mode: SearchPlanRouteMode::Fts,
                    query: query.clone(),
                },
                SearchPlanRoute {
                    mode: SearchPlanRouteMode::Vector,
                    query: query.clone(),
                },
            ],
        });
    }
    let offset = groups.len();
    for (index, route) in extras.into_iter().enumerate() {
        groups.push(ContextGroup {
            id: format!("Q{}", offset + index + 1),
            query: route.query.clone(),
            role: GroupRole::Supplemental,
            routes: vec![route],
        });
    }
    Ok(NormalizedContextRequest {
        display_query,
        groups,
    })
}

/// Trims extra routes and rejects blank ones before they consume a group
/// slot, mirroring TS `normalizeContextRoutes`.
///
/// # Errors
///
/// Returns `SERVICE.EMPTY_ROUTE_QUERY` for a blank route query.
fn normalize_context_routes(
    options: &ZvecGrepContextOptions<'_>,
) -> EngineResult<Vec<SearchPlanRoute>> {
    let mut extras: Vec<SearchPlanRoute> = Vec::new();
    extras.extend(options.routes.iter().cloned());
    extras.extend(options.fts.iter().map(|term| SearchPlanRoute {
        mode: SearchPlanRouteMode::Fts,
        query: term.clone(),
    }));
    extras.extend(options.vector.iter().map(|term| SearchPlanRoute {
        mode: SearchPlanRouteMode::Vector,
        query: term.clone(),
    }));
    for (index, route) in extras.iter_mut().enumerate() {
        let query = route.query.trim().to_owned();
        if query.is_empty() {
            let mode = match route.mode {
                SearchPlanRouteMode::Fts => "fts",
                SearchPlanRouteMode::Vector => "vector",
            };
            return Err(EngineError::new(
                codes::service_empty_route_query(),
                "zvec-grep context route requires a non-empty query",
            )
            .with_context(format!("routeIndex={index} mode={mode}")));
        }
        route.query = query;
    }
    Ok(extras)
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::error::EngineErrorCode;

    fn options_with(
        query: Option<&str>,
        queries: &[&str],
        routes: Vec<SearchPlanRoute>,
        fts: &[&str],
    ) -> ZvecGrepContextOptions<'static> {
        ZvecGrepContextOptions {
            query: query.map(str::to_owned),
            queries: queries.iter().map(|query| (*query).to_owned()).collect(),
            routes,
            fts: fts.iter().map(|term| (*term).to_owned()).collect(),
            ..ZvecGrepContextOptions::default()
        }
    }

    fn fts_route(query: &str) -> SearchPlanRoute {
        SearchPlanRoute {
            mode: SearchPlanRouteMode::Fts,
            query: query.to_owned(),
        }
    }

    #[test]
    fn bare_query_expands_to_hybrid_primary_group() {
        let request = normalize_context_request(&options_with(Some("alpha"), &[], Vec::new(), &[]))
            .expect("valid request");
        assert_eq!(request.display_query, "alpha");
        assert_eq!(request.groups.len(), 1);
        let group = &request.groups[0];
        assert_eq!(group.id, "Q1");
        assert_eq!(group.role, GroupRole::Primary);
        assert_eq!(group.routes.len(), 2);
        assert_eq!(group.routes[0].mode, SearchPlanRouteMode::Fts);
        assert_eq!(group.routes[1].mode, SearchPlanRouteMode::Vector);
        assert!(group.routes.iter().all(|route| route.query == "alpha"));
    }

    #[test]
    fn queries_share_display_and_extras_continue_numbering() {
        let request =
            normalize_context_request(&options_with(None, &["a", "b"], vec![fts_route("c")], &[]))
                .expect("valid request");
        assert_eq!(request.display_query, "a | b");
        assert_eq!(request.groups.len(), 3);
        assert_eq!(request.groups[0].id, "Q1");
        assert_eq!(request.groups[1].id, "Q2");
        assert!(
            request.groups[..2]
                .iter()
                .all(|group| group.role == GroupRole::Primary && group.routes.len() == 2)
        );
        let extra = &request.groups[2];
        assert_eq!(extra.id, "Q3");
        assert_eq!(extra.role, GroupRole::Supplemental);
        assert_eq!(extra.routes.len(), 1);
        assert_eq!(extra.routes[0].query, "c");
    }

    #[test]
    fn routes_only_request_is_valid_with_display_fallback() {
        let request = normalize_context_request(&options_with(None, &[], Vec::new(), &["sym"]))
            .expect("routes-only is valid");
        assert_eq!(request.display_query, "sym");
        assert_eq!(request.groups.len(), 1);
        assert_eq!(request.groups[0].role, GroupRole::Supplemental);
    }

    #[test]
    fn empty_request_errors() {
        let error = normalize_context_request(&options_with(None, &[], Vec::new(), &[]))
            .expect_err("empty request errors");
        assert_eq!(*error.code(), EngineErrorCode::ContextEmptyQuery);
    }

    #[test]
    fn blank_queries_are_trimmed_and_dropped() {
        let request =
            normalize_context_request(&options_with(Some("   "), &["", " b "], Vec::new(), &[]))
                .expect("valid request");
        assert_eq!(request.display_query, "b");
        assert_eq!(request.groups.len(), 1);
        assert_eq!(request.groups[0].routes[0].query, "b");
    }

    #[test]
    fn blank_route_errors_before_consuming_a_group_slot() {
        let error =
            normalize_context_request(&options_with(None, &[], vec![fts_route("   ")], &[]))
                .expect_err("blank route errors");
        assert_eq!(*error.code(), codes::service_empty_route_query());
    }

    #[test]
    fn route_queries_are_trimmed() {
        let request =
            normalize_context_request(&options_with(None, &[], vec![fts_route("  padded  ")], &[]))
                .expect("valid request");
        assert_eq!(request.groups[0].query, "padded");
        assert_eq!(request.groups[0].routes[0].query, "padded");
    }
}
