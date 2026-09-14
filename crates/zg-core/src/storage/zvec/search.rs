//! Filtered FTS and vector recall over the entity collection.
//!
//! Ports `searchFts` / `searchVector` from `engine/storage/zvec.ts` onto
//! the zvec-rust 0.7 query API (`SearchQuery::fts` for keyword-only
//! retrieval, `SearchQuery::new` for dense recall).

use zvec_rust::{Collection, Doc, Fts, SearchQuery};

use crate::error::{EngineError, EngineErrorCode, EngineResult};
use crate::storage::StorageSearchFilter;

use super::filter::build_filter;
use super::schema::{ENTITY_TEXT_FIELD, ENTITY_VECTOR_FIELD};

/// Upper bound mirrored from the TypeScript implementation.
pub const ZVEC_MAX_QUERY_TOPK: usize = 100_000;

/// Keyword-only recall over the FTS-indexed text field.
///
/// # Errors
///
/// Returns `STORAGE.ZVEC_QUERY_FAILED` when the query payload, filter, or collection query fails.
pub fn search_fts(
    collection: &Collection,
    query: &str,
    limit: usize,
    filter: Option<&StorageSearchFilter>,
) -> EngineResult<Vec<Doc>> {
    let topk = clamp_topk(limit);
    if topk == 0 {
        return Ok(Vec::new());
    }
    let mut fts = Fts::new().map_err(|error| query_error("fts payload", error))?;
    fts.set_match_string(query)
        .map_err(|error| query_error("fts match string", error))?;
    let mut built = SearchQuery::fts(ENTITY_TEXT_FIELD, &fts, topk)
        .map_err(|error| query_error("fts query", error))?;
    apply_query_options(&mut built, filter, "fts")?;
    collection
        .query(&built)
        .map_err(|error| query_error("fts query", error))
}

/// Dense recall over the embedding vector field.
///
/// # Errors
///
/// Returns `STORAGE.ZVEC_QUERY_FAILED` when the query payload, filter, or collection query fails.
pub fn search_vector(
    collection: &Collection,
    vector: &[f32],
    limit: usize,
    filter: Option<&StorageSearchFilter>,
) -> EngineResult<Vec<Doc>> {
    let topk = clamp_topk(limit);
    if topk == 0 {
        return Ok(Vec::new());
    }
    let mut built = SearchQuery::new(ENTITY_VECTOR_FIELD, vector, topk)
        .map_err(|error| query_error("vector query", error))?;
    apply_query_options(&mut built, filter, "vector")?;
    collection
        .query(&built)
        .map_err(|error| query_error("vector query", error))
}

fn apply_query_options(
    query: &mut SearchQuery,
    filter: Option<&StorageSearchFilter>,
    kind: &str,
) -> EngineResult<()> {
    if let Some(expression) = build_filter(filter) {
        query
            .set_filter(&expression)
            .map_err(|error| query_error(kind, error))?;
    }
    query
        .set_include_vector(false)
        .map_err(|error| query_error(kind, error))
}

fn clamp_topk(limit: usize) -> i32 {
    if limit == 0 {
        return 0;
    }
    limit.min(ZVEC_MAX_QUERY_TOPK) as i32
}

/// Recall failure with the typed zvec cause attached (see `doc_field_error`
/// for the `with_context` / `with_source` split).
fn query_error(kind: &str, error: zvec_rust::Error) -> EngineError {
    EngineError::new(
        EngineErrorCode::StorageZvecQueryFailed,
        "zvec recall query failed",
    )
    .with_context(format!("kind={kind}"))
    .with_source(error)
}
