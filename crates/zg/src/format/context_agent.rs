//! Agent markdown renderer (the default CLI layout).
//!
//! Ranked file:range headers with score/provenance, then content previews;
//! empty results name the query and the reason.

use zg_core::service::types::{ContentStatus, ContextItem, ZvecGrepContextResult};
use zg_core::types::Content;

use super::context_shared::empty_detail_lines;
use super::range::range_label;
use super::text::{format_score, one_line, truncate};

/// Renders a context result as agent markdown (the default CLI layout).
///
/// Ranked file:range headers with score/provenance, then content
/// previews; empty results name the query and the reason.
#[must_use]
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
    if !matches!(item.status, ContentStatus::Fresh) {
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
