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
fn search_input_schema_derives_object() {
    let schema = input_schema_for::<SearchInput>();
    assert_eq!(
        schema.get("type").and_then(|kind| kind.as_str()),
        Some("object")
    );
}
