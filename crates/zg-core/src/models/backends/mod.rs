//! Backend implementations.
//!
//! The heavy local backends are cargo features (M7): `onnx` pulls in
//! `ort`, `llama` pulls in `llama-cpp-2`, and both stay compiled out of
//! default builds so `cargo install zg` needs no C++ toolchain and
//! `cargo test -p zg-core` stays hermetic.

#[cfg(feature = "llama")]
pub mod llama_cpp;
pub mod model2vec;
#[cfg(feature = "onnx")]
pub mod onnx;
pub mod qwen;

#[cfg(feature = "llama")]
pub use llama_cpp::LlamaCppEmbeddingModel;
pub use model2vec::Model2VecEmbeddingModel;
#[cfg(feature = "onnx")]
pub use onnx::OnnxEmbeddingModel;
pub use qwen::{Qwen3VlEmbeddingModel, QwenTextEmbeddingModel};
