//! Embedding model abstraction: catalog, factory, and backends.
//!
//! Backends ship behind one trait; local model2vec inference is native Rust
//! (tokenizer + safetensors matmul), remote providers speak HTTP.

pub mod backends;
pub mod catalog;
pub mod download;
pub mod embeddings;
pub mod error;
pub mod factory;
pub mod resolution;
#[cfg(feature = "test-support")]
pub mod stub;

use std::sync::Arc;

use crate::error::EngineResult;
use crate::types::{ImageFormat, SearchMetric};

/// What the embedding is for — some backends prefix instructions by purpose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EmbeddingPurpose {
    #[default]
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
    /// Remote endpoint, when the backend sends data off-host (`qwen`).
    /// `None` for local backends; the authorization planner reads this
    /// exactly like TS (`model.endpoint`).
    pub endpoint: Option<String>,
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
///
/// Named per event type (M2): this is the model-load sink, distinct from
/// the index-progress sink ([`crate::pipeline::indexing::IndexProgressSink`]).
pub type ModelLoadSink = Arc<dyn Fn(EmbeddingModelProgress) + Send + Sync>;

/// A loaded embedding model.
///
/// Implementations must be `Send + Sync`; batch embedding may internally
/// parallelize, but `embed` itself is called from arbitrary threads.
///
/// Intentionally **un**sealed: third-party backends are a supported
/// extension point (the pool accepts any `Arc<dyn EmbeddingModel>` via its
/// model factory), so downstream crates may implement this. New methods
/// must therefore carry default bodies.
pub trait EmbeddingModel: Send + Sync {
    fn info(&self) -> &EmbeddingModelInfo;

    /// Maximum inputs accepted per [`EmbeddingModel::embed`] call.
    fn max_batch_size(&self) -> usize;

    /// True when local artifacts are already in the cache, so `prepare`
    /// loads without network traffic. Remote backends return true (there
    /// is nothing to cache); local backends override this with their
    /// artifact paths. The vector-parity gate uses it to skip — with a
    /// printed reason — instead of downloading gigabytes inside a test.
    fn is_cached(&self) -> bool {
        true
    }

    /// Downloads/loads the model (local backends); remote backends no-op.
    /// Mirrors the optional TS `prepare` (local models only).
    ///
    /// # Errors
    ///
    /// Local backends return an error when artifacts fail to download or load; remote backends
    /// no-op and always succeed.
    fn prepare(&self, sink: Option<ModelLoadSink>) -> EngineResult<()> {
        let _ = sink;
        Ok(())
    }

    /// Embeds a batch of inputs; returns one vector per input, in order,
    /// plus the indices truncated to the model token limit.
    ///
    /// # Errors
    ///
    /// Returns an error when inputs fail validation or the backend fails to produce embeddings.
    fn embed(
        &self,
        purpose: EmbeddingPurpose,
        inputs: &[EmbeddingInput<'_>],
    ) -> EngineResult<embeddings::EmbeddingResult>;
}
