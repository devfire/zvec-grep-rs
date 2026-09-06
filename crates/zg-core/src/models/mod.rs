//! Embedding model abstraction: catalog, factory, and backends.
//!
//! Backends ship behind one trait; local model2vec inference is native Rust
//! (tokenizer + safetensors matmul), remote providers speak HTTP.

pub mod backends;
pub mod catalog;
pub mod download;
pub mod embeddings;
pub mod factory;
pub mod resolution;

use std::sync::Arc;

use crate::error::EngineResult;
use crate::types::{ImageFormat, SearchMetric};

/// What the embedding is for — some backends prefix instructions by purpose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmbeddingPurpose {
    Document,
    Query,
}

/// One input to embed.
#[derive(Debug, Clone, Copy)]
pub enum EmbeddingInput<'a> {
    Text { text: &'a str },
    Image { data: &'a [u8], format: ImageFormat },
}

/// Input kind a model accepts (mirrors `inputKinds` in the TS model info).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EmbeddingInputKind {
    Text,
    Image,
}

/// Static identity of a loaded embedding model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddingModelInfo {
    /// Full catalog reference, e.g. `local/potion-retrieval-32m`.
    pub reference: String,
    pub provider: String,
    /// Provider model name, e.g. `text-embedding-v4` (mirrors `info.name`).
    pub model: String,
    pub dimension: usize,
    pub metric: SearchMetric,
    /// True when the backend accepts image inputs.
    pub supports_images: bool,
    /// Model context window in tokens, when the catalog defines one.
    pub max_input_tokens: Option<usize>,
    /// Content kinds accepted by [`EmbeddingModel::embed`].
    pub input_kinds: Vec<EmbeddingInputKind>,
    /// Suggested embedding concurrency, when the catalog defines one.
    pub default_concurrency: Option<usize>,
}

/// Progress event emitted while a model downloads or prepares.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EmbeddingModelProgress {
    pub stage: Option<EmbeddingStageKind>,
    pub downloaded_bytes: Option<u64>,
    pub total_bytes: Option<u64>,
    pub message: Option<String>,
}

/// Lifecycle stage of a model load.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmbeddingStageKind {
    Preparing,
    Downloading,
    Ready,
    Warning,
}

/// Sink for model-load progress notifications.
pub type ProgressSink = Arc<dyn Fn(EmbeddingModelProgress) + Send + Sync>;

/// A loaded embedding model.
///
/// Implementations must be `Send + Sync`; batch embedding may internally
/// parallelize, but `embed` itself is called from arbitrary threads.
pub trait EmbeddingModel: Send + Sync {
    fn info(&self) -> &EmbeddingModelInfo;

    /// Maximum inputs accepted per [`EmbeddingModel::embed`] call.
    fn max_batch_size(&self) -> usize;

    /// Downloads/loads the model (local backends); remote backends no-op.
    /// Mirrors the optional TS `prepare` (local models only).
    fn prepare(&self, sink: Option<ProgressSink>) -> EngineResult<()> {
        let _ = sink;
        Ok(())
    }

    /// Embeds a batch of inputs; returns one vector per input, in order,
    /// plus the indices truncated to the model token limit.
    fn embed(
        &self,
        purpose: EmbeddingPurpose,
        inputs: &[EmbeddingInput<'_>],
    ) -> EngineResult<embeddings::EmbeddingResult>;
}
