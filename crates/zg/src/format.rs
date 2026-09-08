//! Output renderers: hits, ranges, progress, and status (`cli/format/`).
//!
//! Every renderer is a pure `String`-builder covered by goldens; the thin
//! `print_*` wrappers own stdout/stderr. Layouts are idiomatic ports, not
//! byte copies, of `format/context.ts` + `format/status.ts` +
//! `format/progress.ts` + `format/range.ts` (see `docs/ts-divergence.md`):
//! the agent markdown keeps file:range headers, scores, and previews; the
//! human view keeps labeled fields with optional ANSI color.

use zg_core::service::types::{
    ContextCoverage, ContextItem, ContextSource, ZvecGrepContextResult, ZvecGrepInfoResult,
};
use zg_core::types::{Content, IndexResult, Range};

use crate::cli::ColorMode;
use crate::error::CliError;

/// ANSI reset.
const RESET: &str = "\x1b[0m";
/// ANSI dim.
const DIM: &str = "\x1b[2m";
/// ANSI green.
const GREEN: &str = "\x1b[32m";
/// ANSI red.
const RED: &str = "\x1b[31m";
/// ANSI cyan.
const CYAN: &str = "\x1b[36m";

/// True when color output is enabled: `always`, or `auto` on a terminal.
pub fn use_color(mode: Option<ColorMode>, no_color: bool) -> bool {
    if no_color || std::env::var("NO_COLOR").is_ok() {
        return false;
    }
    match mode {
        Some(ColorMode::Always) => true,
        Some(ColorMode::Never) => false,
        Some(ColorMode::Auto) | None => std::io::IsTerminal::is_terminal(&std::io::stdout()),
    }
}

/// Renders a [`Range`] as `start-end`, `bytes:a-b`, `page:N`, or `file`,
/// mirroring `rangeLabel`.
pub fn range_label(range: &Range) -> String {
    match range {
        Range::Text {
            start_line,
            end_line,
            ..
        } => {
            if start_line == end_line {
                start_line.to_string()
            } else {
                format!("{start_line}-{end_line}")
            }
        }
        Range::Byte {
            start_offset,
            end_offset,
        } => format!("bytes:{start_offset}-{end_offset}"),
        Range::Page { page } => format!("page:{page}"),
        Range::PageText { page, .. } => format!("page:{page}"),
        Range::PageRegion { page, .. } => format!("page:{page}"),
        Range::File => "file".to_owned(),
    }
}

/// Formats a score like the TS agent view: integers plain, else 4dp.
pub fn format_score(score: f64) -> String {
    if score.fract() == 0.0 && score.is_finite() {
        format!("{}", score.trunc() as i64)
    } else {
        format!("{score:.4}")
    }
}

/// Collapses whitespace runs, mirroring `oneLine`.
pub fn one_line(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Clips to `max` chars with an ellipsis, mirroring `truncate`.
pub fn truncate(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        return value.to_owned();
    }
    let clipped: String = value.chars().take(max.saturating_sub(1)).collect();
    format!("{clipped}…")
}

/// Renders a context result as agent markdown (the default CLI layout).
///
/// Ranked file:range headers with score/provenance, then content
/// previews; empty results name the query and the reason.
pub fn format_context_agent(result: &ZvecGrepContextResult) -> String {
    let mut lines = Vec::new();
    if result.items.is_empty() {
        lines.push(format!("No results for \"{}\".", result.query));
        lines.extend(empty_detail_lines(result));
        return lines.join("\n");
    }
    lines.push(format!(
        "# {} result{} for \"{}\"",
        result.items.len(),
        if result.items.len() == 1 { "" } else { "s" },
        result.query
    ));
    for item in &result.items {
        lines.push(String::new());
        lines.push(agent_item_header(item));
        lines.extend(agent_item_body(item));
    }
    lines.join("\n")
}

/// Renders a context result for humans: labeled fields, optional color.
pub fn format_context_human(result: &ZvecGrepContextResult, color: bool) -> String {
    let mut lines = Vec::new();
    if result.items.is_empty() {
        lines.push(human_field("query", &result.query, color));
        lines.push(human_field("results", "0", color));
        lines.extend(empty_detail_lines(result));
        return lines.join("\n");
    }
    lines.push(human_field("query", &result.query, color));
    lines.push(human_field(
        "results",
        &result.items.len().to_string(),
        color,
    ));
    lines.push(human_field("source", &source_label(&result.source), color));
    for item in &result.items {
        lines.push(String::new());
        lines.push(human_field(
            "file",
            &format!("{}:{}", item.file.relative_path, range_label(&item.range)),
            color,
        ));
        if let Some(score) = item.score {
            lines.push(human_field("score", &format_score(score), color));
        }
        lines.extend(human_preview_lines(item, PreviewLines::Full, color));
    }
    lines.join("\n")
}

/// Prints the CLI query layout: human when `--human`, agent otherwise.
pub fn print_context_result(result: &ZvecGrepContextResult, human: bool, color: bool) {
    if human {
        println!("{}", format_context_human(result, color));
    } else {
        println!("{}", format_context_agent(result));
    }
}

/// Prints context warnings (empty reasons) to stderr.
pub fn print_context_warnings(result: &ZvecGrepContextResult) {
    for line in empty_detail_lines(result) {
        eprintln!("{line}");
    }
}

/// Workspace index state for `status`, mirroring `WorkspaceIndexState`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceState {
    /// Fresh and searchable.
    Ready,
    /// Behind the filesystem.
    Stale,
    /// Latest run failed.
    Failed,
    /// Indexing disabled by policy.
    Disabled,
    /// No index yet.
    Unindexed,
    /// No status recorded.
    Undecided,
}

impl WorkspaceState {
    /// Wire string for messages and `--check-ready` output.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Stale => "stale",
            Self::Failed => "failed",
            Self::Disabled => "disabled",
            Self::Unindexed => "unindexed",
            Self::Undecided => "undecided",
        }
    }
}

/// Derives the workspace state from an info result.
pub fn workspace_state(info: &ZvecGrepInfoResult) -> WorkspaceState {
    if info.index_policy == Some(zg_core::types::WorkspaceIndexPolicy::Disabled) {
        return WorkspaceState::Disabled;
    }
    if !info.indexed {
        return WorkspaceState::Unindexed;
    }
    let Some(status) = &info.status else {
        return WorkspaceState::Undecided;
    };
    if !status.failed_files.is_empty() {
        return WorkspaceState::Failed;
    }
    if status.files_pending > 0 || !status.pending_files.is_empty() {
        return WorkspaceState::Stale;
    }
    WorkspaceState::Ready
}

/// Renders workspace info as labeled lines; returns the state for
/// `--check-ready`.
pub fn format_workspace_info(info: &ZvecGrepInfoResult, color: bool) -> (String, WorkspaceState) {
    let state = workspace_state(info);
    let mut lines = vec![
        status_field("root", &info.root, color),
        status_field("state", state.as_str(), color),
    ];
    if let Some(embedding) = &info.embedding {
        let metric = format!("{:?}", embedding.metric);
        lines.push(status_field(
            "embedding",
            &format!(
                "{}/{} (dim {}, {metric})",
                embedding.provider, embedding.model, embedding.dimension
            ),
            color,
        ));
    }
    if let Some(status) = &info.status {
        lines.push(status_field(
            "files",
            &format!(
                "{} stored, {} entities",
                status.files_stored, status.entities_indexed
            ),
            color,
        ));
        if status.files_failed > 0 {
            lines.push(status_field(
                "failed",
                &status.files_failed.to_string(),
                color,
            ));
        }
        if status.files_pending > 0 {
            lines.push(status_field(
                "pending",
                &status.files_pending.to_string(),
                color,
            ));
        }
    }
    if let Some(suggestion) = &info.suggestion {
        lines.push(status_field("suggestion", suggestion, color));
    }
    (lines.join("\n"), state)
}

/// Prints workspace info; returns the state.
pub fn print_workspace_info(info: &ZvecGrepInfoResult, color: bool) -> WorkspaceState {
    let (text, state) = format_workspace_info(info, color);
    println!("{text}");
    state
}

/// Renders an index result, mirroring `printIndexResult` counters.
pub fn format_index_result(label: &str, result: &IndexResult) -> String {
    let mut lines = vec![format!(
        "{label}: {} scanned, {} added, {} modified, {} unchanged, {} failed, {} entities in {}ms",
        result.files_scanned,
        result.files_added,
        result.files_modified,
        result.files_unchanged,
        result.files_failed,
        result.entities_created,
        result.duration_ms
    )];
    if result.files_deleted > 0 {
        lines.push(format!("deleted: {}", result.files_deleted));
    }
    if result.files_pending > 0 {
        lines.push(format!("pending: {}", result.files_pending));
    }
    lines.join("\n")
}

/// Prints an index result.
pub fn print_index_result(label: &str, result: &IndexResult) {
    println!("{}", format_index_result(label, result));
}

/// Prints the no-indexable-files tip.
pub fn print_no_indexable_files_tip() {
    eprintln!(
        "No indexable files found. Adjust --glob/--type filters or check --hidden/--no-ignore."
    );
}

/// Renders a green progress bar, mirroring `formatGreenProgressBar`.
pub fn format_green_progress_bar(
    completed: usize,
    total: usize,
    width: usize,
    color: bool,
) -> String {
    if total == 0 || width == 0 {
        return String::new();
    }
    let ratio = (completed.min(total) as f64) / (total as f64);
    let filled = (ratio * width as f64).round() as usize;
    let bar: String = std::iter::repeat_n('█', filled.min(width)).collect::<String>()
        + &std::iter::repeat_n('░', width.saturating_sub(filled)).collect::<String>();
    let percent = (ratio * 100.0).round() as usize;
    if color {
        format!("{GREEN}{bar}{RESET} {percent}% ({completed}/{total})")
    } else {
        format!("{bar} {percent}% ({completed}/{total})")
    }
}

/// Minimal stderr progress reporter: TTY bar plus throttled plain lines.
pub struct ProgressReporter {
    color: bool,
    last_line: std::time::Instant,
}

impl ProgressReporter {
    /// Builds a reporter; color follows the CLI color selection.
    pub fn new(color: bool) -> Self {
        Self {
            color,
            last_line: std::time::Instant::now() - std::time::Duration::from_secs(60),
        }
    }

    /// Reports one progress event.
    pub fn report(&mut self, progress: &zg_core::types::IndexProgress) {
        let (done, total) = (
            progress.files_indexed.unwrap_or(0),
            progress.files_total.unwrap_or(0),
        );
        if total == 0 {
            return;
        }
        if std::io::IsTerminal::is_terminal(&std::io::stderr()) {
            eprint!(
                "\r\x1b[2K{}",
                format_green_progress_bar(done, total, 20, self.color)
            );
        } else if self.last_line.elapsed() >= std::time::Duration::from_secs(15) {
            eprintln!(
                "indexing: {done}/{total} files{}",
                progress
                    .detail
                    .as_deref()
                    .map_or_else(String::new, |detail| format!(" ({detail})"))
            );
            self.last_line = std::time::Instant::now();
        }
    }

    /// Clears the TTY line (no-op off-TTY).
    pub fn finish(&self) {
        if std::io::IsTerminal::is_terminal(&std::io::stderr()) {
            eprint!("\r\x1b[2K");
        }
    }
}

/// Prints a CLI error like `printError`: red `error:` prefix on color
/// terminals, the message, and the wire code with `--debug`.
pub fn print_error(error: &CliError, color: bool, debug: bool) {
    if color {
        eprintln!("{RED}error:{RESET} {error}");
    } else {
        eprintln!("error: {error}");
    }
    if debug {
        eprintln!("{DIM}code: {}{RESET}", error.code());
        let mut source = std::error::Error::source(error);
        while let Some(next) = source {
            eprintln!("{DIM}caused by: {next}{RESET}");
            source = std::error::Error::source(next);
        }
    }
}

/// Prints daemon control status (`server on|off|status`).
pub fn print_control_status(running: bool, ready: bool, pid: Option<u32>, url: Option<&str>) {
    println!("running: {}", if running { "yes" } else { "no" });
    println!("ready: {}", if ready { "yes" } else { "no" });
    if let Some(pid) = pid {
        println!("pid: {pid}");
    }
    if let Some(url) = url {
        println!("url: {url}");
    }
}

fn agent_item_header(item: &ContextItem) -> String {
    let mut header = format!(
        "## {}. {}:{}",
        item.rank,
        item.file.relative_path,
        range_label(&item.range)
    );
    let mut meta = Vec::new();
    if let Some(score) = item.score {
        meta.push(format!("score {}", format_score(score)));
    }
    if let Some(matched_by) = &item.matched_by {
        meta.push(format!("via {matched_by}"));
    }
    if !matches!(item.status, zg_core::service::types::ContentStatus::Fresh) {
        meta.push("possibly stale".to_owned());
    }
    if !meta.is_empty() {
        header.push_str(&format!(" ({})", meta.join(", ")));
    }
    header
}

fn agent_item_body(item: &ContextItem) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(symbol) = agent_symbol_line(item) {
        lines.push(symbol);
    }
    lines.extend(agent_preview_lines(item));
    lines
}

fn agent_symbol_line(item: &ContextItem) -> Option<String> {
    let metadata = item.metadata.as_ref()?;
    let debug = format!("{metadata:?}");
    Some(format!("symbol: {}", one_line(&debug)))
}

fn agent_preview_lines(item: &ContextItem) -> Vec<String> {
    match &item.content {
        Content::Text { text } => text
            .lines()
            .take(10)
            .map(|line| truncate(line.trim_end(), 160))
            .collect(),
        Content::Image { .. } => vec!["[image content]".to_owned()],
    }
}

fn empty_detail_lines(result: &ZvecGrepContextResult) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(reason) = &result.diagnostics.empty_reason {
        lines.push(reason.clone());
    }
    if result.source == ContextSource::Index && result.coverage == ContextCoverage::RankedSample {
        lines.push("Try --rg for exhaustive lexical search.".to_owned());
    }
    lines
}

/// Preview window for human rendering.
#[derive(Debug, Clone, Copy)]
enum PreviewLines {
    /// Full content.
    Full,
}

fn human_preview_lines(item: &ContextItem, _window: PreviewLines, color: bool) -> Vec<String> {
    let paint = |line: &str| {
        if color {
            format!("{DIM}{line}{RESET}")
        } else {
            line.to_owned()
        }
    };
    match &item.content {
        Content::Text { text } => text.lines().take(10).map(paint).collect(),
        Content::Image { .. } => vec![paint("[image content]")],
    }
}

fn human_field(label: &str, value: &str, color: bool) -> String {
    if color {
        format!("{CYAN}{label}:{RESET} {value}")
    } else {
        format!("{label}: {value}")
    }
}

fn status_field(label: &str, value: &str, color: bool) -> String {
    human_field(label, value, color)
}

fn source_label(source: &ContextSource) -> String {
    match source {
        ContextSource::Index => "index".to_owned(),
        ContextSource::Rg => "rg".to_owned(),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use zg_core::ids::EntityId;
    use zg_core::service::types::{
        ContentStatus, ContextDiagnostics, ContextFile, ContextItemKind,
    };
    use zg_core::types::WorkspaceIndexStatus;

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
}
