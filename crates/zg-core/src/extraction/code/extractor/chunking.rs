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
    let mut out = Vec::new();
    let mut group_start = 0usize;
    let mut group_chars = 0usize;

    for (index, statement) in statements.iter().enumerate() {
        let statement_text = statement.text().unwrap_or("");
        let statement_chars = statement_text.chars().count();
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
            let overlap_start =
                compute_overlap_start(&statements, group_start, index - 1, overlap_chars);
            let mut candidate_start = overlap_start.min(index);
            let mut candidate_chars = statement_chars;
            let mut previous = index;
            while previous > candidate_start {
                previous -= 1;
                let Some(statement) = statements.get(previous) else {
                    break;
                };
                let added = statement.text().unwrap_or("").chars().count() + 1;
                if candidate_chars + added > max_chars {
                    candidate_start = previous + 1;
                    break;
                }
                candidate_chars += added;
            }
            group_start = candidate_start;
            group_chars = candidate_chars;
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
    let mut out = Vec::new();
    let mut line_index = 0usize;
    let mut offset = start_offset;
    while line_index < lines.len() {
        let Some(current) = lines.get(line_index) else {
            break;
        };
        if current.chars().count() > max_chars {
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
            let Some(line) = lines.get(end_index) else {
                break;
            };
            let line_length = line.chars().count() + 1;
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
        out.push(CodeWindow {
            text: chunk.clone(),
            embedding_text: None,
            range: Range::Text {
                start_line: start_line + line_index,
                end_line: start_line + end_index - 1,
                start_offset: offset,
                end_offset: offset + chunk.len(),
            },
        });
        if end_index >= lines.len() {
            break;
        }
        let overlap_lines = compute_line_overlap(&lines, line_index, end_index, overlap_chars);
        let next_index = end_index - overlap_lines;
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

fn compute_overlap_start(
    statements: &[SyntaxNode<'_>],
    group_start: usize,
    group_end: usize,
    overlap_chars: usize,
) -> usize {
    if overlap_chars == 0 {
        return group_end + 1;
    }
    let mut chars = 0usize;
    let mut index = group_end + 1;
    while index > group_start {
        index -= 1;
        let Some(statement) = statements.get(index) else {
            break;
        };
        chars += statement.text().unwrap_or("").chars().count();
        if index < group_end {
            chars += 1;
        }
        if chars >= overlap_chars {
            break;
        }
    }
    index
}

fn compute_line_overlap(
    lines: &[&str],
    start_index: usize,
    end_index: usize,
    overlap_chars: usize,
) -> usize {
    if overlap_chars == 0 {
        return 0;
    }
    let mut chars = 0usize;
    let mut count = 0usize;
    for index in (start_index..end_index).rev() {
        let Some(line) = lines.get(index) else {
            break;
        };
        chars += line.chars().count() + 1;
        if chars > overlap_chars {
            break;
        }
        count += 1;
    }
    count.min((end_index - start_index) / 2)
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
