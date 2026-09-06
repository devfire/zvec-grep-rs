//! Backend implementations.

pub mod model2vec;
pub mod qwen;

pub use model2vec::Model2VecEmbeddingModel;
pub use qwen::{Qwen3VlEmbeddingModel, QwenTextEmbeddingModel};
