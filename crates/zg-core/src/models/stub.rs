//! Deterministic stub embedding model for tests (feature `test-support`).
//!
//! `cfg(test)` items inside `zg-core` are invisible to other crates' test
//! targets (`zg-core` builds as a regular dependency there, with `cfg(test)`
//! unset), so backend index/search tests need a stub behind a normal Cargo
//! feature. Enable it in dev-dependencies:
//! `zg-core = { workspace = true, features = ["test-support"] }`.
//!
//! Vectors are SHA-256 hashes of the input text expanded byte-wise to
//! `dimension` components in `[-0.5, 0.5]` — deterministic across runs and
//! processes, distinct per input. The stub rejects empty text, images, and
//! oversized batches through the shared validation, exactly like real
//! backends.

use super::embeddings::{EmbeddingResult, embed_validated};
use super::error::ModelError;
use super::{
    EmbeddingInput, EmbeddingInputKind, EmbeddingModel, EmbeddingModelInfo, EmbeddingPurpose,
};
use crate::error::{EngineError, EngineResult};
use crate::types::SearchMetric;

/// Deterministic hash-of-input embedding model for tests.
#[derive(Debug, Clone)]
pub struct StubEmbeddingModel {
    info: EmbeddingModelInfo,
    max_batch_size: usize,
}

impl StubEmbeddingModel {
    /// Builds a stub emitting `dimension`-wide vectors.
    pub fn new(dimension: usize) -> Self {
        Self {
            info: EmbeddingModelInfo {
                reference: "stub/deterministic".to_owned(),
                provider: "stub".to_owned(),
                model: "stub-hash".to_owned(),
                dimension,
                metric: SearchMetric::Cosine,
                supports_images: false,
                max_input_tokens: None,
                input_kinds: vec![EmbeddingInputKind::Text],
                endpoint: None,
                default_concurrency: None,
            },
            max_batch_size: 1024,
        }
    }

    fn stub_vector(text: &str, dimension: usize) -> Vec<f32> {
        use sha2::Digest as _;
        let mut out = Vec::with_capacity(dimension);
        let mut counter = 0u64;
        while out.len() < dimension {
            let mut hasher = sha2::Sha256::new();
            hasher.update(text.as_bytes());
            hasher.update(counter.to_le_bytes());
            for byte in hasher.finalize() {
                if out.len() == dimension {
                    break;
                }
                out.push(f32::from(byte) / 255.0 - 0.5);
            }
            counter += 1;
        }
        out
    }
}

impl EmbeddingModel for StubEmbeddingModel {
    fn info(&self) -> &EmbeddingModelInfo {
        &self.info
    }

    fn max_batch_size(&self) -> usize {
        self.max_batch_size
    }

    fn embed(
        &self,
        _purpose: EmbeddingPurpose,
        inputs: &[EmbeddingInput<'_>],
    ) -> EngineResult<EmbeddingResult> {
        embed_validated(self, inputs, || {
            let mut vectors = Vec::with_capacity(inputs.len());
            for (index, input) in inputs.iter().enumerate() {
                match input {
                    EmbeddingInput::Text { text } => {
                        vectors.push(Self::stub_vector(text, self.info.dimension));
                    }
                    EmbeddingInput::Image { .. } => {
                        return Err(EngineError::from(ModelError::UnsupportedImage {
                            reference: self.info.reference.clone(),
                            index: Some(index),
                        }));
                    }
                }
            }
            Ok(EmbeddingResult {
                vectors,
                truncated: Vec::new(),
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vectors_are_deterministic() {
        let model = StubEmbeddingModel::new(16);
        let input = [EmbeddingInput::Text { text: "hello" }];
        let first = model
            .embed(EmbeddingPurpose::Document, &input)
            .unwrap()
            .vectors;
        let second = model
            .embed(EmbeddingPurpose::Document, &input)
            .unwrap()
            .vectors;
        assert_eq!(first, second);
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].len(), 16);
    }

    #[test]
    fn distinct_inputs_give_distinct_vectors() {
        let model = StubEmbeddingModel::new(16);
        let left = model
            .embed(
                EmbeddingPurpose::Document,
                &[EmbeddingInput::Text { text: "alpha" }],
            )
            .unwrap()
            .vectors;
        let right = model
            .embed(
                EmbeddingPurpose::Document,
                &[EmbeddingInput::Text { text: "beta" }],
            )
            .unwrap()
            .vectors;
        assert_ne!(left, right);
    }

    #[test]
    fn rejects_empty_text() {
        let model = StubEmbeddingModel::new(8);
        let err = model
            .embed(
                EmbeddingPurpose::Document,
                &[EmbeddingInput::Text { text: "  " }],
            )
            .unwrap_err();
        assert_eq!(
            err.code().to_string(),
            "ZVEC_GREP.ENGINE.MODELS.EMBEDDING_EMPTY_TEXT"
        );
    }

    #[test]
    fn rejects_oversized_batches() {
        let model = StubEmbeddingModel::new(8);
        let inputs = vec![EmbeddingInput::Text { text: "x" }; 1025];
        let err = model
            .embed(EmbeddingPurpose::Document, &inputs)
            .unwrap_err();
        assert_eq!(
            err.code().to_string(),
            "ZVEC_GREP.ENGINE.MODELS.EMBEDDING_BATCH_TOO_LARGE"
        );
    }
}
