//! Chunking: statement-aware splitting, line/char windows, overlap math.

use crate::extraction::code::adapter::SyntaxNode;
use crate::types::Range;

pub(crate) struct CodeWindow {
    pub(crate) text: String,
    pub(crate) embedding_text: Option<String>,
    pub(crate) range: Range,
}

pub(crate) fn split_large_node(
    node: &SyntaxNode<'_>,
    max_chars: usize,
    overlap_chars: usize,
) -> Vec<CodeWindow> {
    let body = node.field("body").unwrap_or(*node);
    let statements = body.named_children();
    if statements.len() <= 1 {
        let text = node.text().unwrap_or("");
        return split_text_by_lines(
            text,
            max_chars,
            node.start_row() + 1,
            node.start_byte(),
            overlap_chars,
        );
    }

    let base_start = node.start_byte();
    let source = node.text().unwrap_or("");
    // Single precompute per input: one `chars().count()` per statement, then
    // O(1) window sums via `stmt_prefix` for the grouping loop, overlap
    // back-scan, and `compute_overlap_start` below. No recounts in loops.
    let stmt_lens = statement_char_lengths(&statements);
    let stmt_prefix = prefix_sums_with_separator(&stmt_lens);
    let mut out = Vec::new();
    let mut group_start = 0usize;
    let mut group_chars = 0usize;

    for (index, statement) in statements.iter().enumerate() {
        let statement_chars = stmt_lens.get(index).copied().unwrap_or(0);
        let statement_text = statement.text().unwrap_or("");
        if statement_chars > max_chars {
            if index > group_start
                && let Some(window) = statements.get(group_start..index)
                && let Some(fragment) = slice_statements(source, base_start, window)
            {
                out.push(fragment);
            }
            out.extend(split_text_by_lines(
                statement_text,
                max_chars,
                statement.start_row() + 1,
                statement.start_byte(),
                overlap_chars,
            ));
            group_start = index + 1;
            group_chars = 0;
            continue;
        }
        let separator_chars = usize::from(index > group_start);
        if group_chars + separator_chars + statement_chars > max_chars && index > group_start {
            if let Some(window) = statements.get(group_start..index)
                && let Some(fragment) = slice_statements(source, base_start, window)
            {
                out.push(fragment);
            }
            let overlap_start = compute_overlap_start(
                &stmt_lens,
                &stmt_prefix,
                group_start,
                index - 1,
                overlap_chars,
            );
            let bounded_start = overlap_start.min(index);
            // Grow the carried overlap window backwards while it fits, using
            // O(1) prefix sums instead of recounting statement text.
            let mut candidate_start = index;
            for prev in (bounded_start..index).rev() {
                let window = window_chars(&stmt_prefix, prev, index);
                let total = window.saturating_add(1).saturating_add(statement_chars);
                if total > max_chars {
                    break;
                }
                candidate_start = prev;
            }
            let carried = if candidate_start >= index {
                statement_chars
            } else {
                window_chars(&stmt_prefix, candidate_start, index)
                    .saturating_add(1)
                    .saturating_add(statement_chars)
            };
            group_start = candidate_start;
            group_chars = carried;
            continue;
        }
        group_chars += separator_chars + statement_chars;
    }

    if group_start < statements.len()
        && let Some(window) = statements.get(group_start..)
        && let Some(fragment) = slice_statements(source, base_start, window)
    {
        out.push(fragment);
    }
    out
}

fn slice_statements(
    source: &str,
    base_start: usize,
    window: &[SyntaxNode<'_>],
) -> Option<CodeWindow> {
    let first = window.first()?;
    let last = window.last()?;
    let start = first.start_byte();
    let end = last.end_byte();
    let text = slice_bytes(
        source,
        start.saturating_sub(base_start),
        end.saturating_sub(base_start),
    )
    .to_owned();
    let embedding_text = window
        .iter()
        .map(|statement| statement.text().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n");
    Some(CodeWindow {
        text,
        embedding_text: Some(embedding_text),
        range: Range::Text {
            start_line: first.start_row() + 1,
            end_line: last.end_row() + 1,
            start_offset: start,
            end_offset: end,
        },
    })
}

fn split_text_by_lines(
    text: &str,
    max_chars: usize,
    start_line: usize,
    start_offset: usize,
    overlap_chars: usize,
) -> Vec<CodeWindow> {
    let lines: Vec<&str> = text.split('\n').collect();
    // Single precompute per input, mirroring the statement sidecar above:
    // one `chars().count()` per line, then O(1) window sums for packing and
    // `compute_line_overlap`. No recounts in the loops below.
    let line_lens = line_char_lengths(&lines);
    let line_prefix = prefix_sums_with_separator(&line_lens);
    let mut out = Vec::new();
    let mut line_index = 0usize;
    let mut offset = start_offset;
    while line_index < lines.len() {
        let Some(current_len) = line_lens.get(line_index).copied() else {
            break;
        };
        let Some(current) = lines.get(line_index) else {
            break;
        };
        if current_len > max_chars {
            out.extend(split_long_line_by_chars(
                current,
                max_chars,
                start_line + line_index,
                offset,
                overlap_chars,
            ));
            offset += current.len() + 1;
            line_index += 1;
            continue;
        }
        let mut end_index = line_index;
        let mut used_chars = 0usize;
        while end_index < lines.len() {
            let Some(line_len) = line_lens.get(end_index).copied() else {
                break;
            };
            let line_length = line_len.saturating_add(1);
            if used_chars + line_length > max_chars && end_index > line_index {
                break;
            }
            used_chars += line_length;
            end_index += 1;
        }
        let chunk = lines
            .get(line_index..end_index)
            .map(|window| window.join("\n"))
            .unwrap_or_default();
        let end_offset = offset.saturating_add(chunk.len());
        out.push(CodeWindow {
            text: chunk,
            embedding_text: None,
            range: Range::Text {
                start_line: start_line + line_index,
                end_line: start_line + end_index - 1,
                start_offset: offset,
                end_offset,
            },
        });
        if end_index >= lines.len() {
            break;
        }
        let overlap_lines = compute_line_overlap(
            &line_lens,
            &line_prefix,
            line_index,
            end_index,
            overlap_chars,
        );
        // `compute_line_overlap` caps overlap at half the chunk, so
        // `next_index > line_index` always holds here and the window makes
        // progress without recounting line text.
        let next_index = end_index.saturating_sub(overlap_lines).max(line_index + 1);
        offset += lines
            .get(line_index..next_index)
            .map(|window| window.join("\n").len())
            .unwrap_or(0);
        if next_index > line_index {
            offset += 1;
        }
        line_index = next_index;
    }
    out
}

fn split_long_line_by_chars(
    text: &str,
    max_chars: usize,
    line: usize,
    start_offset: usize,
    overlap_chars: usize,
) -> Vec<CodeWindow> {
    // Byte offset of every char boundary; `bounds[k]` starts the k-th char.
    let mut bounds = vec![0usize];
    for (index, ch) in text.char_indices() {
        bounds.push(index + ch.len_utf8());
    }
    let total = bounds.len() - 1;
    let mut out = Vec::new();
    let mut relative_start = 0usize;
    while relative_start < total {
        let raw_end = (relative_start + max_chars).min(total);
        let relative_end = raw_end;
        let (Some(&byte_start), Some(&byte_end)) =
            (bounds.get(relative_start), bounds.get(relative_end))
        else {
            break;
        };
        let Some(window_text) = text.get(byte_start..byte_end) else {
            break;
        };
        out.push(CodeWindow {
            text: window_text.to_owned(),
            embedding_text: None,
            range: Range::Text {
                start_line: line,
                end_line: line,
                start_offset: start_offset + byte_start,
                end_offset: start_offset + byte_end,
            },
        });
        if relative_end >= total {
            break;
        }
        relative_start = (relative_start + 1)
            .max(relative_end.saturating_sub(overlap_chars))
            .min(total);
    }
    out
}

/// One `chars().count()` per statement; call once per input and reuse.
fn statement_char_lengths(statements: &[SyntaxNode<'_>]) -> Vec<usize> {
    statements
        .iter()
        .map(|statement| statement.text().unwrap_or("").chars().count())
        .collect()
}

/// One `chars().count()` per line; call once per input and reuse.
fn line_char_lengths(lines: &[&str]) -> Vec<usize> {
    lines.iter().map(|line| line.chars().count()).collect()
}

/// Prefix sums over `lens` where consecutive items are joined by one `'\n'`.
///
/// `prefix[k]` is the char budget of items `[0..k)` plus one separator per
/// item, so `window_chars(prefix, a, b)` recovers the joined width in O(1).
/// Same table pattern as the char-boundary `bounds` in
/// `split_long_line_by_chars`.
fn prefix_sums_with_separator(lens: &[usize]) -> Vec<usize> {
    let mut prefix = Vec::with_capacity(lens.len() + 1);
    prefix.push(0);
    for len in lens {
        let next = prefix
            .last()
            .copied()
            .unwrap_or(0usize)
            .saturating_add(len.saturating_add(1));
        prefix.push(next);
    }
    prefix
}

/// Joined char width of items `[start..end)` including `'\n'` separators.
///
/// O(1) via the prefix table; out-of-range or empty windows yield 0 instead
/// of panicking on indexing.
fn window_chars(prefix: &[usize], start: usize, end: usize) -> usize {
    if start >= end {
        return 0;
    }
    let (Some(&lo), Some(&hi)) = (prefix.get(start), prefix.get(end)) else {
        return 0;
    };
    hi.saturating_sub(lo).saturating_sub(1)
}

fn compute_overlap_start(
    lens: &[usize],
    prefix: &[usize],
    group_start: usize,
    group_end: usize,
    overlap_chars: usize,
) -> usize {
    if overlap_chars == 0 {
        return group_end.saturating_add(1);
    }
    if group_end < group_start {
        return group_end.saturating_add(1);
    }
    if lens.is_empty() || group_start >= lens.len() {
        return group_start;
    }
    // Suffix widths are O(1) prefix queries, so no statement text is
    // recounted here. Scanning from the group end, the first index whose
    // suffix covers `overlap_chars` is exactly where the accumulating loop
    // used to break; each step's running total equals `window_chars`.
    let clamped_end = group_end.min(lens.len().saturating_sub(1));
    let end_exclusive = clamped_end
        .saturating_add(1)
        .min(prefix.len().saturating_sub(1));
    for index in (group_start..=clamped_end).rev() {
        if window_chars(prefix, index, end_exclusive) >= overlap_chars {
            return index;
        }
    }
    group_start
}

fn compute_line_overlap(
    lens: &[usize],
    prefix: &[usize],
    start_index: usize,
    end_index: usize,
    overlap_chars: usize,
) -> usize {
    if overlap_chars == 0 {
        return 0;
    }
    // Clamp to the precomputed sidecar; valid callers always land inside.
    let end_index = end_index
        .min(lens.len())
        .min(prefix.len().saturating_sub(1));
    if start_index >= end_index {
        return 0;
    }
    // Back-scan with O(1) suffix widths: `window_chars(index, end) + 1` equals
    // the accumulating `line.chars().count() + 1` total step for step, so no
    // line text is recounted here and chunk boundaries are unchanged.
    let mut count = 0usize;
    for index in (start_index..end_index).rev() {
        if lens.get(index).is_none() {
            break;
        }
        let suffix = window_chars(prefix, index, end_index).saturating_add(1);
        if suffix > overlap_chars {
            break;
        }
        count += 1;
    }
    count.min(end_index.saturating_sub(start_index) / 2)
}

fn slice_bytes(text: &str, start: usize, end: usize) -> &str {
    let len = text.len();
    let mut s = start.min(len);
    let mut e = end.min(len).max(s);
    while s < len && !text.is_char_boundary(s) {
        s += 1;
    }
    while e > s && !text.is_char_boundary(e) {
        e -= 1;
    }
    text.get(s..e).unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Single-pass char widths for budget assertions; mirrors the production
    /// sidecar so tests never recount inside their own check loops.
    fn window_widths(windows: &[CodeWindow]) -> Vec<usize> {
        windows
            .iter()
            .map(|window| window.text.chars().count())
            .collect()
    }
    #[test]
    fn prefix_window_matches_naive_join_width() {
        let lens = vec![3, 0, 5, 2];
        let prefix = prefix_sums_with_separator(&lens);
        // Joined widths: sum + (count - 1) separators.
        assert_eq!(window_chars(&prefix, 0, 0), 0);
        assert_eq!(window_chars(&prefix, 1, 1), 0);
        assert_eq!(window_chars(&prefix, 0, 1), 3);
        assert_eq!(window_chars(&prefix, 1, 2), 0);
        assert_eq!(window_chars(&prefix, 0, 2), 4); // "abc" + "\n" + ""
        assert_eq!(window_chars(&prefix, 0, 4), 13); // 3+0+5+2 + 3 seps
        assert_eq!(window_chars(&prefix, 2, 4), 8); // 5 + "\n" + 2
        // Out-of-range windows fall back to 0 instead of panicking.
        assert_eq!(window_chars(&prefix, 3, 99), 0);
        assert_eq!(window_chars(&prefix, 99, 100), 0);
        assert_eq!(window_chars(&prefix, 2, 1), 0);
    }

    #[test]
    fn line_lengths_count_unicode_scalar_values_once() {
        let lines = vec!["héllo", "🦀🦀", "", "a"];
        let lens = line_char_lengths(&lines);
        assert_eq!(lens, vec![5, 2, 0, 1]);
        let prefix = prefix_sums_with_separator(&lens);
        assert_eq!(window_chars(&prefix, 0, 2), 8); // 5 + "\n" + 2
        assert_eq!(window_chars(&prefix, 1, 3), 3); // 2 + "\n" + 0
    }

    #[test]
    fn overlap_start_covers_unicode_and_empty_statements() {
        // Widths: ["fn a() {}", "", "🦀🦀🦀"] -> [9, 0, 3].
        let lens = vec![9, 0, 3];
        let prefix = prefix_sums_with_separator(&lens);
        // Zero overlap jumps past the group end.
        assert_eq!(compute_overlap_start(&lens, &prefix, 0, 2, 0), 3);
        // Overlap smaller than the tail statement lands on it.
        assert_eq!(compute_overlap_start(&lens, &prefix, 0, 2, 2), 2);
        // Overlap spanning the empty statement + separator lands mid-group.
        // Suffix from 1 is "":3 -> 0 + 1 + 3 = 4 >= 4.
        assert_eq!(compute_overlap_start(&lens, &prefix, 0, 2, 4), 1);
        // Overlap larger than the whole group clamps to the group start.
        assert_eq!(compute_overlap_start(&lens, &prefix, 0, 2, 100), 0);
        // Empty statements contribute only their separator: suffixes are
        // 0, 1, 2, so overlap 2 walks all the way back to the group start.
        let empty = vec![0, 0, 0];
        let empty_prefix = prefix_sums_with_separator(&empty);
        assert_eq!(compute_overlap_start(&empty, &empty_prefix, 0, 2, 2), 0);
    }

    #[test]
    fn line_overlap_respects_unicode_empty_and_half_cap() {
        let lines = vec!["héllo", "", "🦀"];
        let lens = line_char_lengths(&lines);
        let prefix = prefix_sums_with_separator(&lens);
        assert_eq!(compute_line_overlap(&lens, &prefix, 0, 3, 0), 0);
        // Tail "🦀" + newline = 2 chars fits in 2.
        assert_eq!(compute_line_overlap(&lens, &prefix, 0, 3, 2), 1);
        // Empty middle line costs only its newline (raw count would be 2),
        // but the half-chunk cap keeps a single overlap line here.
        assert_eq!(compute_line_overlap(&lens, &prefix, 0, 3, 4), 1);
        assert_eq!(compute_line_overlap(&lens, &prefix, 0, 4 - 1, 10_000), 1);
        // Empty window yields no overlap.
        assert_eq!(compute_line_overlap(&lens, &prefix, 2, 2, 10), 0);
    }

    #[test]
    fn text_lines_respect_chunk_size_boundaries() {
        // Each line is 3 chars; with the implicit trailing newline each costs
        // 4 against the budget, so max 8 fits two lines per chunk.
        let windows = split_text_by_lines("aaa\nbbb\nccc", 8, 1, 0, 1);
        assert!(windows.len() >= 2);
        for (i, (window, width)) in windows
            .iter()
            .zip(window_widths(&windows).iter())
            .enumerate()
        {
            assert!(*width <= 8, "oversized window {i}: {:?}", window.text);
        }
        assert_eq!(windows.first().map(|w| w.text.as_str()), Some("aaa\nbbb"));
        // Exact-fit input stays a single chunk.
        let exact = split_text_by_lines("aaa\nbbb", 8, 1, 0, 2);
        assert_eq!(exact.first().map(|w| w.text.as_str()), Some("aaa\nbbb"));
        // Empty lines pack without overshooting.
        let with_empty = split_text_by_lines("aa\n\nbb", 8, 1, 0, 2);
        assert!(!with_empty.is_empty());
        for width in window_widths(&with_empty) {
            assert!(width <= 8);
        }
    }

    #[test]
    fn text_line_overlap_repeats_unicode_tail() {
        let text = "aaaa\n🦀🦀\nbbbb";
        // max 8 packs "aaaa\n🦀🦀" (4+1+2=7); overlap 3 carries the crab line
        // into the next chunk while every chunk still makes progress.
        let windows = split_text_by_lines(text, 8, 1, 0, 3);
        assert!(windows.len() >= 2);
        assert_eq!(windows.first().map(|w| w.text.as_str()), Some("aaaa\n🦀🦀"));
        // No chunk exceeds the budget even with multi-byte characters.
        for (i, (window, width)) in windows
            .iter()
            .zip(window_widths(&windows).iter())
            .enumerate()
        {
            assert!(*width <= 8, "oversized window {i}: {:?}", window.text);
        }
        // Full coverage: every input line appears in at least one chunk.
        let joined = windows
            .iter()
            .map(|window| window.text.clone())
            .collect::<Vec<_>>()
            .join("|");
        for line in text.split('\n') {
            assert!(joined.contains(line), "missing line {line:?}");
        }
    }

    #[test]
    fn long_unicode_line_splits_on_char_boundaries_with_overlap() {
        let text = "a🦀b🦀c🦀d";
        let windows = split_long_line_by_chars(text, 3, 5, 100, 1);
        // 7 chars, width 3, overlap 1: starts 0, 2, 4, then end (7) reached.
        assert_eq!(windows.len(), 3);
        assert_eq!(windows.first().map(|w| w.text.as_str()), Some("a🦀b"));
        assert_eq!(windows.get(1).map(|w| w.text.as_str()), Some("b🦀c"));
        assert_eq!(windows.get(2).map(|w| w.text.as_str()), Some("c🦀d"));
        for (i, (window, width)) in windows
            .iter()
            .zip(window_widths(&windows).iter())
            .enumerate()
        {
            assert!(*width <= 3, "oversized window {i}: {:?}", window.text);
            // Byte offsets stay on char boundaries for multi-byte text.
            assert!(text.contains(&window.text));
        }
        let first_len = windows.first().map(|w| w.text.len()).unwrap_or(0);
        assert_eq!(
            windows.first().map(|w| &w.range),
            Some(&Range::Text {
                start_line: 5,
                end_line: 5,
                start_offset: 100,
                end_offset: 100 + first_len,
            })
        );
    }
    #[test]
    fn long_line_exact_boundary_yields_single_chunk() {
        let windows = split_long_line_by_chars("🦀🦀🦀", 3, 1, 0, 1);
        assert_eq!(windows.len(), 1);
        assert_eq!(windows.first().map(|w| w.text.as_str()), Some("🦀🦀🦀"));
    }
}
