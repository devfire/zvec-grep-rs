//! The shared `test-support` stub is reachable from this crate's test target:
//! `zg-core` builds here as a dependency (with `cfg(test)` unset), so only a
//! Cargo feature — not `cfg(test)` — can expose test fakes across crates.
// Test targets exercise fallible fixtures directly: `unwrap`/`expect`/`panic!`
// refusal branches are the same class the crate roots allow under `cfg(test)`
// (integration tests are separate crates, so they carry their own allow).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use zg_core::models::stub::StubEmbeddingModel;
use zg_core::models::{EmbeddingInput, EmbeddingModel, EmbeddingPurpose};

#[test]
fn stub_model_embeds_from_server_tests() {
    let model = StubEmbeddingModel::new(32);
    let result = model
        .embed(
            EmbeddingPurpose::Document,
            &[EmbeddingInput::Text { text: "hello" }],
        )
        .unwrap();
    assert_eq!(result.vectors.len(), 1);
    assert_eq!(result.vectors[0].len(), 32);
}
