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
use super::progress::{
    format_green_progress_bar, format_progress_line, format_progress_line_clamped, stderr_width,
    visible_width,
};
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
        coverage_group: None,
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
        zg_core::error::EngineErrorCode::JsonReadFailed,
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
fn progress_line_covers_scan_download_and_indexing() {
    use zg_core::types::{IndexEmbeddingProgress, IndexProgress, IndexProgressPhase};
    let scanning = IndexProgress {
        phase: Some(IndexProgressPhase::Scanning),
        detail: Some("listing files".to_owned()),
        ..IndexProgress::default()
    };
    let line = format_progress_line(&scanning, false, 0);
    assert!(line.contains("scanning"), "{line}");
    assert!(line.contains("listing files"), "{line}");
    let downloading = IndexProgress {
        phase: Some(IndexProgressPhase::Indexing),
        detail: Some("downloading model".to_owned()),
        embedding: Some(IndexEmbeddingProgress {
            downloaded_bytes: Some(5_000_000),
            total_bytes: Some(10_000_000),
            ..IndexEmbeddingProgress::default()
        }),
        ..IndexProgress::default()
    };
    let line = format_progress_line(&downloading, false, 3);
    assert!(line.contains("downloading"), "{line}");
    assert!(line.contains("50%"), "{line}");
    let indexing = IndexProgress {
        phase: Some(IndexProgressPhase::Indexing),
        files_total: Some(10),
        files_indexed: Some(4),
        detail: Some("a.rs".to_owned()),
        ..IndexProgress::default()
    };
    let line = format_progress_line(&indexing, false, 0);
    assert!(line.contains("(4/10)"), "{line}");
    assert!(line.contains("indexing"), "{line}");
    assert!(line.contains("a.rs"), "{line}");
}

#[test]
fn progress_line_clamped_fits_narrow_terminals() {
    use zg_core::types::{IndexEmbeddingProgress, IndexProgress, IndexProgressPhase};
    // Worst case from the wrap report: bar + counts + download counters +
    // long detail (~144 visible chars) on an 80-col terminal.
    let full_detail = "a/very/long/path/that/keeps/going/on/and/on/file.rs";
    let progress = IndexProgress {
        phase: Some(IndexProgressPhase::Indexing),
        files_total: Some(9999),
        files_indexed: Some(9999),
        detail: Some(full_detail.to_owned()),
        embedding: Some(IndexEmbeddingProgress {
            downloaded_bytes: Some(123_400_000),
            total_bytes: Some(456_700_000),
            ..IndexEmbeddingProgress::default()
        }),
        ..IndexProgress::default()
    };
    let line = format_progress_line_clamped(&progress, false, 0, Some(80));
    assert!(visible_width(&line) <= 80, "{line}");
    assert!(line.contains("(9999/9999)"), "{line}");
    assert!(line.contains("downloading"), "{line}");
    assert!(!line.contains(full_detail), "{line}");
    // Narrow terminal: bar drops, phase and counters survive.
    let line = format_progress_line_clamped(&progress, false, 0, Some(40));
    assert!(visible_width(&line) <= 40, "{line}");
    assert!(line.contains("indexing"), "{line}");
    assert!(line.contains("(9999/9999)"), "{line}");
    // Colored clamp keeps escapes balanced and within width.
    let line = format_progress_line_clamped(&progress, true, 0, Some(80));
    assert!(visible_width(&line) <= 80, "{line}");
    assert_eq!(
        line.matches("\x1b[32m").count(),
        line.matches("\x1b[0m").count(),
        "{line}"
    );
    // Wide chars count 2 columns each toward the budget.
    let cjk = IndexProgress {
        phase: Some(IndexProgressPhase::Scanning),
        detail: Some("\u{65e5}\u{672c}\u{8a9e}\u{306e}\u{30d1}\u{30b9}\u{304c}\u{9577}\u{3044}\u{5834}\u{5408}\u{306e}\u{30c6}\u{30b9}\u{30c8}.rs".to_owned()),
        ..IndexProgress::default()
    };
    let line = format_progress_line_clamped(&cjk, false, 0, Some(30));
    assert!(visible_width(&line) <= 30, "{line}");
    assert!(line.contains("scanning"), "{line}");
    // `None` preserves the legacy unclamped layout exactly.
    let indexing = IndexProgress {
        phase: Some(IndexProgressPhase::Indexing),
        files_total: Some(10),
        files_indexed: Some(4),
        detail: Some("a.rs".to_owned()),
        ..IndexProgress::default()
    };
    let legacy = format_progress_line(&indexing, false, 0);
    assert_eq!(
        format_progress_line_clamped(&indexing, false, 0, None),
        legacy
    );
    assert_eq!(legacy, "\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}\u{2591} 40% (4/10) indexing a.rs");
    assert!(stderr_width() >= 20);
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
