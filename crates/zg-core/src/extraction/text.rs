//! Plain-text line chunker with punctuation-scored cuts (universal fallback).
//!
//! Mirrors `engine/extraction/text/extractor.ts`: line-oriented windows of at
//! most `max_chunk_chars` chars with a trailing-line overlap, plus
//! punctuation-scored slicing for single lines longer than the window.
//!
//! Size accounting counts Unicode scalar values (`char`s); ranges are byte
//! offsets so they slice `text` directly. The TypeScript original counts
//! UTF-16 code units instead — identical for BMP text, and only astral
//! characters (emoji, some CJK extensions) differ.

use crate::error::{EngineError, EngineResult, codes};
use crate::extraction::{ChunkOptions, make_entity_id, validate_source_file};
use crate::types::{Content, Entity, EntityFragment, FileInfo, Range};

/// Chunks `text` into line windows and wraps each as an [`EntityFragment`].
pub fn extract_fragments(
    file: &FileInfo,
    text: &str,
    options: &ChunkOptions,
) -> EngineResult<Vec<EntityFragment>> {
    validate_source_file(file)?;
    let (max_chunk_chars, chunk_overlap_chars) = resolve_chunk_options(options)?;
    Ok(extract_plain_text_fragments(
        file,
        text,
        max_chunk_chars,
        chunk_overlap_chars,
    ))
}

/// Line-windows `text` without option handling (mirrors
/// `extractPlainTextFragments`). Callers pass already-resolved limits.
pub fn extract_plain_text_fragments(
    file: &FileInfo,
    text: &str,
    max_chunk_chars: usize,
    chunk_overlap_chars: usize,
) -> Vec<EntityFragment> {
    chunk_text(text, max_chunk_chars, chunk_overlap_chars)
        .into_iter()
        .enumerate()
        .map(|(index, chunk)| {
            let id = make_entity_id(&file.id, index);
            EntityFragment {
                entity: Entity {
                    id,
                    file_id: file.id.clone(),
                    range: chunk.range,
                    content: Content::Text { text: chunk.text },
                    metadata: None,
                },
                group: None,
            }
        })
        .collect()
}

struct TextChunk {
    text: String,
    range: Range,
}

fn chunk_text(text: &str, max_chars: usize, overlap_chars: usize) -> Vec<TextChunk> {
    if text.trim().is_empty() {
        return Vec::new();
    }

    let lines: Vec<&str> = text.split('\n').collect();
    let line_offsets = compute_line_offsets(&lines);
    let line_chars: Vec<usize> = lines.iter().map(|line| line.chars().count()).collect();
    let mut chunks = Vec::new();
    let mut start_index = 0;

    while start_index < lines.len() {
        if line_chars[start_index] + 1 > max_chars {
            chunks.extend(split_long_line(
                lines[start_index],
                start_index,
                line_offsets[start_index],
                max_chars,
            ));
            start_index += 1;
            continue;
        }

        let mut used_chars = 0;
        let mut end_index = start_index;
        while end_index < lines.len() {
            let line_length = line_chars[end_index] + 1;
            if used_chars + line_length > max_chars && end_index > start_index {
                break;
            }
            used_chars += line_length;
            end_index += 1;
        }

        let chunk = lines[start_index..end_index].join("\n");
        if !chunk.trim().is_empty() {
            // `end_index` always advanced past `start_index` (the first line
            // fits: over-long single lines take the branch above).
            let end_line_index = end_index - 1;
            chunks.push(TextChunk {
                text: chunk,
                range: Range::Text {
                    start_line: start_index + 1,
                    end_line: end_index,
                    start_offset: line_offsets[start_index],
                    end_offset: line_offsets[end_line_index] + lines[end_line_index].len(),
                },
            });
        }

        if end_index >= lines.len() {
            break;
        }
        start_index = compute_next_start_line(&line_chars, start_index, end_index, overlap_chars);
    }

    chunks
}

/// Slices one over-long line into scored cuts, keeping TS range semantics.
fn split_long_line(
    line: &str,
    line_index: usize,
    line_offset: usize,
    max_chars: usize,
) -> Vec<TextChunk> {
    let chars: Vec<char> = line.chars().collect();
    let mut chunks = Vec::new();
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
        let slice = &line[byte_cursor..byte_cursor + cut_bytes];
        if !slice.trim().is_empty() {
            let line_number = line_index + 1;
            chunks.push(TextChunk {
                text: slice.to_owned(),
                range: Range::Text {
                    start_line: line_number,
                    end_line: line_number,
                    start_offset: line_offset + byte_cursor,
                    end_offset: line_offset + byte_cursor + cut_bytes,
                },
            });
        }
        char_cursor += cut_chars;
        byte_cursor += cut_bytes;
    }

    chunks
}

fn compute_line_offsets(lines: &[&str]) -> Vec<usize> {
    let mut offsets = Vec::with_capacity(lines.len());
    let mut offset = 0;
    for line in lines {
        offsets.push(offset);
        // One byte per `\n` separator (`split('\n')` removed exactly one).
        offset += line.len() + 1;
    }
    offsets
}

fn compute_next_start_line(
    line_chars: &[usize],
    start_index: usize,
    end_index: usize,
    overlap_chars: usize,
) -> usize {
    if overlap_chars == 0 {
        return end_index;
    }

    let mut overlap_lines = 0;
    let mut overlap_count = 0;
    let mut index = end_index;
    while index > start_index && overlap_count < overlap_chars {
        index -= 1;
        overlap_count += line_chars[index] + 1;
        overlap_lines += 1;
    }

    let next_start = end_index - overlap_lines;
    if next_start > start_index {
        next_start
    } else {
        end_index
    }
}

/// Chooses a cut length (in chars) for an over-long char slice, preferring
/// sentence/clause/word/separator boundaries in the last 30% of the window.
/// Shared with the markdown extractor (both TS files duplicate it).
pub(crate) fn find_line_cut_chars(line: &[char], max_chars: usize) -> usize {
    if line.len() <= max_chars {
        return line.len();
    }

    let min_position = max_chars * 7 / 10;
    let mut best_position = 0;
    let mut best_score = 0;

    for (index, &character) in line.iter().enumerate().take(max_chars).skip(min_position) {
        let score = match character {
            '.' | '!' | '?' => 4,
            ',' | ';' | ':' => 3,
            ' ' | '\t' => 2,
            '-' | '/' | '\\' => 1,
            _ => 0,
        };
        if score > 0 && score >= best_score {
            best_score = score;
            best_position = index + 1;
        }
    }

    if best_position > 0 {
        best_position
    } else {
        max_chars
    }
}

fn resolve_chunk_options(options: &ChunkOptions) -> EngineResult<(usize, usize)> {
    let max_chunk_chars = options.max_chunk_chars();
    let chunk_overlap_chars = options.overlap_chars();

    if max_chunk_chars == 0 {
        return Err(EngineError::new(
            codes::extractor("TEXT_INVALID_CHUNK_SIZE"),
            "Text extractor requires a positive integer chunk size",
        )
        .with_context(format!("maxChunkChars={max_chunk_chars}")));
    }

    if chunk_overlap_chars >= max_chunk_chars {
        return Err(EngineError::new(
            codes::extractor("TEXT_INVALID_CHUNK_OVERLAP"),
            "Text extractor requires overlap to be smaller than chunk size",
        )
        .with_context(format!(
            "maxChunkChars={max_chunk_chars} chunkOverlapChars={chunk_overlap_chars}"
        )));
    }

    Ok((max_chunk_chars, chunk_overlap_chars))
}
