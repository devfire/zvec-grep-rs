//! Model catalog: every embedding model zvec-grep knows how to load.
//!
//! Mirrors `src/engine/models/catalog.ts`. All entries are compile-time
//! constants (`&'static str` payloads, no allocation); lookups are plain
//! linear scans over a 14-element table.

use std::fmt;

use serde::{Deserialize, Serialize};

use super::download_verify::ArtifactExpectation;
use crate::types::SearchMetric;

/// `DashScope` OpenAI-compatible endpoint for Qwen text embeddings.
pub const QWEN_TEXT_EMBEDDING_ENDPOINT: &str =
    "https://dashscope.aliyuncs.com/compatible-mode/v1/embeddings";
/// `DashScope` endpoint for Qwen multimodal (VL) embeddings.
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

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// True when the reference names an entry in [`EMBEDDING_MODEL_CATALOG`].
    #[must_use]
    pub fn is_known(&self) -> bool {
        embedding_model_catalog_entry(&self.0).is_some()
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
///
/// Non-exhaustive: new backends must not break downstream matches.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BackendKind {
    LlamaCpp,
    Qwen,
    TransformersJs,
    Model2Vec,
}

impl BackendKind {
    #[must_use]
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
    #[must_use]
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
    #[must_use]
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
    #[must_use]
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
    /// Immutable Hugging Face commit revision the GGUF is fetched at:
    /// cache paths and download URLs go through this pin, never `main`
    /// or a literal at a call site.
    pub revision: &'static str,
    /// Upstream-recorded size/SHA-256 for the GGUF artifact, verified
    /// before a download is published and on every cache revalidation.
    pub artifact: ArtifactExpectation<'static>,
}

/// Builds a pinned size/SHA-256 expectation from upstream-recorded values.
const fn pinned(size: u64, sha256: &'static str) -> ArtifactExpectation<'static> {
    ArtifactExpectation::new(Some(size), Some(sha256))
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
    /// Upstream-recorded size/SHA-256 for the ONNX graph file.
    pub onnx_artifact: ArtifactExpectation<'static>,
    /// Upstream-recorded size/SHA-256 for the external-data companion
    /// (`*_data`), or `UNVERIFIED` when the model has none upstream.
    pub onnx_data_artifact: ArtifactExpectation<'static>,
    /// Upstream-recorded size/SHA-256 for `tokenizer.json`.
    pub tokenizer_artifact: ArtifactExpectation<'static>,
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
    /// Upstream-recorded size/SHA-256 for `model_file`.
    pub model_artifact: ArtifactExpectation<'static>,
    /// Upstream-recorded size/SHA-256 for `tokenizer_file`.
    pub tokenizer_artifact: ArtifactExpectation<'static>,
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
    #[must_use]
    pub fn reference(&self) -> &'static str {
        match self {
            Self::LlamaCpp(e) => e.reference,
            Self::QwenText(e) => e.reference,
            Self::QwenMultimodal(e) => e.reference,
            Self::TransformersJs(e) => e.reference,
            Self::Model2Vec(e) => e.reference,
        }
    }

    #[must_use]
    pub fn provider(&self) -> &'static str {
        match self {
            Self::LlamaCpp(e) => e.provider,
            Self::QwenText(e) => e.provider,
            Self::QwenMultimodal(e) => e.provider,
            Self::TransformersJs(e) => e.provider,
            Self::Model2Vec(e) => e.provider,
        }
    }

    #[must_use]
    pub fn model(&self) -> &'static str {
        match self {
            Self::LlamaCpp(e) => e.model,
            Self::QwenText(e) => e.model,
            Self::QwenMultimodal(e) => e.model,
            Self::TransformersJs(e) => e.model,
            Self::Model2Vec(e) => e.model,
        }
    }

    #[must_use]
    pub fn dimension(&self) -> usize {
        match self {
            Self::LlamaCpp(e) => e.dimension,
            Self::QwenText(e) => e.dimension,
            Self::QwenMultimodal(e) => e.dimension,
            Self::TransformersJs(e) => e.dimension,
            Self::Model2Vec(e) => e.dimension,
        }
    }

    #[must_use]
    pub fn metric(&self) -> SearchMetric {
        SearchMetric::Cosine
    }

    #[must_use]
    pub fn backend(&self) -> BackendKind {
        match self {
            Self::LlamaCpp(_) => BackendKind::LlamaCpp,
            Self::QwenText(_) | Self::QwenMultimodal(_) => BackendKind::Qwen,
            Self::TransformersJs(_) => BackendKind::TransformersJs,
            Self::Model2Vec(_) => BackendKind::Model2Vec,
        }
    }

    #[must_use]
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
    #[must_use]
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
        revision: "0f741b5a6585bd53aeb15cd1372c56f2a0f65e12",
        artifact: ArtifactExpectation::new(
            Some(333_590_944),
            Some("b5ce9d77a3fc4b3b39ccb5643c36777911cc4eb46a66962eadfa3f5f60490d63"),
        ),
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
        revision: "370f27d7550e0def9b39c1f16d3fbaa13aa67728",
        artifact: ArtifactExpectation::new(
            Some(639_150_592),
            Some("06507c7b42688469c4e7298b0a1e16deff06caf291cf0a5b278c308249c3e439"),
        ),
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
        onnx_artifact: pinned(
            132_562,
            "266acb3edd98a1932f876d15bd8f7881a4955d0d53a6d5e79900f788b432de09",
        ),
        onnx_data_artifact: pinned(
            61_185_024,
            "77aff1dfb1e0591a40b61a91e5a97796f14c2aff56706e345bef5cbb3613b8cc",
        ),
        tokenizer_artifact: pinned(
            533_603,
            "ea77de727ef7fd34d177b83b4b1f1d3bb8884c95c90b6554a0adb0b3b65350a9",
        ),
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
        onnx_artifact: pinned(
            69_663,
            "e4dcb918111189b7686147e309379832fce83d4ecbf17c395961749b5788c786",
        ),
        onnx_data_artifact: pinned(
            54_429_696,
            "56fb7a55115e900196115a74e399beb45c2f41ae00b99525d46fb52935c4ee2a",
        ),
        tokenizer_artifact: pinned(
            533_808,
            "07805d116826679de90b4edeb2222269c4b8753bc0981be4399f732b2708e904",
        ),
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
        model_artifact: pinned(
            129_210_456,
            "07609e5bd33aad37900b3fd62f4ec96f6daec88ca4d46b9d8b928bfababf6ea0",
        ),
        tokenizer_artifact: pinned(
            1_493_150,
            "7d75cbc54318138807c401b0f0c9721117c628b39de8e8e0edb6cb17e0ee7d18",
        ),
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
        model_artifact: pinned(
            512_361_560,
            "14b5eb39cb4ce5666da8ad1f3dc6be4346e9b2d601c073302fa0a31bf7943397",
        ),
        tokenizer_artifact: pinned(
            18_616_131,
            "19f1909063da3cfe3bd83a782381f040dccea475f4816de11116444a73e1b6a1",
        ),
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
        model_artifact: pinned(
            32_490_072,
            "75cf7a6c2171b230ad19b1e7d8e0b1aee86da5a02af8e7cacedd9921d227623c",
        ),
        tokenizer_artifact: pinned(
            1_024_340,
            "107bbdcbad4bff1d299b7a4c3a2fb17c52890688b7dd0e4c9deab79d3c4f3d45",
        ),
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
        onnx_artifact: pinned(
            118_308_185,
            "f80102d3f2a1229f387d3c81909990d8945513e347b0eab049f7de3c6f98c193",
        ),
        onnx_data_artifact: ArtifactExpectation::UNVERIFIED,
        tokenizer_artifact: pinned(
            17_082_730,
            "0b44a9d7b51c3c62626640cda0e2c2f70fdacdc25bbbd68038369d14ebdf4c39",
        ),
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
        onnx_artifact: pinned(
            161_895_621,
            "ed45870251c9f0cf656e78aab0d37a23489066df8a222bb1c8caf8a45f2cb16d",
        ),
        onnx_data_artifact: ArtifactExpectation::UNVERIFIED,
        tokenizer_artifact: pinned(
            2_561_316,
            "b01c78a902aa4facb2f47f95449f48e2f7bbfea5d2472ee2f6ce92323c6f86e5",
        ),
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
        onnx_artifact: pinned(
            224_152_761,
            "5d1278a1ba749c06b82f9a2f65c2c1c5765d36f2eb88b4888de62ff12b0724a2",
        ),
        onnx_data_artifact: ArtifactExpectation::UNVERIFIED,
        tokenizer_artifact: pinned(
            3_583_228,
            "6c8aaa9a542084f2457eab775d4eeb51f92a70c0fd9de28d5edb0ddec3c08d30",
        ),
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
        onnx_artifact: pinned(
            165_113_221,
            "314976b7b9fba83283f9c8a29ee680a159fa485f52104e3fa39d3d5858337003",
        ),
        onnx_data_artifact: ArtifactExpectation::UNVERIFIED,
        tokenizer_artifact: pinned(
            711_396,
            "d241a60d5e8f04cc1b2b3e9ef7a4921b27bf526d9f6050ab90f9267a1f9e5c66",
        ),
    }),
];

/// All catalog entries, in catalog order.
#[must_use]
pub fn list_embedding_models() -> &'static [EmbeddingCatalogEntry] {
    EMBEDDING_MODEL_CATALOG
}

/// Finds the catalog entry for `reference`, if it names a known model.
#[must_use]
pub fn get_embedding_model_catalog_entry(
    reference: &str,
) -> Option<&'static EmbeddingCatalogEntry> {
    EMBEDDING_MODEL_CATALOG
        .iter()
        .find(|entry| entry.reference() == reference)
}

/// Preferred name for [`get_embedding_model_catalog_entry`]: finds the
/// catalog entry for `reference`, if it names a known model.
///
/// Thin alias only: callers outside this module still use the historic name,
/// so the historic name stays the canonical implementation for one release
/// (no cross-file churn).
#[must_use]
pub fn embedding_model_catalog_entry(reference: &str) -> Option<&'static EmbeddingCatalogEntry> {
    get_embedding_model_catalog_entry(reference)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_commit_revision(text: &str) -> bool {
        text.len() == 40
            && text
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    }

    fn is_sha256_hex(text: &str) -> bool {
        text.len() == 64
            && text
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    }

    #[test]
    fn gguf_entries_are_pinned_and_verified() {
        let mut seen = 0usize;
        for entry in EMBEDDING_MODEL_CATALOG {
            let EmbeddingCatalogEntry::LlamaCpp(entry) = entry else {
                continue;
            };
            seen += 1;
            assert_ne!(entry.revision, "main", "{}", entry.reference);
            assert!(is_commit_revision(entry.revision), "{}", entry.reference);
            assert!(
                entry.artifact.size.is_some_and(|size| size > 0),
                "{}",
                entry.reference
            );
            let sha = entry.artifact.sha256.unwrap_or("");
            assert!(is_sha256_hex(sha), "{} sha256={sha}", entry.reference);
        }
        assert_eq!(seen, 2);
    }

    fn assert_pinned(expectation: ArtifactExpectation<'static>, reference: &str) {
        assert!(
            expectation.size.is_some_and(|size| size > 0),
            "{reference} size"
        );
        let sha = expectation.sha256.unwrap_or("");
        assert!(is_sha256_hex(sha), "{reference} sha256={sha}");
    }

    #[test]
    fn downloaded_artifacts_carry_size_and_sha256() {
        let (mut onnx, mut model2vec) = (0usize, 0usize);
        for entry in EMBEDDING_MODEL_CATALOG {
            match entry {
                EmbeddingCatalogEntry::TransformersJs(e) => {
                    onnx += 1;
                    assert_pinned(e.onnx_artifact, e.reference);
                    assert_pinned(e.tokenizer_artifact, e.reference);
                }
                EmbeddingCatalogEntry::Model2Vec(e) => {
                    model2vec += 1;
                    assert_pinned(e.model_artifact, e.reference);
                    assert_pinned(e.tokenizer_artifact, e.reference);
                }
                EmbeddingCatalogEntry::LlamaCpp(_)
                | EmbeddingCatalogEntry::QwenText(_)
                | EmbeddingCatalogEntry::QwenMultimodal(_) => {}
            }
        }
        assert_eq!(onnx, 6);
        assert_eq!(model2vec, 3);
    }
}
