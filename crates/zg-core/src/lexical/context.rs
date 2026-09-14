//! Before/after context expansion for lexical matches.
//!
//! Mirrors `expandContextItem`: the match range widens to whole lines, the
//! original range moves to `excerptRange`, and content becomes the joined
//! window. Whole-file line splits are cached per search.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use super::options::LexicalSearchOptions;

/// Expands a match with before/after context lines, mirroring
/// `expandContextItem`: the range widens to whole lines, the original range
/// moves to `excerptRange`, and content becomes the joined window.
pub(crate) fn expand_context_item(
    item: &mut crate::service::types::ContextItem,
    options: &LexicalSearchOptions,
    cache: &mut HashMap<PathBuf, Option<Vec<String>>>,
) {
    if options.before_context == 0 && options.after_context == 0 {
        return;
    }
    let crate::types::Range::Text {
        start_line,
        end_line,
        ..
    } = item.range
    else {
        return;
    };
    let path = PathBuf::from(&item.file.absolute_path);
    let lines = read_text_lines(&path, cache);
    let Some(lines) = lines else { return };
    if lines.is_empty() {
        return;
    }
    let start = start_line.max(1).min(lines.len() + 1);
    let end = end_line.max(1).min(lines.len());
    let window_start = start.saturating_sub(options.before_context).max(1);
    let window_end = (end + options.after_context).min(lines.len());
    if window_start > window_end {
        return;
    }
    let original = std::mem::replace(
        &mut item.range,
        crate::types::Range::Text {
            start_line: window_start,
            end_line: window_end,
            start_offset: 0,
            end_offset: lines
                .get(window_end - 1)
                .map(|line| line.chars().count())
                .unwrap_or(0),
        },
    );
    item.excerpt_range = Some(original);
    item.content = crate::types::Content::Text {
        text: lines
            .get(window_start - 1..window_end)
            .map(|window| window.join("\n"))
            .unwrap_or_default(),
    };
}

/// Cached whole-file line split, mirroring `readTextLines` (lossy UTF-8,
/// one trailing empty line dropped).
fn read_text_lines(
    path: &Path,
    cache: &mut HashMap<PathBuf, Option<Vec<String>>>,
) -> Option<Vec<String>> {
    if let Some(cached) = cache.get(path) {
        return cached.clone();
    }
    let lines = fs::read(path).ok().map(|bytes| {
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let mut parts: Vec<String> = text
            .split('\n')
            .map(|line| line.strip_suffix('\r').unwrap_or(line).to_owned())
            .collect();
        if parts.last().is_some_and(|last| last.is_empty()) {
            parts.pop();
        }
        parts
    });
    cache.insert(path.to_owned(), lines.clone());
    lines
}
