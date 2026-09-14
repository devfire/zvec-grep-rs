//! MCP result rendering: tool results and the compact agent text format.
//!
//! Mirrors `../zvec-grep/src/mcp/result-format.ts` (`textToolResult`,
//! `toolResult`) plus the agent short-preview rendering the search/rg
//! tools use (`formatAgentContextResult(result, { preview: "short" })`
//! from `../zvec-grep/src/cli/format/context.ts`). The full CLI format
//! surface (symbol blocks, highlighters, human layout) belongs to phase I;
//! this module renders the bounded short form: ranked headers, group
//! lines, clipped outlines, and a 10-line anchored source window.

use rmcp::model::{CallToolResult, Content as McpContent};
use zg_core::service::types::{ContextItem, ContextSource, ZvecGrepContextResult};
use zg_core::types::{Content, Range};

/// Short-preview window: at most 10 source lines…
pub const SHORT_SOURCE_MAX_LINES: usize = 10;
/// …with 2 lines of context before the excerpt anchor.
pub const SHORT_SOURCE_CONTEXT_BEFORE: usize = 2;
/// Outline preview is clipped to 7 lines.
pub const SHORT_OUTLINE_MAX_LINES: usize = 7;
/// Source lines are truncated to 160 columns.
pub const AGENT_PREVIEW_MAX_LINE_LENGTH: usize = 160;

/// Text-only tool result.
#[must_use]
pub fn text_tool_result(text: String) -> CallToolResult {
    CallToolResult::success(vec![McpContent::text(text)])
}

/// Tool result with text and structured content.
#[must_use]
pub fn tool_result(text: String, structured: serde_json::Value) -> CallToolResult {
    let mut result = CallToolResult::structured(structured);
    result.content = vec![McpContent::text(text)];
    result
}

/// Compact agent rendering of a context result. `trace` controls the
/// `score=` header suffix and `trace:` detail lines (mirrors the
/// `options.trace` flag, not the per-item payload presence).
#[must_use]
pub fn format_agent_context_result(result: &ZvecGrepContextResult, trace: bool) -> String {
    if result.source == ContextSource::Rg || result.group_results.is_none() {
        let refs: Vec<&ContextItem> = result.items.iter().collect();
        return agent_item_lines(result, &refs, trace).join("\n");
    }
    let mut lines = Vec::new();
    let groups = result.group_results.as_deref().unwrap_or(&[]);
    lines.push(format!("query groups ({}):", groups.len()));
    for (index, group) in groups.iter().enumerate() {
        if index > 0 {
            lines.push(String::new());
        }
        let role = group
            .role
            .map(|role| format!("{role:?}").to_lowercase())
            .unwrap_or_else(|| "primary".to_owned());
        lines.push(format!("{} [{role}]: {}", group.id, one_line(&group.query)));
        let grouped: Vec<&ContextItem> = result
            .items
            .iter()
            .filter(|item| {
                item.query_groups
                    .iter()
                    .any(|reference| reference.id == group.id)
            })
            .collect();
        lines.push(format!("hits: {}", grouped.len()));
        if !grouped.is_empty() {
            lines.push(String::new());
            lines.extend(agent_item_lines(result, &grouped, trace));
        }
    }
    lines.join("\n")
}

/// Flat item rendering with empty-result labels.
fn agent_item_lines(
    result: &ZvecGrepContextResult,
    items: &[&ContextItem],
    trace: bool,
) -> Vec<String> {
    if items.is_empty() {
        return vec![empty_context_label(result)];
    }
    let mut ordered: Vec<&ContextItem> = items.to_vec();
    ordered.sort_by(|left, right| compare_items(left, right));
    let mut lines = Vec::new();
    // Query-group header when several groups contributed (mirrors the
    // `query groups (N):` selection preamble).
    let mut group_ids: Vec<&str> = ordered
        .iter()
        .flat_map(|item| item.query_groups.iter().map(|group| group.id.as_str()))
        .collect();
    group_ids.sort_unstable();
    group_ids.dedup();
    if group_ids.len() > 1 {
        lines.push(format!("query groups ({}):", group_ids.len()));
        for id in &group_ids {
            lines.push(format!("  {id}"));
        }
        lines.push(
            "selection: primary-group coverage then global_fill; prioritized<=6; all candidates detailed"
                .to_owned(),
        );
        lines.push(String::new());
    }
    let mut first = true;
    for item in ordered {
        if !first {
            lines.push(String::new());
        }
        first = false;
        lines.push(item_header(item, trace));
        if let Some(groups) = query_group_line(item) {
            lines.push(groups);
        }
        if let Some(matched) = matched_range_line(item) {
            lines.push(matched);
        }
        lines.extend(outline_lines(item));
        if item.status == zg_core::service::types::ContentStatus::PossiblyStale {
            lines.push("status: possibly_stale".to_owned());
        }
        let source = source_lines(item);
        if !source.is_empty() {
            if item.kind != zg_core::service::types::ContextItemKind::RgMatch {
                lines.push("source:".to_owned());
            }
            lines.extend(source);
        }
        if trace && let Some(detail) = item.trace.as_ref().and_then(trace_detail_line) {
            lines.push(format!("trace: {detail}"));
        }
    }
    lines
}

fn compare_items(left: &ContextItem, right: &ContextItem) -> std::cmp::Ordering {
    left.rank
        .cmp(&right.rank)
        .then_with(|| range_start_line(&left.range).cmp(&range_start_line(&right.range)))
        .then_with(|| range_label(&left.range).cmp(&range_label(&right.range)))
}

fn range_start_line(range: &Range) -> usize {
    match range {
        Range::Text { start_line, .. } => *start_line,
        Range::File
        | Range::Byte { .. }
        | Range::Page { .. }
        | Range::PageText { .. }
        | Range::PageRegion { .. } => 0,
    }
}

/// `#rank[selection] matchedBy=...[ score=...] file:range`.
fn item_header(item: &ContextItem, trace: bool) -> String {
    let selection = match item.selection_reason {
        Some(zg_core::service::types::SelectionReason::Coverage) => " [group_coverage]",
        Some(zg_core::service::types::SelectionReason::GlobalFill) => " [global_fill]",
        None => "",
    };
    let score = if trace {
        item.score
            .map(|score| format!(" score={}", format_score(score)))
            .unwrap_or_default()
    } else {
        String::new()
    };
    let matched_by = item.matched_by.as_deref().unwrap_or("unknown");
    let range = match item.container.as_ref() {
        Some(container) => &container.range,
        None => &item.range,
    };
    format!(
        "#{}{selection} matchedBy={matched_by}{score} {}:{}",
        item.rank,
        item.file.relative_path,
        range_label(range)
    )
}

fn format_score(score: f64) -> String {
    if score.fract() == 0.0 && score.is_finite() {
        format!("{}", score as i64)
    } else {
        format!("{score:.4}")
    }
}

/// `groups: id#rank, …` (the engine tracks group id and rank per item;
/// per-group `matchedBy` is a CLI-transport detail from phase I).
fn query_group_line(item: &ContextItem) -> Option<String> {
    if item.query_groups.is_empty() {
        return None;
    }
    Some(format!(
        "groups: {}",
        item.query_groups
            .iter()
            .map(|group| format!("{}#{}", group.id, group.rank))
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

/// `matched: …` when the excerpt range differs from the item range.
fn matched_range_line(item: &ContextItem) -> Option<String> {
    if item.kind == zg_core::service::types::ContextItemKind::RgMatch {
        return None;
    }
    let excerpt = item.excerpt_range.as_ref()?;
    if range_label(excerpt) == range_label(&item.range) {
        return None;
    }
    Some(format!("matched: {}", range_label(excerpt)))
}

/// `outline:` plus up to 7 outline lines.
fn outline_lines(item: &ContextItem) -> Vec<String> {
    let outline = match item.content_role {
        Some(zg_core::service::types::ContentRole::Outline) => item.content.as_text(),
        _ => item.outline.as_deref(),
    };
    let Some(outline) = outline else {
        return Vec::new();
    };
    let mut lines: Vec<String> = outline.lines().map(str::to_owned).collect();
    if lines.len() > SHORT_OUTLINE_MAX_LINES {
        lines.truncate(SHORT_OUTLINE_MAX_LINES);
        lines.push("...".to_owned());
    }
    if lines.is_empty() {
        return Vec::new();
    }
    let mut rendered = vec!["outline:".to_owned()];
    rendered.extend(lines);
    rendered
}

/// Source preview: full numbered lines for rg matches, a 10-line anchored
/// window otherwise.
fn source_lines(item: &ContextItem) -> Vec<String> {
    let Some(text) = item.content.as_text() else {
        if matches!(item.content, Content::Image { .. }) {
            return vec!["[non-text content]".to_owned()];
        }
        return Vec::new();
    };
    if text.is_empty() {
        return Vec::new();
    }
    if item.kind == zg_core::service::types::ContextItemKind::RgMatch {
        return source_entries(item, text)
            .iter()
            .map(|entry| {
                format!(
                    "  {}",
                    format_source_line(entry, None, line_marker(item, entry))
                )
            })
            .collect();
    }
    short_window(item, text)
        .iter()
        .map(|line| match line {
            WindowLine::Ellipsis => "...".to_owned(),
            WindowLine::Entry(entry) => {
                format_source_line(entry, Some(AGENT_PREVIEW_MAX_LINE_LENGTH), None)
            }
        })
        .collect()
}

/// Numbered content lines with file-anchored line numbers.
fn source_entries<'a>(item: &ContextItem, text: &'a str) -> Vec<SourceEntry<'a>> {
    let start = content_start_line(item, text.lines().count());
    text.lines()
        .enumerate()
        .map(|(index, line)| SourceEntry {
            number: start.map(|first| first + index),
            text: line,
        })
        .collect()
}

/// First content line number: excerpt range wins for short excerpts
/// (mirrors `contentRangeForItem`).
fn content_start_line(item: &ContextItem, line_count: usize) -> Option<usize> {
    if item.kind == zg_core::service::types::ContextItemKind::RgMatch {
        return text_start(&item.range);
    }
    let excerpt = item.excerpt_range.as_ref()?;
    let (excerpt_start, excerpt_end) = text_span(excerpt)?;
    let (range_start, range_end) = text_span(&item.range)?;
    let entity_lines = range_end - range_start + 1;
    let excerpt_lines = excerpt_end - excerpt_start + 1;
    if line_count <= excerpt_lines + 2 && line_count < entity_lines {
        Some(excerpt_start)
    } else {
        text_start(&item.range)
    }
}

fn text_start(range: &Range) -> Option<usize> {
    text_span(range).map(|(start, _)| start)
}

fn text_span(range: &Range) -> Option<(usize, usize)> {
    match range {
        Range::Text {
            start_line,
            end_line,
            ..
        } => Some((*start_line, *end_line)),
        Range::File
        | Range::Byte { .. }
        | Range::Page { .. }
        | Range::PageText { .. }
        | Range::PageRegion { .. } => None,
    }
}

struct SourceEntry<'a> {
    number: Option<usize>,
    text: &'a str,
}

enum WindowLine<'a> {
    Ellipsis,
    Entry(SourceEntry<'a>),
}

/// 10-line window anchored at the excerpt (mirrors
/// `sourceWindowEntries`).
fn short_window<'a>(item: &ContextItem, text: &'a str) -> Vec<WindowLine<'a>> {
    let entries = source_entries(item, text);
    if entries.len() <= SHORT_SOURCE_MAX_LINES {
        return entries.into_iter().map(WindowLine::Entry).collect();
    }
    let anchor = anchor_index(item, &entries);
    let excerpt_lines = item
        .excerpt_range
        .as_ref()
        .and_then(text_span)
        .map(|(start, end)| end - start + 1)
        .unwrap_or(1);
    let before = if excerpt_lines >= SHORT_SOURCE_MAX_LINES {
        0
    } else {
        SHORT_SOURCE_CONTEXT_BEFORE
    };
    let mut start = anchor.saturating_sub(before);
    if start + SHORT_SOURCE_MAX_LINES > entries.len() {
        start = entries.len() - SHORT_SOURCE_MAX_LINES;
    }
    let end = (start + SHORT_SOURCE_MAX_LINES).min(entries.len());
    let mut window = Vec::new();
    if start > 0 {
        window.push(WindowLine::Ellipsis);
    }
    window.extend(
        entries
            .into_iter()
            .skip(start)
            .take(end - start)
            .map(WindowLine::Entry),
    );
    if end < window_len(text) {
        window.push(WindowLine::Ellipsis);
    }
    window
}

fn window_len(text: &str) -> usize {
    text.lines().count()
}

/// First entry at or after the excerpt start (mirrors
/// `sourceAnchorIndex`).
fn anchor_index(item: &ContextItem, entries: &[SourceEntry<'_>]) -> usize {
    let Some(excerpt) = item.excerpt_range.as_ref() else {
        return 0;
    };
    let Range::Text { start_line, .. } = excerpt else {
        return 0;
    };
    entries
        .iter()
        .position(|entry| entry.number.is_some_and(|number| number >= *start_line))
        .unwrap_or(0)
}

/// `213:\ttext` with `:` inside the match span, `-` outside (lexical
/// matches only); otherwise `213\ttext`.
fn line_marker(item: &ContextItem, entry: &SourceEntry<'_>) -> Option<char> {
    if item.kind != zg_core::service::types::ContextItemKind::RgMatch {
        return None;
    }
    let number = entry.number?;
    let span = text_span(item.excerpt_range.as_ref().unwrap_or(&item.range))?;
    Some(if number >= span.0 && number <= span.1 {
        ':'
    } else {
        '-'
    })
}

fn format_source_line(
    entry: &SourceEntry<'_>,
    max_length: Option<usize>,
    marker: Option<char>,
) -> String {
    let text = match max_length {
        Some(max) => truncate(entry.text, max),
        None => entry.text.to_owned(),
    };
    let prefix = entry
        .number
        .map(|number| number.to_string())
        .unwrap_or_default();
    match marker {
        Some(marker) => format!("{prefix}{marker}\t{text}"),
        None => format!("{prefix}\t{text}"),
    }
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    // Byte-boundary-safe truncation (mirrors TS `slice`, which can split
    // graphemes but never panics; Rust must not split UTF-8).
    let end = text
        .char_indices()
        .take(max)
        .last()
        .map(|(index, char)| index + char.len_utf8())
        .unwrap_or(0);
    text[..end].to_owned()
}

fn one_line(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `file:start-end` labels (mirrors `cli/format/range.ts`).
#[must_use]
pub fn range_label(range: &Range) -> String {
    match range {
        Range::File => "file".to_owned(),
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
            ..
        } => format!("bytes:{start_offset}-{end_offset}"),
        Range::Page { page, .. } => format!("page:{page}"),
        Range::PageText { page, .. } => format!("page:{page}"),
        Range::PageRegion { page, .. } => format!("page:{page}"),
    }
}

fn empty_context_label(result: &ZvecGrepContextResult) -> String {
    match result.diagnostics.empty_reason.as_deref() {
        Some("no_searchable_files") => "No searchable files.".to_owned(),
        _ => "No matches.".to_owned(),
    }
}

/// Trace detail: the trace payload is engine-opaque JSON, rendered
/// compactly on one line.
fn trace_detail_line(trace: &serde_json::Value) -> Option<String> {
    match trace {
        serde_json::Value::Null => None,
        serde_json::Value::Bool(_)
        | serde_json::Value::Number(_)
        | serde_json::Value::String(_)
        | serde_json::Value::Array(_)
        | serde_json::Value::Object(_) => serde_json::to_string(trace).ok(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zg_core::service::types::{
        ContentRole, ContentStatus, ContextCoverage, ContextDiagnostics, ContextFile,
        ContextItemKind, ContextSource, GroupResult, GroupRole, QueryGroupRef,
    };
    use zg_core::types::SearchMatchedBy;

    fn item(rank: usize, text: &str) -> ContextItem {
        ContextItem {
            kind: ContextItemKind::IndexedEntity,
            rank,
            file: ContextFile {
                absolute_path: "/repo/a.rs".to_owned(),
                relative_path: "a.rs".to_owned(),
                root_path: "/repo".to_owned(),
            },
            range: Range::Text {
                start_line: 10,
                end_line: 12,
                start_offset: 0,
                end_offset: 30,
            },
            excerpt_range: None,
            content: Content::Text {
                text: text.to_owned(),
            },
            content_role: Some(ContentRole::Source),
            outline: None,
            status: ContentStatus::Fresh,
            score: Some(0.75),
            matched_by: Some("vector".to_owned()),
            metadata: None,
            entity_id: None,
            trace: None,
            query_groups: Vec::new(),
            container: None,
            selection_reason: None,
            coverage_group: None,
        }
    }

    fn result(items: Vec<ContextItem>) -> ZvecGrepContextResult {
        ZvecGrepContextResult {
            query: "alpha".to_owned(),
            root: "/repo".to_owned(),
            source: ContextSource::Index,
            coverage: ContextCoverage::RankedSample,
            workspace_index: None,
            items,
            group_results: None,
            diagnostics: ContextDiagnostics::default(),
        }
    }

    #[test]
    fn renders_ranked_headers_with_short_preview() {
        let text = format_agent_context_result(
            &result(vec![item(1, "fn alpha() {}\nfn beta() {}\n")]),
            false,
        );
        assert!(text.contains("#1 matchedBy=vector a.rs:10-12"), "{text}");
        assert!(text.contains("source:"), "{text}");
        assert!(!text.contains("score="), "{text}");
    }

    #[test]
    fn trace_flag_adds_scores() {
        let text = format_agent_context_result(&result(vec![item(2, "fn alpha() {}\n")]), true);
        assert!(text.contains("score=0.75"), "{text}");
    }

    #[test]
    fn empty_results_get_labels() {
        assert_eq!(
            format_agent_context_result(&result(Vec::new()), false),
            "No matches."
        );
    }

    #[test]
    fn range_labels_match_cli_shapes() {
        assert_eq!(
            range_label(&Range::Text {
                start_line: 7,
                end_line: 7,
                start_offset: 0,
                end_offset: 1,
            }),
            "7"
        );
        assert_eq!(range_label(&Range::File), "file");
    }

    #[test]
    fn grouped_results_render_items_per_group() {
        let mut first = item(1, "fn alpha() {}\n");
        first.query_groups = vec![QueryGroupRef {
            id: "Q1".to_owned(),
            query: "alpha".to_owned(),
            role: GroupRole::Primary,
            rank: 1,
            matched_by: SearchMatchedBy::Fts,
        }];
        let mut second = item(2, "fn beta() {}\n");
        second.query_groups = vec![QueryGroupRef {
            id: "Q2".to_owned(),
            query: "beta".to_owned(),
            role: GroupRole::Supplemental,
            rank: 1,
            matched_by: SearchMatchedBy::Vector,
        }];
        let mut grouped = result(vec![first, second]);
        grouped.group_results = Some(vec![
            GroupResult {
                id: "Q1".to_owned(),
                query: "alpha".to_owned(),
                role: Some(GroupRole::Primary),
                items: Vec::new(),
                timings: None,
            },
            GroupResult {
                id: "Q2".to_owned(),
                query: "beta".to_owned(),
                role: Some(GroupRole::Supplemental),
                items: Vec::new(),
                timings: None,
            },
        ]);
        let text = format_agent_context_result(&grouped, false);
        assert!(text.contains("query groups (2):"), "{text}");
        assert!(text.contains("Q1 [primary]: alpha"), "{text}");
        assert!(text.contains("Q2 [supplemental]: beta"), "{text}");
        assert!(text.contains("hits: 1"), "{text}");
        assert!(text.contains("#1 matchedBy=vector a.rs:10-12"), "{text}");
        assert!(text.contains("#2 matchedBy=vector a.rs:10-12"), "{text}");
    }
}
