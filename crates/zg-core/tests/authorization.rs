//! Phase F proof: a remote-embedding call without a permit fails closed
//! before any network traffic, with the TS-exact `AUTH` code.

use zg_core::models::backends::QwenTextEmbeddingModel;
use zg_core::models::catalog::{EmbeddingCatalogEntry, get_embedding_model_catalog_entry};
use zg_core::models::embeddings::ApiKey;
use zg_core::models::{EmbeddingInput, EmbeddingModel, EmbeddingPurpose};

fn qwen_text_model() -> QwenTextEmbeddingModel {
    let Some(EmbeddingCatalogEntry::QwenText(entry)) =
        get_embedding_model_catalog_entry("qwen/text-embedding-v4")
    else {
        panic!("qwen/text-embedding-v4 must be in the catalog");
    };
    QwenTextEmbeddingModel::from_plan(
        *entry,
        ApiKey::new("test-key"),
        "https://example.invalid/embeddings".to_owned(),
    )
}

#[test]
fn remote_embedding_without_permit_fails_closed() {
    let model = qwen_text_model();
    let error = model
        .embed(
            EmbeddingPurpose::Query,
            &[EmbeddingInput::Text {
                text: "not authorized",
            }],
        )
        .expect_err("embed without a permit must fail");
    assert_eq!(
        error.code().to_string(),
        "ZVEC_GREP.ENGINE.AUTH.REMOTE_EMBEDDING_REQUIRED"
    );
    assert!(error.message().contains("authorization is required"));
}
