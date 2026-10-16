//! Boundary regression tests (moved verbatim with the split).

use super::*;

#[test]
fn query_text_rejects_overlong_input() {
    let long = "x".repeat(MCP_MAX_QUERY_CHARS + 1);
    assert!(QueryText::parse(long).is_err());
    assert!(QueryText::parse("ok".to_owned()).is_ok());
}

#[test]
fn search_limit_rejects_zero_and_over_max() {
    assert!(SearchLimit::parse(0).is_err());
    assert!(SearchLimit::parse(MCP_MAX_SEARCH_LIMIT + 1).is_err());
    assert_eq!(SearchLimit::parse(7).unwrap().get(), 7);
}

#[test]
fn root_requires_absolute_path() {
    assert!(parse_root("").is_err());
    assert!(parse_root("relative/path").is_err());
}

#[test]
fn search_schema_advertises_preview_and_index_schema_advertises_embedding() {
    // #78: both fields must be in the advertised schemas, not just
    // accepted (preview) or rejected (embedding) at call time.
    let search = input_schema_for::<SearchInput>();
    let properties = search
        .get("properties")
        .and_then(|value| value.as_object())
        .expect("search schema properties");
    assert!(properties.contains_key("preview"), "{search:?}");
    let index = input_schema_for::<IndexInput>();
    let properties = index
        .get("properties")
        .and_then(|value| value.as_object())
        .expect("index schema properties");
    assert!(properties.contains_key("embedding"), "{index:?}");
}

#[test]
fn unknown_fields_are_rejected_not_ignored() {
    // #78: a misspelled or unsupported field must error, not be
    // silently dropped.
    let search: Result<SearchInput, _> = serde_json::from_value(serde_json::json!({
        "root": "/repo",
        "query": "alpha",
        "previeew": "full"
    }));
    assert!(search.is_err());
    let search: SearchInput = serde_json::from_value(serde_json::json!({
        "root": "/repo",
        "query": "alpha",
        "preview": "full"
    }))
    .expect("preview is a known field");
    assert_eq!(search.preview, Some(PreviewInput::Full));
    let index: Result<IndexInput, _> = serde_json::from_value(serde_json::json!({
        "root": "/repo",
        "embeddding": "local/potion-code-16m-v2"
    }));
    assert!(index.is_err());
}

#[test]
fn search_input_schema_derives_object() {
    let schema = input_schema_for::<SearchInput>();
    assert_eq!(
        schema.get("type").and_then(|kind| kind.as_str()),
        Some("object")
    );
}
