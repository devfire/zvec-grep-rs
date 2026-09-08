//! Human renderer: labeled fields with optional ANSI color.
//!
//! The single-variant `PreviewLines` switch from the monolith is gone:
//! the window was always `Full` and the parameter ignored, so the preview
//! helper takes just the item and the color flag.

use zg_core::service::types::{ContextItem, ZvecGrepContextResult};
use zg_core::types::Content;

use super::color::{DIM, RESET};
use super::context_shared::{empty_detail_lines, source_label};
use super::fields::human_field;
use super::range::range_label;
use super::text::format_score;

/// Renders a context result for humans: labeled fields, optional color.
#[must_use]
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
        lines.extend(human_preview_lines(item, color));
    }
    lines.join("\n")
}

/// Dimmed content preview (up to 10 lines).
fn human_preview_lines(item: &ContextItem, color: bool) -> Vec<String> {
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
