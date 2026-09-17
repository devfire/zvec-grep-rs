//! Visibility sentinel: `service_facade` is feature-gated and would otherwise
//! vanish silently from default-feature runs; this name appears in output instead.
// The triple underscore stands for `--` (invalid in identifiers): the name
// spells "rebuild with --features test-support". Targeted allow, not style.
#![allow(non_snake_case)]

#[test]
#[cfg(not(feature = "test-support"))]
fn service_facade_suite_skipped_rebuild_with___features_test_support() {
    // Name-only sentinel: passing tests print their names, so an unfeatured
    // `cargo test` shows the suite did not run instead of reporting nothing.
}
