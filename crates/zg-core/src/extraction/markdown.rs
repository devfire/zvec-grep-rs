//! Markdown sectioning: ATX/setext headings, fence-aware windows, outline fragments.
//!
//! Mirrors `engine/extraction/markdown/extractor.ts`. Documents with headings
//! are split into per-section windows (fence-aware, structure-scored breaks);
//! multi-window sections gain an outline fragment that shares its id as the
//! collapse group. Heading-less documents return `None` so the router falls
//! back to the plain-text chunker (same as the TS `fallback`).
//!
//! As in [`crate::extraction::text`], sizes count Unicode scalar values while
//! ranges are byte offsets. ATX parsing replicates the TS regex
//! `^(#{1,6})\s+(.+?)\s*#*\s*$` (lazy trailing-`#` strip, backtracking when
//! the body is whitespace-only) without a regex engine.

use crate::error::{EngineError, EngineResult, codes};
use crate::extraction::text::find_line_cut_chars;
use crate::extraction::vector_content::{chunk_options_for_metadata, fit_text_to_chars};
use crate::extraction::{ChunkOptions, ExtractedFragment, make_entity_id, validate_source_file};
use crate::types::{
    Content, Entity, EntityFragment, EntityMetadata, FileInfo, MarkdownEntityMetadata, Range,
};

/// Sections `text` per heading, or `None` when the plain-text fallback applies.
pub fn extract_for_indexing(
    file: &FileInfo,
    text: &str,
    options: &ChunkOptions,
) -> EngineResult<Option<Vec<ExtractedFragment>>> {
    validate_source_file(file)?;
    let (max_chunk_chars, chunk_overlap_chars) = resolve_chunk_options(options)?;

    let lines: Vec<&str> = text.split('\n').collect();
    let headings = scan_headings(&lines);
    if headings.is_empty() {
        return Ok(None);
    }

    let line_offsets = compute_line_offsets(&lines);
    let fence_lines = compute_fence_lines(&lines);
    let sections = build_sections(&headings, &lines);
    let mut fragments = Vec::new();

    for section in &sections {
        let metadata = markdown_metadata(section);
        let content_options = chunk_options_for_metadata(
            max_chunk_chars,
            chunk_overlap_chars,
            Some(&EntityMetadata::Markdown(metadata.clone())),
        );
        let windows = split_markdown_section(
            &lines,
            &line_offsets,
            &fence_lines,
            section,
            content_options.max_chunk_chars,
            content_options.chunk_overlap_chars,
        );

        if windows.len() > 1 {
            let id = make_entity_id(&file.id, fragments.len());
            let group = id.to_string();
            let outline = lines_to_window(
                &lines,
                &line_offsets,
                section.start_index,
                section.end_index,
            );
            fragments.push(ExtractedFragment {
                embedding_source: None,
                fragment: EntityFragment {
                    entity: Entity {
                        id: id.clone(),
                        file_id: file.id.clone(),
                        range: outline.range,
                        content: Content::Text {
                            text: fit_text_to_chars(
                                &markdown_outline(&metadata),
                                content_options.max_chunk_chars,
                            ),
                        },
                        metadata: Some(EntityMetadata::Markdown(metadata.clone())),
                    },
                    group: Some(group.clone()),
                },
            });
            for window in &windows {
                fragments.push(ExtractedFragment {
                    embedding_source: None,
                    fragment: markdown_window_to_fragment(
                        file,
                        &metadata,
                        window,
                        fragments.len(),
                        Some(group.clone()),
                    ),
                });
            }
            continue;
        }

        for window in &windows {
            fragments.push(ExtractedFragment {
                embedding_source: None,
                fragment: markdown_window_to_fragment(
                    file,
                    &metadata,
                    window,
                    fragments.len(),
                    None,
                ),
            });
        }
    }

    if fragments.is_empty() {
        return Ok(None);
    }
    Ok(Some(fragments))
}

#[derive(Debug, Clone)]
struct Heading {
    level: u32,
    text: String,
    line_index: usize,
}

#[derive(Debug, Clone)]
struct Section {
    heading: Option<Heading>,
    start_index: usize,
    end_index: usize,
    breadcrumb: Vec<String>,
}

struct MarkdownWindow {
    text: String,
    range: Range,
}

fn markdown_window_to_fragment(
    file: &FileInfo,
    metadata: &MarkdownEntityMetadata,
    window: &MarkdownWindow,
    index: usize,
    group: Option<String>,
) -> EntityFragment {
    EntityFragment {
        entity: Entity {
            id: make_entity_id(&file.id, index),
            file_id: file.id.clone(),
            range: window.range.clone(),
            content: Content::Text {
                text: window.text.clone(),
            },
            metadata: Some(EntityMetadata::Markdown(metadata.clone())),
        },
        group,
    }
}

fn markdown_outline(metadata: &MarkdownEntityMetadata) -> String {
    metadata
        .heading
        .clone()
        .unwrap_or_else(|| "markdown section".to_owned())
}

fn scan_headings(lines: &[&str]) -> Vec<Heading> {
    let mut headings = Vec::new();
    let mut fence: Option<&str> = None;
    let mut index = 0;

    while index < lines.len() {
        let line = lines[index];
        let trimmed = line.trim_start();

        if let Some(open) = fence {
            if trimmed.starts_with(open) {
                fence = None;
            }
            index += 1;
            continue;
        }

        if trimmed.starts_with("```") {
            fence = Some("```");
            index += 1;
            continue;
        }
        if trimmed.starts_with("~~~") {
            fence = Some("~~~");
            index += 1;
            continue;
        }

        if let Some((level, text)) = parse_atx_heading(line) {
            headings.push(Heading {
                level,
                text,
                line_index: index,
            });
            index += 1;
            continue;
        }

        let underline = lines.get(index + 1).map(|next| next.trim());
        let mut setext_level = None;
        if !line.trim().is_empty() {
            if let Some(next) = underline {
                if is_setext_underline(next) {
                    setext_level = Some(if next.starts_with('=') { 1 } else { 2 });
                }
            }
        }
        if let Some(level) = setext_level {
            headings.push(Heading {
                level,
                text: line.trim().to_owned(),
                line_index: index,
            });
            index += 2;
            continue;
        }
        index += 1;
    }

    headings
}
/// Parses an ATX heading, replicating `^(#{1,6})\s+(.+?)\s*#*\s*$`: leading
/// `#` run, required whitespace, then the shortest body whose remainder is
/// only whitespace/`#`. A whitespace-only body backtracks one char (so
/// `"##  "` yields empty text while `"## "` is not a heading).
fn parse_atx_heading(line: &str) -> Option<(u32, String)> {
    let hashes = line.bytes().take_while(|&b| b == b'#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let after = &line[hashes..];
    let ws_len = after.len() - after.trim_start().len();
    if ws_len == 0 {
        return None;
    }

    let mut body_start = ws_len;
    if body_start == after.len() {
        let last_ws = after[..body_start]
            .chars()
            .next_back()
            .map(|c| c.len_utf8())
            .unwrap_or(0);
        if body_start <= last_ws {
            return None;
        }
        body_start -= last_ws;
    }
    let rest = &after[body_start..];
    if rest.is_empty() {
        return None;
    }

    let char_bounds: Vec<usize> = rest.char_indices().map(|(b, _)| b).collect();
    for k in 1..=char_bounds.len() {
        let tail_byte = if k < char_bounds.len() {
            char_bounds[k]
        } else {
            rest.len()
        };
        if is_hash_tail(&rest[tail_byte..]) {
            return Some((hashes as u32, rest[..tail_byte].trim().to_owned()));
        }
    }
    None
}

/// Matches `\s*#*\s*$`: the remainder after an ATX body.
fn is_hash_tail(tail: &str) -> bool {
    tail.trim_start().trim_start_matches('#').trim().is_empty()
}

/// Matches `/^(=+|-+)\s*$/` on an already-trimmed line.
fn is_setext_underline(line: &str) -> bool {
    !line.is_empty() && (line.bytes().all(|b| b == b'=') || line.bytes().all(|b| b == b'-'))
}

fn build_sections(headings: &[Heading], lines: &[&str]) -> Vec<Section> {
    let mut stack: Vec<&Heading> = Vec::new();
    let mut sections = Vec::new();
    let first = &headings[0];

    if first.line_index > 0 && !lines[..first.line_index].join("\n").trim().is_empty() {
        sections.push(Section {
            heading: None,
            start_index: 0,
            end_index: first.line_index - 1,
            breadcrumb: Vec::new(),
        });
    }

    for (position, heading) in headings.iter().enumerate() {
        while stack.last().is_some_and(|top| top.level >= heading.level) {
            stack.pop();
        }
        sections.push(Section {
            heading: Some(heading.clone()),
            start_index: heading.line_index,
            end_index: if position + 1 < headings.len() {
                headings[position + 1].line_index - 1
            } else {
                lines.len() - 1
            },
            breadcrumb: stack.iter().map(|item| item.text.clone()).collect(),
        });
        stack.push(heading);
    }

    sections
}

fn split_markdown_section(
    lines: &[&str],
    line_offsets: &[usize],
    fence_lines: &[bool],
    section: &Section,
    max_chars: usize,
    overlap_chars: usize,
) -> Vec<MarkdownWindow> {
    let line_chars: Vec<usize> = lines.iter().map(|line| line.chars().count()).collect();
    let mut windows = Vec::new();
    let mut start_index = section.start_index;

    while start_index <= section.end_index {
        if line_chars[start_index] + 1 > max_chars {
            windows.extend(split_long_markdown_line(
                lines[start_index],
                start_index,
                line_offsets[start_index],
                max_chars,
            ));
            start_index += 1;
            continue;
        }

        let mut end_index = start_index;
        let mut used_chars = 0;
        while end_index <= section.end_index {
            let line_length = line_chars[end_index] + 1;
            if used_chars + line_length > max_chars && end_index > start_index {
                break;
            }
            used_chars += line_length;
            end_index += 1;
        }

        if end_index <= section.end_index && end_index - start_index > 1 {
            end_index = choose_markdown_break(lines, fence_lines, start_index, end_index);
        }

        windows.push(lines_to_window(
            lines,
            line_offsets,
            start_index,
            end_index - 1,
        ));

        if end_index > section.end_index {
            break;
        }

        let overlap_lines =
            compute_markdown_overlap_lines(lines, start_index, end_index, overlap_chars);
        let next_start = end_index - overlap_lines;
        start_index = if next_start > start_index {
            next_start
        } else {
            end_index
        };
    }

    windows
        .into_iter()
        .filter(|window| !window.text.trim().is_empty())
        .collect()
}

fn choose_markdown_break(
    lines: &[&str],
    fence_lines: &[bool],
    start_index: usize,
    end_index: usize,
) -> usize {
    let min_break = start_index + ((end_index - start_index) * 7 / 10).max(1);
    let mut best_break = end_index;
    let mut best_score = markdown_break_score(lines, fence_lines, end_index);

    for index in min_break..=end_index {
        let score = markdown_break_score(lines, fence_lines, index);
        if score > best_score {
            best_break = index;
            best_score = score;
        }
    }

    best_break
}

fn markdown_break_score(lines: &[&str], fence_lines: &[bool], break_index: usize) -> u32 {
    if break_index == 0 || break_index >= lines.len() || fence_lines[break_index] {
        return 0;
    }

    let current = lines[break_index].trim();
    let previous = lines[break_index - 1].trim();

    if is_heading_line(current) {
        return 100;
    }
    if previous.is_empty() && current.is_empty() {
        return 70;
    }
    if previous.is_empty() {
        return 60;
    }
    if is_list_item(current) {
        return 35;
    }
    if is_blockquote(current) {
        return 25;
    }
    10
}

/// Mirrors `/^#{1,6}\s+/` on a trimmed line.
fn is_heading_line(line: &str) -> bool {
    let hashes = line.bytes().take_while(|&b| b == b'#').count();
    hashes >= 1
        && hashes <= 6
        && line[hashes..]
            .chars()
            .next()
            .is_some_and(|c| c.is_whitespace())
}

/// Mirrors `/^([-*+]|\d+\.)\s+/`.
fn is_list_item(line: &str) -> bool {
    let bytes = line.as_bytes();
    if bytes.is_empty() {
        return false;
    }
    if matches!(bytes[0], b'-' | b'*' | b'+') {
        return line[1..].chars().next().is_some_and(|c| c.is_whitespace());
    }
    let digits = bytes.iter().take_while(|&&b| b.is_ascii_digit()).count();
    if digits == 0 || bytes.get(digits) != Some(&b'.') {
        return false;
    }
    line[digits + 1..]
        .chars()
        .next()
        .is_some_and(|c| c.is_whitespace())
}

/// Mirrors `/^>\s+/`.
fn is_blockquote(line: &str) -> bool {
    line.as_bytes().first() == Some(&b'>')
        && line[1..].chars().next().is_some_and(|c| c.is_whitespace())
}

fn split_long_markdown_line(
    line: &str,
    line_index: usize,
    line_offset: usize,
    max_chars: usize,
) -> Vec<MarkdownWindow> {
    let chars: Vec<char> = line.chars().collect();
    let mut windows = Vec::new();
    let mut char_cursor = 0;
    let mut byte_cursor = 0;

    while char_cursor < chars.len() {
        let remaining = chars.len() - char_cursor;
        let cut_chars = if remaining <= max_chars {
            remaining
        } else {
            find_line_cut_chars(&chars[char_cursor..], max_chars)
        };
        let cut_bytes: usize = chars[char_cursor..char_cursor + cut_chars]
            .iter()
            .map(|c| c.len_utf8())
            .sum();
        let text = &line[byte_cursor..byte_cursor + cut_bytes];
        let line_number = line_index + 1;
        windows.push(MarkdownWindow {
            text: text.to_owned(),
            range: Range::Text {
                start_line: line_number,
                end_line: line_number,
                start_offset: line_offset + byte_cursor,
                end_offset: line_offset + byte_cursor + cut_bytes,
            },
        });
        char_cursor += cut_chars;
        byte_cursor += cut_bytes;
    }

    windows
}

fn lines_to_window(
    lines: &[&str],
    line_offsets: &[usize],
    start_index: usize,
    end_index: usize,
) -> MarkdownWindow {
    MarkdownWindow {
        text: lines[start_index..=end_index].join("\n"),
        range: Range::Text {
            start_line: start_index + 1,
            end_line: end_index + 1,
            start_offset: line_offsets[start_index],
            end_offset: line_offsets[end_index] + lines[end_index].len(),
        },
    }
}

fn compute_line_offsets(lines: &[&str]) -> Vec<usize> {
    let mut offsets = Vec::with_capacity(lines.len());
    let mut offset = 0;
    for line in lines {
        offsets.push(offset);
        offset += line.len() + 1;
    }
    offsets
}

fn compute_fence_lines(lines: &[&str]) -> Vec<bool> {
    let mut in_fence = vec![false; lines.len()];
    let mut fence: Option<&str> = None;

    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        if let Some(open) = fence {
            in_fence[index] = true;
            if trimmed.starts_with(open) {
                fence = None;
            }
            continue;
        }
        if trimmed.starts_with("```") {
            fence = Some("```");
        } else if trimmed.starts_with("~~~") {
            fence = Some("~~~");
        }
    }

    in_fence
}

fn compute_markdown_overlap_lines(
    lines: &[&str],
    start_index: usize,
    end_index: usize,
    overlap_chars: usize,
) -> usize {
    if overlap_chars == 0 {
        return 0;
    }

    let mut chars = 0;
    let mut count = 0;
    for index in (start_index + 1..end_index).rev() {
        chars += lines[index].chars().count() + 1;
        if chars > overlap_chars {
            break;
        }
        count += 1;
    }

    count.min((end_index - start_index) / 2)
}

fn markdown_metadata(section: &Section) -> MarkdownEntityMetadata {
    MarkdownEntityMetadata {
        heading: section.heading.as_ref().map(|heading| heading.text.clone()),
        level: section.heading.as_ref().map(|heading| heading.level as i32),
        scope: if section.breadcrumb.is_empty() {
            None
        } else {
            Some(section.breadcrumb.join("::"))
        },
    }
}

fn resolve_chunk_options(options: &ChunkOptions) -> EngineResult<(usize, usize)> {
    let max_chunk_chars = options.max_chunk_chars();
    let chunk_overlap_chars = options.overlap_chars();

    if max_chunk_chars == 0 {
        return Err(EngineError::new(
            codes::extractor("MARKDOWN_INVALID_CHUNK_SIZE"),
            "Markdown extractor requires a positive integer chunk size",
        )
        .with_context(format!("maxChunkChars={max_chunk_chars}")));
    }

    if chunk_overlap_chars >= max_chunk_chars {
        return Err(EngineError::new(
            codes::extractor("MARKDOWN_INVALID_CHUNK_OVERLAP"),
            "Markdown extractor requires overlap to be smaller than chunk size",
        )
        .with_context(format!(
            "maxChunkChars={max_chunk_chars} chunkOverlapChars={chunk_overlap_chars}"
        )));
    }

    Ok((max_chunk_chars, chunk_overlap_chars))
}
