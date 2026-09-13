//! Embedding request/response types and the shared validation every backend
//! must run, mirroring `src/engine/models/embeddings.ts` (`BaseEmbeddingModel`)
//! and `src/engine/models/ranking.ts`.
//!
//! The [`EmbeddingModel`] trait itself lives in
//! [`super`]; this module supplies its inputs (`CreateEmbeddingModelOptions`,
//! `EmbeddingResult`), the validation helpers backends call at the top of
//! `embed`, and the ranking surface.

use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::error::ModelError;
use super::{EmbeddingInput, EmbeddingModel, EmbeddingPurpose, ModelLoadSink};
use crate::error::EngineResult;
use crate::types::Content;

/// Which device a local backend should prefer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeviceKind {
    #[default]
    Auto,
    Cpu,
    Metal,
    Vulkan,
    Cuda,
}

impl DeviceKind {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Cpu => "cpu",
            Self::Metal => "metal",
            Self::Vulkan => "vulkan",
            Self::Cuda => "cuda",
        }
    }
}

/// Options accepted by every embedding model constructor.
///
/// Remote backends read `api_key`/`endpoint`; local backends read
/// `model_cache_dir`/`device`. Unused fields are ignored.
#[derive(Debug, Clone, Default)]
pub struct CreateEmbeddingModelOptions {
    pub api_key: Option<String>,
    pub endpoint: Option<String>,
    pub model_cache_dir: Option<PathBuf>,
    pub device: DeviceKind,
}

/// Per-request embedding options.
#[derive(Clone, Default)]
pub struct EmbeddingOptions {
    pub purpose: EmbeddingPurpose,
    pub on_progress: Option<ModelLoadSink>,
}

impl fmt::Debug for EmbeddingOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EmbeddingOptions")
            .field("purpose", &self.purpose)
            .field("on_progress", &self.on_progress.is_some())
            .finish()
    }
}

// `Default for EmbeddingPurpose` is derived on the enum in `super`.

/// Normalized request options passed to a backend's embed core.
#[derive(Clone, Default)]
pub struct NormalizedEmbeddingOptions {
    pub purpose: EmbeddingPurpose,
    pub on_progress: Option<ModelLoadSink>,
}

impl fmt::Debug for NormalizedEmbeddingOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NormalizedEmbeddingOptions")
            .field("purpose", &self.purpose)
            .field("on_progress", &self.on_progress.is_some())
            .finish()
    }
}

/// Normalizes request options, defaulting the purpose to documents.
#[must_use]
pub fn normalize_embedding_options(options: &EmbeddingOptions) -> NormalizedEmbeddingOptions {
    NormalizedEmbeddingOptions {
        purpose: options.purpose,
        on_progress: options.on_progress.clone(),
    }
}

/// Vectors produced for one embed call, in input order.
#[derive(Debug, Clone, PartialEq)]
pub struct EmbeddingResult {
    pub vectors: Vec<Vec<f32>>,
    /// Indices (into the input slice) whose text was truncated to the
    /// model's token limit.
    pub truncated: Vec<usize>,
}

/// API key with redacted debug output so keys never leak into logs.
#[derive(Clone)]
pub struct ApiKey(String);

impl ApiKey {
    pub fn new(key: impl Into<String>) -> Self {
        Self(key.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ApiKey([redacted])")
    }
}

/// Selects the instruction prefix for `purpose`, if the model defines one.
#[must_use]
pub fn purpose_prefix<'a>(
    query_prefix: Option<&'a str>,
    document_prefix: Option<&'a str>,
    purpose: EmbeddingPurpose,
) -> Option<&'a str> {
    match purpose {
        EmbeddingPurpose::Query => query_prefix,
        EmbeddingPurpose::Document => document_prefix,
    }
}

/// Validates batch inputs exactly like `BaseEmbeddingModel.validateContents`:
/// non-empty batch, within the model's batch limit, kinds the model accepts,
/// and non-empty text / image payloads.
///
/// # Errors
///
/// Returns [`ModelError::EmptyInput`] when the batch is empty, [`ModelError::BatchTooLarge`] when
/// it exceeds the model limit, or [`ModelError::EmptyText`], [`ModelError::UnsupportedImage`], or
/// [`ModelError::EmptyImage`] when an entry is invalid.
pub fn validate_contents(
    model: &dyn EmbeddingModel,
    inputs: &[EmbeddingInput<'_>],
) -> EngineResult<()> {
    let info = model.info();
    let reference = info.reference.clone();
    if inputs.is_empty() {
        return Err(ModelError::EmptyInput { reference }.into());
    }
    let max = model.max_batch_size();
    if inputs.len() > max {
        return Err(ModelError::BatchTooLarge {
            reference,
            size: inputs.len(),
            max,
        }
        .into());
    }
    for (index, input) in inputs.iter().enumerate() {
        match input {
            EmbeddingInput::Text { text } => {
                if text.trim().is_empty() {
                    return Err(ModelError::EmptyText {
                        reference: reference.clone(),
                        index,
                    }
                    .into());
                }
            }
            EmbeddingInput::Image { data, .. } => {
                if !info.supports_images {
                    return Err(ModelError::UnsupportedImage {
                        reference: reference.clone(),
                        index: Some(index),
                    }
                    .into());
                }
                if data.is_empty() {
                    return Err(ModelError::EmptyImage {
                        reference: reference.clone(),
                        index,
                    }
                    .into());
                }
            }
        }
    }
    Ok(())
}

/// Validates a backend's raw output exactly like
/// `BaseEmbeddingModel.validateResult`: one finite vector of the model's
/// dimension per input, plus in-range unique truncation indices.
///
/// # Errors
///
/// Returns [`ModelError::VectorCountMismatch`] when the vector count differs from the input count,
/// [`ModelError::DimensionMismatch`] or [`ModelError::NonFiniteValue`] for malformed vectors, or
/// [`ModelError::InvalidTruncatedIndex`] for out-of-range or duplicate truncation indices.
pub fn validate_result(
    model: &dyn EmbeddingModel,
    input_count: usize,
    result: &EmbeddingResult,
) -> EngineResult<()> {
    let info = model.info();
    let reference = info.reference.clone();
    if result.vectors.len() != input_count {
        return Err(ModelError::VectorCountMismatch {
            reference,
            expected: input_count,
            actual: result.vectors.len(),
        }
        .into());
    }
    for (vector_index, vector) in result.vectors.iter().enumerate() {
        if vector.len() != info.dimension {
            return Err(ModelError::DimensionMismatch {
                reference: reference.clone(),
                vector_index,
                expected: info.dimension,
                actual: vector.len(),
            }
            .into());
        }
        for (value_index, value) in vector.iter().enumerate() {
            if !value.is_finite() {
                return Err(ModelError::NonFiniteValue {
                    reference: reference.clone(),
                    vector_index,
                    value_index,
                }
                .into());
            }
        }
    }
    let mut seen = vec![false; input_count];
    for index in &result.truncated {
        if *index >= input_count || seen.get(*index).is_some_and(|flag| *flag) {
            return Err(ModelError::InvalidTruncatedIndex {
                reference: reference.clone(),
                index: *index,
                input_count,
            }
            .into());
        }
        if let Some(slot) = seen.get_mut(*index) {
            *slot = true;
        }
    }
    Ok(())
}

/// Convenience wrapper backends call: validate inputs, run `embed_core`,
/// then validate the output.
///
/// # Errors
///
/// Propagates [`ModelError`] failures from [`validate_contents`], the embed core, and
/// [`validate_result`].
pub fn embed_validated(
    model: &dyn EmbeddingModel,
    inputs: &[EmbeddingInput<'_>],
    embed_core: impl FnOnce() -> EngineResult<EmbeddingResult>,
) -> EngineResult<EmbeddingResult> {
    validate_contents(model, inputs)?;
    let result = embed_core()?;
    validate_result(model, inputs.len(), &result)?;
    Ok(result)
}

/// One rerank candidate.
#[derive(Debug, Clone, PartialEq)]
pub struct RankingCandidate {
    pub id: String,
    pub content: Content,
}

/// Score assigned to one candidate; order is the backend's rank order.
#[derive(Debug, Clone, PartialEq)]
pub struct RankingScore {
    pub id: String,
    pub score: f64,
}

/// Identity of a loaded ranking model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RankingModelInfo {
    pub reference: String,
    pub provider: String,
    pub name: String,
}

/// A loaded reranking model: scores candidates against a query.
pub trait RankingModel: Send + Sync {
    fn info(&self) -> &RankingModelInfo;

    /// Scores every candidate against `query`; returns one score per
    /// candidate, best first.
    ///
    /// # Errors
    ///
    /// Returns an error when scoring fails; backends report their own failure conditions.
    fn rank(
        &self,
        query: &Content,
        candidates: &[RankingCandidate],
    ) -> EngineResult<Vec<RankingScore>>;
}
