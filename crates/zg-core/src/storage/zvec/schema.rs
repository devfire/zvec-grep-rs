//! Entity collection schema: field names, index types, vector metric.
//!
//! Ports `createSchema`, `metricToZvec`, and the field helpers from
//! `engine/storage/zvec.ts` onto the zvec-rust 0.7 builder API.

use zvec_rust::{CollectionSchema, DataType, FieldSchema, IndexParams, MetricType};

use crate::error::{EngineError, EngineErrorCode, EngineResult};
use crate::types::{SearchMetric, WorkspaceIndexEmbeddingSchema};

/// Vector field holding fragment embeddings.
pub const ENTITY_VECTOR_FIELD: &str = "embedding";
/// Full-text indexed field holding fragment text.
pub const ENTITY_TEXT_FIELD: &str = "text";

/// Builds the `zvec_grep_entities` collection schema for `embedding`.
///
/// # Errors
///
/// Returns `STORAGE.INVALID_EMBEDDING_DIMENSION` when the dimension does not fit a `u32`, or
/// `STORAGE.SCHEMA_FAILED` when a schema field cannot be built.
pub fn create_entities_schema(
    embedding: &WorkspaceIndexEmbeddingSchema,
) -> EngineResult<CollectionSchema> {
    let dimension: u32 = embedding.dimension.try_into().map_err(|error| {
        EngineError::new(
            EngineErrorCode::StorageInvalidEmbeddingDimension,
            "embedding dimension does not fit a u32",
        )
        .with_context(format!("dimension={}", embedding.dimension))
        .with_source(error)
    })?;
    let mut schema = CollectionSchema::new("zvec_grep_entities")
        .map_err(|error| schema_error("zvec_grep_entities", error))?;
    indexed_string_field(&mut schema, "group", true)?;
    indexed_string_field(&mut schema, "file_id", false)?;
    plain_string_field(&mut schema, "content_kind", false)?;
    plain_string_field(&mut schema, "content_hash", true)?;
    plain_string_field(&mut schema, "metadata_kind", true)?;
    indexed_string_field(&mut schema, "symbol_type", true)?;
    indexed_string_field(&mut schema, "symbol_name", true)?;
    plain_string_field(&mut schema, "symbol_scope", true)?;
    plain_string_field(&mut schema, "symbol_signature", true)?;
    plain_string_field(&mut schema, "symbol_doc", true)?;
    plain_string_field(&mut schema, "symbol_modifiers", true)?;
    plain_string_field(&mut schema, "node_type", true)?;
    plain_string_field(&mut schema, "heading", true)?;
    int_field(&mut schema, "heading_level", true)?;
    fts_text_field(&mut schema, ENTITY_TEXT_FIELD)?;
    int_field(&mut schema, "fragment_index", false)?;
    plain_string_field(&mut schema, "range_json", false)?;
    plain_string_field(&mut schema, "content_base64", true)?;
    plain_string_field(&mut schema, "image_format", true)?;
    vector_field(
        &mut schema,
        ENTITY_VECTOR_FIELD,
        dimension,
        metric_to_zvec(embedding.metric)?,
    )?;
    Ok(schema)
}

/// Maps a workspace search metric onto the zvec metric type.
///
/// # Errors
///
/// Never returns `Err`; the `Result` reserves failure for future [`SearchMetric`] variants.
pub fn metric_to_zvec(metric: SearchMetric) -> EngineResult<MetricType> {
    match metric {
        SearchMetric::Cosine => Ok(MetricType::Cosine),
        SearchMetric::Dot => Ok(MetricType::Ip),
        SearchMetric::Euclidean => Ok(MetricType::L2),
    }
}

fn indexed_string_field(
    schema: &mut CollectionSchema,
    name: &str,
    nullable: bool,
) -> EngineResult<()> {
    let mut field = FieldSchema::new(name, DataType::String, nullable, 0)
        .map_err(|error| schema_error(name, error))?;
    let params = IndexParams::invert(false, false)
        .map_err(|error| schema_error(name, error))?;
    field
        .set_index_params(&params)
        .map_err(|error| schema_error(name, error))?;
    schema
        .add_field(&field)
        .map_err(|error| schema_error(name, error))
}

fn plain_string_field(
    schema: &mut CollectionSchema,
    name: &str,
    nullable: bool,
) -> EngineResult<()> {
    let field = FieldSchema::new(name, DataType::String, nullable, 0)
        .map_err(|error| schema_error(name, error))?;
    schema
        .add_field(&field)
        .map_err(|error| schema_error(name, error))
}

fn int_field(schema: &mut CollectionSchema, name: &str, nullable: bool) -> EngineResult<()> {
    let field = FieldSchema::new(name, DataType::Int32, nullable, 0)
        .map_err(|error| schema_error(name, error))?;
    schema
        .add_field(&field)
        .map_err(|error| schema_error(name, error))
}

fn fts_text_field(schema: &mut CollectionSchema, name: &str) -> EngineResult<()> {
    let mut field = FieldSchema::new(name, DataType::String, false, 0)
        .map_err(|error| schema_error(name, error))?;
    // Standalone choice, not a compat shim: the TS generation uses the
    // `jieba` tokenizer, but the two builds never share collections, so
    // this port uses `standard` and ships no dictionary (see
    // docs/ts-divergence.md).
    let params = IndexParams::fts(Some("standard"), Some(&["lowercase"]), None)
        .map_err(|error| schema_error(name, error))?;
    field
        .set_index_params(&params)
        .map_err(|error| schema_error(name, error))?;
    schema
        .add_field(&field)
        .map_err(|error| schema_error(name, error))
}

fn vector_field(
    schema: &mut CollectionSchema,
    name: &str,
    dimension: u32,
    metric: MetricType,
) -> EngineResult<()> {
    let mut field = FieldSchema::new(name, DataType::VectorFp32, false, dimension)
        .map_err(|error| schema_error(name, error))?;
    let params = IndexParams::hnsw(metric, 16, 200)
        .map_err(|error| schema_error(name, error))?;
    field
        .set_index_params(&params)
        .map_err(|error| schema_error(name, error))?;
    schema
        .add_field(&field)
        .map_err(|error| schema_error(name, error))
}

/// Schema-build failure with the typed zvec cause attached (see
/// `doc_field_error` for the `with_context` / `with_source` split).
fn schema_error(field: &str, error: zvec_rust::Error) -> EngineError {
    EngineError::new(
        EngineErrorCode::StorageSchemaFailed,
        "failed to build entity collection schema",
    )
    .with_context(format!("field={field}"))
    .with_source(error)
}
