//! Model catalog: every embedding model zvec-grep knows how to load.
//!
//! Mirrors `src/engine/models/catalog.ts`. All entries are compile-time
//! constants (`&'static str` payloads, no allocation); lookups are plain
//! linear scans over a 14-element table.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::types::SearchMetric;

/// DashScope OpenAI-compatible endpoint for Qwen text embeddings.
pub const QWEN_TEXT_EMBEDDING_ENDPOINT: &str =
    "https://dashscope.aliyuncs.com/compatible-mode/v1/embeddings";
/// DashScope endpoint for Qwen multimodal (VL) embeddings.
pub const QWEN3_VL_EMBEDDING_ENDPOINT: &str = "https://dashscope.aliyuncs.com/api/v1/services/embeddings/multimodal-embedding/multimodal-embedding";

/// Fully-qualified catalog reference, e.g. `local/potion-retrieval-32m`.
///
/// Newtype over the raw string so model ids cannot be confused with repo
/// names, file paths, or provider keys.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ModelReference(String);

impl ModelReference {
    pub fn new(reference: impl Into<String>) -> Self {
        Self(reference.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// True when the reference names an entry in [`EMBEDDING_MODEL_CATALOG`].
    pub fn is_known(&self) -> bool {
        get_embedding_model_catalog_entry(&self.0).is_some()
    }
}

impl fmt::Display for ModelReference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for ModelReference {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

impl From<String> for ModelReference {
    fn from(value: String) -> Self {
        Self(value)
    }
}

/// Backend that serves a catalog entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BackendKind {
    LlamaCpp,
    Qwen,
    TransformersJs,
    Model2Vec,
}

impl BackendKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::LlamaCpp => "llama-cpp",
            Self::Qwen => "qwen",
            Self::TransformersJs => "transformers-js",
            Self::Model2Vec => "model2vec",
        }
    }
}

/// GGUF prompt format understood by the llama-cpp backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LlamaModelFormat {
    Embeddinggemma,
    Qwen3,
}

impl LlamaModelFormat {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Embeddinggemma => "embeddinggemma",
            Self::Qwen3 => "qwen3",
        }
    }
}

/// Quantization dtype for a transformers-js (ONNX) model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TransformersDtype {
    Q4,
    Q8,
    Fp32,
}

impl TransformersDtype {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Q4 => "q4",
            Self::Q8 => "q8",
            Self::Fp32 => "fp32",
        }
    }
}

/// Pooling strategy used to reduce token vectors to one embedding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PoolingKind {
    Cls,
    Mean,
}

impl PoolingKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Cls => "cls",
            Self::Mean => "mean",
        }
    }
}

/// Catalog entry for a GGUF model served by llama-cpp.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LlamaCppEntry {
    pub reference: &'static str,
    pub provider: &'static str,
    pub model: &'static str,
    pub uri: &'static str,
    pub dimension: usize,
    pub context_size: usize,
    pub max_batch_size: usize,
    pub format: LlamaModelFormat,
}

/// Catalog entry for a Qwen text embedding model (remote API).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QwenTextEntry {
    pub reference: &'static str,
    pub provider: &'static str,
    pub model: &'static str,
    pub dimension: usize,
    pub default_endpoint: &'static str,
    pub max_batch_size: usize,
    pub max_input_tokens: usize,
}

/// Catalog entry for the Qwen multimodal (text+image) embedding model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QwenMultimodalEntry {
    pub reference: &'static str,
    pub provider: &'static str,
    pub model: &'static str,
    pub dimension: usize,
    pub default_endpoint: &'static str,
    pub max_batch_size: usize,
    pub max_input_tokens: usize,
    pub max_image_bytes: u64,
}

/// Catalog entry for an ONNX model served by transformers-js.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransformersJsEntry {
    pub reference: &'static str,
    pub provider: &'static str,
    pub model: &'static str,
    pub repo: &'static str,
    pub revision: &'static str,
    pub dtype: TransformersDtype,
    pub dimension: usize,
    pub pooling: PoolingKind,
    pub normalize: bool,
    pub query_prefix: Option<&'static str>,
    pub document_prefix: Option<&'static str>,
    pub max_input_tokens: usize,
    pub max_batch_size: usize,
}

/// Catalog entry for a static-embedding (model2vec) model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Model2VecEntry {
    pub reference: &'static str,
    pub provider: &'static str,
    pub model: &'static str,
    pub repo: &'static str,
    pub revision: &'static str,
    pub model_file: &'static str,
    pub embedding_tensor: &'static str,
    pub tokenizer_file: &'static str,
    pub dimension: usize,
    pub normalize: bool,
    pub max_input_tokens: usize,
    pub max_batch_size: usize,
    pub default_concurrency: usize,
}

/// One row of the embedding model catalog, discriminated by backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmbeddingCatalogEntry {
    LlamaCpp(LlamaCppEntry),
    QwenText(QwenTextEntry),
    QwenMultimodal(QwenMultimodalEntry),
    TransformersJs(TransformersJsEntry),
    Model2Vec(Model2VecEntry),
}

impl EmbeddingCatalogEntry {
    pub fn reference(&self) -> &'static str {
        match self {
            Self::LlamaCpp(e) => e.reference,
            Self::QwenText(e) => e.reference,
            Self::QwenMultimodal(e) => e.reference,
            Self::TransformersJs(e) => e.reference,
            Self::Model2Vec(e) => e.reference,
        }
    }

    pub fn provider(&self) -> &'static str {
        match self {
            Self::LlamaCpp(e) => e.provider,
            Self::QwenText(e) => e.provider,
            Self::QwenMultimodal(e) => e.provider,
            Self::TransformersJs(e) => e.provider,
            Self::Model2Vec(e) => e.provider,
        }
    }

    pub fn model(&self) -> &'static str {
        match self {
            Self::LlamaCpp(e) => e.model,
            Self::QwenText(e) => e.model,
            Self::QwenMultimodal(e) => e.model,
            Self::TransformersJs(e) => e.model,
            Self::Model2Vec(e) => e.model,
        }
    }

    pub fn dimension(&self) -> usize {
        match self {
            Self::LlamaCpp(e) => e.dimension,
            Self::QwenText(e) => e.dimension,
            Self::QwenMultimodal(e) => e.dimension,
            Self::TransformersJs(e) => e.dimension,
            Self::Model2Vec(e) => e.dimension,
        }
    }

    pub fn metric(&self) -> SearchMetric {
        SearchMetric::Cosine
    }

    pub fn backend(&self) -> BackendKind {
        match self {
            Self::LlamaCpp(_) => BackendKind::LlamaCpp,
            Self::QwenText(_) | Self::QwenMultimodal(_) => BackendKind::Qwen,
            Self::TransformersJs(_) => BackendKind::TransformersJs,
            Self::Model2Vec(_) => BackendKind::Model2Vec,
        }
    }

    pub fn max_batch_size(&self) -> usize {
        match self {
            Self::LlamaCpp(e) => e.max_batch_size,
            Self::QwenText(e) => e.max_batch_size,
            Self::QwenMultimodal(e) => e.max_batch_size,
            Self::TransformersJs(e) => e.max_batch_size,
            Self::Model2Vec(e) => e.max_batch_size,
        }
    }

    /// True for the single entry that accepts image inputs.
    pub fn supports_images(&self) -> bool {
        matches!(self, Self::QwenMultimodal(_))
    }
}

/// Every embedding model zvec-grep can load, mirroring
/// `EMBEDDING_MODEL_CATALOG` in the TypeScript implementation.
pub static EMBEDDING_MODEL_CATALOG: &[EmbeddingCatalogEntry] = &[
    EmbeddingCatalogEntry::LlamaCpp(LlamaCppEntry {
        reference: "local/embeddinggemma-300m",
        provider: "local",
        model: "embeddinggemma-300m",
        uri: "hf:ggml-org/embeddinggemma-300M-GGUF/embeddinggemma-300M-Q8_0.gguf",
        dimension: 768,
        context_size: 2048,
        max_batch_size: 16,
        format: LlamaModelFormat::Embeddinggemma,
    }),
    EmbeddingCatalogEntry::LlamaCpp(LlamaCppEntry {
        reference: "local/qwen3-embedding-0.6b",
        provider: "local",
        model: "qwen3-embedding-0.6b",
        uri: "hf:Qwen/Qwen3-Embedding-0.6B-GGUF/Qwen3-Embedding-0.6B-Q8_0.gguf",
        dimension: 1024,
        context_size: 8192,
        max_batch_size: 8,
        format: LlamaModelFormat::Qwen3,
    }),
    EmbeddingCatalogEntry::QwenText(QwenTextEntry {
        reference: "qwen/text-embedding-v4",
        provider: "qwen",
        model: "text-embedding-v4",
        dimension: 1024,
        default_endpoint: QWEN_TEXT_EMBEDDING_ENDPOINT,
        max_batch_size: 10,
        max_input_tokens: 8192,
    }),
    EmbeddingCatalogEntry::QwenText(QwenTextEntry {
        reference: "qwen/qwen3.7-text-embedding",
        provider: "qwen",
        model: "qwen3.7-text-embedding",
        dimension: 1024,
        default_endpoint: QWEN_TEXT_EMBEDDING_ENDPOINT,
        max_batch_size: 20,
        max_input_tokens: 128_000,
    }),
    EmbeddingCatalogEntry::QwenMultimodal(QwenMultimodalEntry {
        reference: "qwen/qwen3-vl-embedding",
        provider: "qwen",
        model: "qwen3-vl-embedding",
        dimension: 2560,
        default_endpoint: QWEN3_VL_EMBEDDING_ENDPOINT,
        max_batch_size: 20,
        max_input_tokens: 32_000,
        max_image_bytes: 10 * 1024 * 1024,
    }),
    EmbeddingCatalogEntry::TransformersJs(TransformersJsEntry {
        reference: "local/bge-small-en-v1.5",
        provider: "local",
        model: "bge-small-en-v1.5",
        repo: "onnx-community/bge-small-en-v1.5-ONNX",
        revision: "4a9a46c7b88fa408e650a571a1800243f26309bd",
        dtype: TransformersDtype::Q4,
        dimension: 384,
        pooling: PoolingKind::Cls,
        normalize: true,
        query_prefix: Some("Represent this sentence for searching relevant passages: "),
        document_prefix: None,
        max_input_tokens: 512,
        max_batch_size: 4,
    }),
    EmbeddingCatalogEntry::TransformersJs(TransformersJsEntry {
        reference: "local/all-minilm-l6-v2",
        provider: "local",
        model: "all-minilm-l6-v2",
        repo: "onnx-community/all-MiniLM-L6-v2-ONNX",
        revision: "aff7a1dc4e8a1ea593e6ea21e95c22ef0a25966f",
        dtype: TransformersDtype::Q4,
        dimension: 384,
        pooling: PoolingKind::Mean,
        normalize: true,
        query_prefix: None,
        document_prefix: None,
        max_input_tokens: 256,
        max_batch_size: 4,
    }),
    EmbeddingCatalogEntry::Model2Vec(Model2VecEntry {
        reference: "local/potion-retrieval-32m",
        provider: "local",
        model: "potion-retrieval-32m",
        repo: "minishlab/potion-retrieval-32M",
        revision: "6fc8051fab2a1e0ee76689cf08c853792ac285e7",
        model_file: "model.safetensors",
        embedding_tensor: "embeddings",
        tokenizer_file: "tokenizer.json",
        dimension: 512,
        normalize: true,
        max_input_tokens: 1024,
        max_batch_size: 256,
        default_concurrency: 2,
    }),
    EmbeddingCatalogEntry::Model2Vec(Model2VecEntry {
        reference: "local/potion-multilingual-128m",
        provider: "local",
        model: "potion-multilingual-128m",
        repo: "minishlab/potion-multilingual-128M",
        revision: "73908c3438cf03b6a01bcb9611d62b23d0726f08",
        model_file: "model.safetensors",
        embedding_tensor: "embeddings",
        tokenizer_file: "tokenizer.json",
        dimension: 256,
        normalize: true,
        max_input_tokens: 1024,
        max_batch_size: 256,
        default_concurrency: 2,
    }),
    EmbeddingCatalogEntry::Model2Vec(Model2VecEntry {
        reference: "local/potion-code-16m-v2",
        provider: "local",
        model: "potion-code-16m-v2",
        repo: "minishlab/potion-code-16M-v2",
        revision: "e9d2a44ca6a05ac6685f3b23709ea57eb7352d5b",
        model_file: "model.safetensors",
        embedding_tensor: "embeddings",
        tokenizer_file: "tokenizer.json",
        dimension: 256,
        normalize: true,
        max_input_tokens: 1024,
        max_batch_size: 256,
        default_concurrency: 2,
    }),
    EmbeddingCatalogEntry::TransformersJs(TransformersJsEntry {
        reference: "local/multilingual-e5-small",
        provider: "local",
        model: "multilingual-e5-small",
        repo: "Xenova/multilingual-e5-small",
        revision: "761b726dd34fb83930e26aab4e9ac3899aa1fa78",
        dtype: TransformersDtype::Q8,
        dimension: 384,
        pooling: PoolingKind::Mean,
        normalize: true,
        query_prefix: Some("query: "),
        document_prefix: Some("passage: "),
        max_input_tokens: 512,
        max_batch_size: 4,
    }),
    EmbeddingCatalogEntry::TransformersJs(TransformersJsEntry {
        reference: "local/jina-embeddings-v2-base-code",
        provider: "local",
        model: "jina-embeddings-v2-base-code",
        repo: "jinaai/jina-embeddings-v2-base-code",
        revision: "516f4baf13dec4ddddda8631e019b5737c8bc250",
        dtype: TransformersDtype::Q8,
        dimension: 768,
        pooling: PoolingKind::Mean,
        normalize: true,
        query_prefix: None,
        document_prefix: None,
        max_input_tokens: 8192,
        max_batch_size: 2,
    }),
    EmbeddingCatalogEntry::TransformersJs(TransformersJsEntry {
        reference: "local/gte-modernbert-base",
        provider: "local",
        model: "gte-modernbert-base",
        repo: "Alibaba-NLP/gte-modernbert-base",
        revision: "e7f32e3c00f91d699e8c43b53106206bcc72bb22",
        dtype: TransformersDtype::Q4,
        dimension: 768,
        pooling: PoolingKind::Cls,
        normalize: true,
        query_prefix: None,
        document_prefix: None,
        max_input_tokens: 8192,
        max_batch_size: 2,
    }),
    EmbeddingCatalogEntry::TransformersJs(TransformersJsEntry {
        reference: "local/nomic-embed-text-v1.5",
        provider: "local",
        model: "nomic-embed-text-v1.5",
        repo: "nomic-ai/nomic-embed-text-v1.5",
        revision: "e9b6763023c676ca8431644204f50c2b100d9aab",
        dtype: TransformersDtype::Q4,
        dimension: 768,
        pooling: PoolingKind::Mean,
        normalize: true,
        query_prefix: Some("search_query: "),
        document_prefix: Some("search_document: "),
        max_input_tokens: 8192,
        max_batch_size: 2,
    }),
];

/// All catalog entries, in catalog order.
pub fn list_embedding_models() -> &'static [EmbeddingCatalogEntry] {
    EMBEDDING_MODEL_CATALOG
}

/// Finds the catalog entry for `reference`, if it names a known model.
pub fn get_embedding_model_catalog_entry(
    reference: &str,
) -> Option<&'static EmbeddingCatalogEntry> {
    EMBEDDING_MODEL_CATALOG
        .iter()
        .find(|entry| entry.reference() == reference)
}
