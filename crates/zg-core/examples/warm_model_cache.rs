//! Seeds the local embedding-model cache for the vector-parity gate.
//!
//! `cargo run -p zg-core --all-features --example warm_model_cache`
//! constructs each parity-gate model and calls
//! [`EmbeddingModel::prepare`](zg_core::models::EmbeddingModel::prepare),
//! downloading missing artifacts into the shared model cache
//! (`$ZVEC_GREP_MODEL_CACHE`, else `~/.zvec-grep/models`). CI runs this on
//! cache miss before the gate (`ZVEC_REQUIRE_MODELS=1 ... --test
//! vector_parity`), so the gate checks real coverage instead of skipping.
//!
//! Requires the `onnx` and `llama` features (see `required-features` in
//! `crates/zg-core/Cargo.toml`); without them Cargo skips this target.

// Stdout lines are the user-facing progress contract of this example, like
// the `zg` binary's output.
#![allow(clippy::print_stdout)]

use std::process::ExitCode;

use zg_core::models::EmbeddingModel;
use zg_core::models::catalog::ModelReference;
use zg_core::models::embeddings::CreateEmbeddingModelOptions;
use zg_core::models::factory::create_embedding_model;

/// The four local models `tests/vector_parity.rs` checks.
const PARITY_MODELS: &[&str] = &[
    "local/bge-small-en-v1.5",
    "local/all-minilm-l6-v2",
    "local/embeddinggemma-300m",
    "local/qwen3-embedding-0.6b",
];

fn main() -> ExitCode {
    let mut failures = 0_u32;
    for &reference in PARITY_MODELS {
        if let Err(err) = warm_one(reference) {
            eprintln!("warm_model_cache: {reference}: {err}");
            failures += 1;
        }
    }
    if failures == 0 {
        ExitCode::SUCCESS
    } else {
        eprintln!("warm_model_cache: {failures} model(s) failed");
        ExitCode::FAILURE
    }
}

/// Downloads (then loads) one model's artifacts into the shared cache.
///
/// # Errors
///
/// Returns a message when the model fails to construct or its artifacts
/// fail to download or load.
fn warm_one(reference: &str) -> Result<(), String> {
    let model = create_embedding_model(
        &ModelReference::new(reference),
        &CreateEmbeddingModelOptions::default(),
    )
    .map_err(|err| err.to_string())?;
    if model.is_cached() {
        println!("warm_model_cache: {reference}: already cached");
    } else {
        println!("warm_model_cache: {reference}: downloading...");
    }
    model.prepare(None).map_err(|err| err.to_string())?;
    println!("warm_model_cache: {reference}: ready");
    Ok(())
}
