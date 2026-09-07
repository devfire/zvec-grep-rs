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
    let mut fts = Fts::new().map_err(|error| query_error("fts payload", &error.to_string()))?;
    fts.set_match_string(query)
        .map_err(|error| query_error("fts match string", &error.to_string()))?;
    let mut built = SearchQuery::fts(ENTITY_TEXT_FIELD, &fts, topk)
        .map_err(|error| query_error("fts query", &error.to_string()))?;
    apply_query_options(&mut built, filter, "fts")?;
    collection
        .query(&built)
        .map_err(|error| query_error("fts query", &error.to_string()))
}

/// Dense recall over the embedding vector field.
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
        .map_err(|error| query_error("vector query", &error.to_string()))?;
    apply_query_options(&mut built, filter, "vector")?;
    collection
        .query(&built)
        .map_err(|error| query_error("vector query", &error.to_string()))
}

fn apply_query_options(
    query: &mut SearchQuery,
    filter: Option<&StorageSearchFilter>,
    kind: &str,
) -> EngineResult<()> {
    if let Some(expression) = build_filter(filter) {
        query
            .set_filter(&expression)
            .map_err(|error| query_error(kind, &error.to_string()))?;
    }
    query
        .set_include_vector(false)
        .map_err(|error| query_error(kind, &error.to_string()))
}

fn clamp_topk(limit: usize) -> i32 {
    if limit == 0 {
        return 0;
    }
    limit.min(ZVEC_MAX_QUERY_TOPK) as i32
}

fn query_error(kind: &str, detail: &str) -> EngineError {
    EngineError::new(
        EngineErrorCode::from_static("STORAGE.ZVEC_QUERY_FAILED"),
        "zvec recall query failed",
    )
    .with_context(format!("kind={kind} error={detail}"))
}
