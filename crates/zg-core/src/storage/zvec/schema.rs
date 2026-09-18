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
/// FTS tokenizer this build bakes into `index.zvec` (the TypeScript
/// generation uses `jieba`; see `docs/ts-divergence.md`). Recorded in the
/// [`EmbeddingFingerprint`] so a foreign collection is refused, not adopted.
pub const ENTITY_FTS_TOKENIZER: &str = "standard";
/// Sidecar file beside `index.zvec` carrying the [`EmbeddingFingerprint`]
/// the collection was created with.
pub const EMBEDDING_FINGERPRINT_FILE: &str = "embedding.json";

/// Provenance fingerprint of one `index.zvec` collection: embedding identity
/// plus the FTS tokenizer. Compared at open (see [`fingerprint_mismatch`])
/// so a foreign collection (wrong workspace copy, TS-generation `jieba`
/// index, or re-embedded dimension) is refused instead of silently adopted.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmbeddingFingerprint {
    pub provider: String,
    pub model: String,
    pub dimension: usize,
    pub metric: SearchMetric,
    pub fts_tokenizer: String,
}

/// Fingerprint the given embedding schema stamps into a fresh collection.
#[must_use]
pub fn embedding_fingerprint(embedding: &WorkspaceIndexEmbeddingSchema) -> EmbeddingFingerprint {
    EmbeddingFingerprint {
        provider: embedding.provider.clone(),
        model: embedding.model.clone(),
        dimension: embedding.dimension,
        metric: embedding.metric,
        fts_tokenizer: ENTITY_FTS_TOKENIZER.to_owned(),
    }
}
/// Mismatch between the stored fingerprint and the expected embedding, in
/// `validate_embedding_schema` field order with the tokenizer last: `None`
/// when the collection may be adopted.
#[must_use]
pub fn fingerprint_mismatch(
    stored: &EmbeddingFingerprint,
    expected: &WorkspaceIndexEmbeddingSchema,
) -> Option<EngineError> {
    let mismatch = |code: EngineErrorCode, message: &str, actual: &str| {
        EngineError::new(code, message).with_context(format!(
            "expected={} actual={actual}",
            fingerprint_text(expected)
        ))
    };
    if stored.provider != expected.provider {
        return Some(mismatch(
            EngineErrorCode::WorkspaceIndexEmbeddingProviderMismatch,
            "workspace index embedding provider does not match stored index",
            &stored.provider,
        ));
    }
    if stored.model != expected.model {
        return Some(mismatch(
            EngineErrorCode::WorkspaceIndexEmbeddingModelMismatch,
            "workspace index embedding model does not match stored index",
            &stored.model,
        ));
    }
    if stored.dimension != expected.dimension {
        return Some(mismatch(
            EngineErrorCode::WorkspaceIndexEmbeddingDimensionMismatch,
            "workspace index embedding dimension does not match stored index",
            &stored.dimension.to_string(),
        ));
    }
    if stored.metric != expected.metric {
        return Some(mismatch(
            EngineErrorCode::WorkspaceIndexEmbeddingMetricMismatch,
            "workspace index embedding metric does not match stored index",
            &format!("{:?}", stored.metric),
        ));
    }
    if stored.fts_tokenizer != ENTITY_FTS_TOKENIZER {
        return Some(
            EngineError::new(
                EngineErrorCode::StorageForeignTsIndexPresent,
                "storage directory holds a foreign index.zvec",
            )
            .with_context(format!(
                "path=index.zvec ftsTokenizer={} hint=this build uses the standard tokenizer and never adopts foreign collections; rebuild the index in a separate storage directory",
                stored.fts_tokenizer
            )),
        );
    }
    None
}

fn fingerprint_text(embedding: &WorkspaceIndexEmbeddingSchema) -> String {
    format!(
        "{}:{}:{}:{:?}",
        embedding.provider, embedding.model, embedding.dimension, embedding.metric
    )
}

/// Writes the fingerprint sidecar for a freshly created collection.
///
/// # Errors
///
/// Returns `JSON.WRITE_FAILED` when the sidecar cannot be persisted.
pub fn write_embedding_fingerprint(
    dir: &std::path::Path,
    embedding: &WorkspaceIndexEmbeddingSchema,
) -> EngineResult<()> {
    crate::utils::json_io::write_json_file(
        &dir.join(EMBEDDING_FINGERPRINT_FILE),
        &embedding_fingerprint(embedding),
        crate::utils::json_io::DEFAULT_MODES,
    )
}

/// Refuses a foreign `index.zvec`: a missing sidecar or any fingerprint
/// mismatch is an error, never a silent adopt.
///
/// # Errors
///
/// Returns `STORAGE.FOREIGN_TS_INDEX_PRESENT` when no sidecar exists (a
/// collection this build did not stamp) or the tokenizer disagrees, or the
/// `WORKSPACE_INDEX.EMBEDDING_*_MISMATCH` code naming the first disagreeing
/// field otherwise.
pub fn verify_embedding_fingerprint(
    dir: &std::path::Path,
    expected: &WorkspaceIndexEmbeddingSchema,
) -> EngineResult<()> {
    let stored: Option<EmbeddingFingerprint> =
        crate::utils::json_io::read_json_file(&dir.join(EMBEDDING_FINGERPRINT_FILE), None)?;
    let Some(stored) = stored else {
        return Err(EngineError::new(
            EngineErrorCode::StorageForeignTsIndexPresent,
            "storage directory holds an index.zvec from an unknown build",
        )
        .with_context(
            "path=index.zvec hint=this build stamps every collection it creates; rebuild the index to restamp it",
        ));
    };
    if let Some(error) = fingerprint_mismatch(&stored, expected) {
        return Err(error);
    }
    Ok(())
}

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
    let params = IndexParams::invert(false, false).map_err(|error| schema_error(name, error))?;
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
    let params = IndexParams::hnsw(metric, 16, 200).map_err(|error| schema_error(name, error))?;
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

#[cfg(test)]
mod fingerprint_tests {
    use super::*;

    fn expected() -> WorkspaceIndexEmbeddingSchema {
        WorkspaceIndexEmbeddingSchema {
            provider: "test".to_owned(),
            model: "dummy".to_owned(),
            dimension: 4,
            metric: SearchMetric::Cosine,
        }
    }

    #[test]
    fn foreign_tokenizer_and_dim_mismatch_are_refused() {
        let expected = expected();
        let stored = embedding_fingerprint(&expected);
        assert!(fingerprint_mismatch(&stored, &expected).is_none());
        let jieba = EmbeddingFingerprint {
            fts_tokenizer: "jieba".to_owned(),
            ..stored.clone()
        };
        assert_eq!(
            *fingerprint_mismatch(&jieba, &expected)
                .expect("jieba tokenizer must mismatch")
                .code(),
            EngineErrorCode::StorageForeignTsIndexPresent
        );
        let dim = EmbeddingFingerprint {
            dimension: stored.dimension + 1,
            ..stored
        };
        assert_eq!(
            *fingerprint_mismatch(&dim, &expected)
                .expect("dimension must mismatch")
                .code(),
            EngineErrorCode::WorkspaceIndexEmbeddingDimensionMismatch
        );
    }
}
