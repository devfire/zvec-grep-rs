//! Golden tests for the format renderers.
//!
//! Every renderer is a pure `String`-builder; these goldens pin the
//! agent/human layouts, range labels, progress bar, text helpers, index
//! counters, and workspace states. Builders are imported from their
//! canonical submodules; the facade only carries the command surface.

use super::context_agent::format_context_agent;
use super::context_human::format_context_human;
use super::error::debug_lines;
use super::index::format_index_result;
use super::progress::format_green_progress_bar;
use super::range::range_label;
use super::text::{format_score, one_line, truncate};
use super::workspace::{WorkspaceState, format_workspace_info, workspace_state};
use crate::error::CliError;
use zg_core::ids::EntityId;
use zg_core::service::types::{
    ContentStatus, ContextCoverage, ContextDiagnostics, ContextFile, ContextItem, ContextItemKind,
    ContextSource, ZvecGrepContextResult, ZvecGrepInfoResult,
};
use zg_core::types::{Content, IndexResult, Range, WorkspaceIndexStatus};

fn item(rank: usize, path: &str, text: &str) -> ContextItem {
    ContextItem {
        kind: ContextItemKind::IndexedEntity,
        rank,
        file: ContextFile {
            absolute_path: format!("/root/{path}"),
            relative_path: path.to_owned(),
            root_path: "/root".to_owned(),
        },
        range: Range::Text {
            start_line: 1,
            end_line: 3,
            start_offset: 0,
            end_offset: 10,
        },
        excerpt_range: None,
        content: Content::Text {
            text: text.to_owned(),
        },
        content_role: None,
        outline: None,
        status: ContentStatus::Fresh,
        score: Some(0.5),
        matched_by: Some("vector".to_owned()),
        metadata: None,
        entity_id: EntityId::parse("f:abc").ok(),
        trace: None,
        query_groups: Vec::new(),
        container: None,
        selection_reason: None,
    }
}

fn result() -> ZvecGrepContextResult {
    ZvecGrepContextResult {
        query: "alpha".to_owned(),
        root: "/root".to_owned(),
        source: ContextSource::Index,
        coverage: ContextCoverage::RankedSample,
        workspace_index: None,
        items: vec![item(1, "a.rs", "fn alpha() {}\nfn beta() {}\n")],
        group_results: None,
        diagnostics: ContextDiagnostics::default(),
    }
}

#[test]
fn agent_golden() {
    let text = format_context_agent(&result());
    assert!(text.contains("# 1 result for \"alpha\""), "{text}");
    assert!(
        text.contains("## 1. a.rs:1-3 (score 0.5000, via vector)"),
        "{text}"
    );
    assert!(text.contains("fn alpha() {}"), "{text}");
}

#[test]
fn agent_empty_names_the_query() {
    let mut empty = result();
    empty.items.clear();
    let text = format_context_agent(&empty);
    assert!(text.contains("No results for \"alpha\"."), "{text}");
    assert!(text.contains("--rg"), "{text}");
}

#[test]
fn human_golden_without_color() {
    let text = format_context_human(&result(), false);
    assert!(text.contains("query: alpha"), "{text}");
    assert!(text.contains("file: a.rs:1-3"), "{text}");
    assert!(!text.contains("\x1b["), "{text}");
}

#[test]
fn debug_chain_is_deep_and_non_duplicating() {
    #[derive(Debug)]
    struct Wrap(std::io::Error);
    impl std::fmt::Display for Wrap {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "wrap: {}", self.0)
        }
    }
    impl std::error::Error for Wrap {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            Some(&self.0)
        }
    }
    let inner = std::io::Error::new(std::io::ErrorKind::NotFound, "gone");
    let engine = zg_core::error::EngineError::new(
        zg_core::error::EngineErrorCode::from_static("JSON.READ_FAILED"),
        "failed to read x",
    )
    .with_source(Wrap(inner));
    // Through the transparent `Engine` variant: code + two causes, and no
    // line repeats (the skipped hop would duplicate the `error:` line).
    let lines = debug_lines(&CliError::Engine(engine));
    let [code, first, second] = lines.as_slice() else {
        panic!("expected exactly 3 debug lines: {lines:?}");
    };
    assert!(
        code.starts_with("code: ZVEC_GREP.ENGINE.JSON.READ_FAILED"),
        "{lines:?}"
    );
    assert!(first.contains("wrap: gone"), "{lines:?}");
    assert!(second.contains("gone"), "{lines:?}");
    for pair in lines.windows(2) {
        assert_eq!(pair.len(), 2);
        assert_ne!(pair.first(), pair.get(1), "{lines:?}");
    }
}

#[test]
fn range_labels_match_ts() {
    assert_eq!(
        range_label(&Range::Text {
            start_line: 4,
            end_line: 4,
            start_offset: 0,
            end_offset: 1,
        }),
        "4"
    );
    assert_eq!(
        range_label(&Range::Byte {
            start_offset: 2,
            end_offset: 9,
        }),
        "bytes:2-9"
    );
    assert_eq!(range_label(&Range::Page { page: 3 }), "page:3");
    assert_eq!(range_label(&Range::File), "file");
    assert_eq!(format_score(2.0), "2");
    assert_eq!(format_score(0.123_456), "0.1235");
}

#[test]
fn text_helpers_match_ts() {
    assert_eq!(one_line("  foo\tbar\nbaz  "), "foo bar baz");
    assert_eq!(truncate("abcdef", 10), "abcdef");
    assert_eq!(truncate("abcdef", 5), "abcd…");
    // Char-boundary clip: a byte slice at 4 would split `é`.
    assert_eq!(truncate("héllo", 4), "hél…");
}

#[test]
fn index_result_counters() {
    let result = IndexResult {
        files_scanned: 10,
        files_added: 3,
        files_modified: 1,
        files_unchanged: 5,
        files_failed: 1,
        files_deleted: 2,
        files_pending: 0,
        entities_created: 42,
        duration_ms: 7,
        ..IndexResult::default()
    };
    let text = format_index_result("root", &result);
    assert!(
        text.contains(
            "root: 10 scanned, 3 added, 1 modified, 5 unchanged, 1 failed, 42 entities in 7ms"
        ),
        "{text}"
    );
    assert!(text.contains("deleted: 2"), "{text}");
}

#[test]
fn progress_bar_golden() {
    assert_eq!(format_green_progress_bar(1, 2, 4, false), "██░░ 50% (1/2)");
    assert_eq!(format_green_progress_bar(0, 0, 4, false), "");
}

#[test]
fn workspace_states() {
    let mut info = ZvecGrepInfoResult {
        root: "/root".to_owned(),
        indexed: false,
        ..ZvecGrepInfoResult::default()
    };
    assert_eq!(workspace_state(&info), WorkspaceState::Unindexed);
    info.indexed = true;
    info.status = Some(WorkspaceIndexStatus::default());
    assert_eq!(workspace_state(&info), WorkspaceState::Ready);
    let (text, state) = format_workspace_info(&info, false);
    assert_eq!(state, WorkspaceState::Ready);
    assert!(text.contains("state: ready"), "{text}");
}
