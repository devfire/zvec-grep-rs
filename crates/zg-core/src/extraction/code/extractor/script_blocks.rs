//! Component `<script>` block extraction and host-coordinate remapping.

use std::collections::HashMap;

use crate::error::EngineResult;
use crate::extraction::code::extractor::entry::extract_for_indexing;
use crate::extraction::{ChunkOptions, ExtractedFragment};
use crate::ids::{EntityId, FileId, make_entity_id};
use crate::types::{Entity, EntityFragment, FileInfo, Range};

struct ScriptBlock {
    text: String,
    format: String,
    start_line: usize,
    start_offset: usize,
}

pub(crate) fn extract_script_blocks(
    file: &FileInfo,
    text: &str,
    max_chunk_chars: usize,
    chunk_overlap_chars: usize,
) -> EngineResult<Vec<ExtractedFragment>> {
    let mut fragments = Vec::new();
    for block in find_script_blocks(text) {
        let block_file = FileInfo {
            format: crate::types::FileFormat::parse(&block.format),
            ..file.clone()
        };
        let options = ChunkOptions {
            max_chunk_chars: Some(max_chunk_chars),
            overlap_chars: Some(chunk_overlap_chars),
        };
        if let Some(block_fragments) = extract_for_indexing(&block_file, &block.text, &options)? {
            let start_index = fragments.len();
            fragments.extend(remap_script_block_fragments(
                &file.id,
                block_fragments,
                start_index,
                block.start_line,
                block.start_offset,
            ));
        }
    }
    Ok(fragments)
}

fn script_block_format(attrs: &str) -> String {
    if let Some(lang) = script_lang_attr(attrs) {
        if lang == "ts" || lang == "typescript" {
            return "typescript".to_owned();
        }
        if lang == "tsx" {
            return "tsx".to_owned();
        }
        if lang == "jsx" {
            return "jsx".to_owned();
        }
    }
    "javascript".to_owned()
}

fn script_lang_attr(attrs: &str) -> Option<String> {
    let bytes = attrs.as_bytes();
    let mut index = 0usize;
    while index + 4 <= bytes.len() {
        if match_lang_keyword(bytes, index) {
            let mut cursor = index + 4;
            while bytes.get(cursor).is_some_and(|b| b.is_ascii_whitespace()) {
                cursor += 1;
            }
            if bytes.get(cursor) != Some(&b'=') {
                index += 1;
                continue;
            }
            cursor += 1;
            while bytes.get(cursor).is_some_and(|b| b.is_ascii_whitespace()) {
                cursor += 1;
            }
            if matches!(bytes.get(cursor), Some(b'"') | Some(b'\'')) {
                cursor += 1;
            }
            let start = cursor;
            while bytes
                .get(cursor)
                .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_' || *b == b'-')
            {
                cursor += 1;
            }
            if cursor > start {
                return Some(attrs[start..cursor].to_lowercase());
            }
            return None;
        }
        index += 1;
    }
    None
}

/// ASCII case-insensitive `lang` keyword match with a word boundary on both
/// sides (mirrors the TS `\blang\s*=` pattern).
fn match_lang_keyword(bytes: &[u8], index: usize) -> bool {
    let word = b"lang";
    let Some(word_bytes) = bytes.get(index..index + word.len()) else {
        return false;
    };
    if !word_bytes.eq_ignore_ascii_case(word) {
        return false;
    }
    if index > 0 && bytes.get(index - 1).is_some_and(|b| is_attr_word_char(*b)) {
        return false;
    }
    !bytes
        .get(index + word.len())
        .is_some_and(|b| is_attr_word_char(*b))
}

fn is_attr_word_char(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-'
}

fn find_insensitive(haystack: &[u8], needle: &str, from: usize) -> Option<usize> {
    let needle = needle.as_bytes();
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    let mut start = from.min(haystack.len());
    while start + needle.len() <= haystack.len() {
        if haystack
            .get(start..start + needle.len())
            .is_some_and(|window| window.eq_ignore_ascii_case(needle))
        {
            return Some(start);
        }
        start += 1;
    }
    None
}

fn find_script_blocks(text: &str) -> Vec<ScriptBlock> {
    let bytes = text.as_bytes();
    let mut blocks = Vec::new();
    let mut cursor = 0usize;
    while let Some(open) = find_insensitive(bytes, "<script", cursor) {
        let after = open + 7;
        let delimiter = bytes.get(after).copied().unwrap_or(b'>');
        if !(delimiter.is_ascii_whitespace() || delimiter == b'/' || delimiter == b'>') {
            cursor = after;
            continue;
        }
        let Some(tag_end) = bytes
            .iter()
            .skip(after)
            .position(|b| *b == b'>')
            .map(|pos| after + pos)
        else {
            break;
        };
        // UTF-8 safety: every offset here derives from an ASCII-byte match
        // (`<`, `>`, `s`), and no ASCII byte can appear inside a multi-byte
        // UTF-8 sequence — so each is a `str` boundary by construction.
        let attrs = &text[after..tag_end];
        let start_offset = tag_end + 1;
        let Some(close) = find_insensitive(bytes, "</script", start_offset) else {
            break;
        };
        let mut close_end = close + 8;
        while bytes
            .get(close_end)
            .is_some_and(|b| b.is_ascii_whitespace())
        {
            close_end += 1;
        }
        if bytes.get(close_end) != Some(&b'>') {
            cursor = close + 2;
            continue;
        }
        close_end += 1;
        blocks.push(ScriptBlock {
            // Same ASCII-boundary invariant as the `attrs` slice above.
            text: text[start_offset..close].to_owned(),
            format: script_block_format(attrs),
            start_line: bytes
                .iter()
                .take(start_offset)
                .filter(|b| **b == b'\n')
                .count()
                + 1,
            start_offset,
        });
        cursor = close_end;
    }
    blocks
}

fn remap_script_block_fragments(
    file_id: &FileId,
    fragments: Vec<ExtractedFragment>,
    start_index: usize,
    start_line: usize,
    start_offset: usize,
) -> Vec<ExtractedFragment> {
    let mut id_map: HashMap<String, EntityId> = HashMap::new();
    for (index, item) in fragments.iter().enumerate() {
        id_map.insert(
            item.fragment.entity.id.as_str().to_owned(),
            make_entity_id(file_id, start_index + index),
        );
    }
    fragments
        .into_iter()
        .map(|item| {
            let id = id_map
                .get(item.fragment.entity.id.as_str())
                .cloned()
                .unwrap_or_else(|| item.fragment.entity.id.clone());
            let group = item
                .fragment
                .group
                .as_deref()
                .filter(|group| !group.is_empty())
                .and_then(|group| id_map.get(group).cloned())
                .map(|group| group.as_str().to_owned());
            ExtractedFragment {
                fragment: EntityFragment {
                    entity: Entity {
                        id,
                        file_id: file_id.clone(),
                        range: remap_script_block_range(
                            &item.fragment.entity.range,
                            start_line,
                            start_offset,
                        ),
                        content: item.fragment.entity.content.clone(),
                        metadata: item.fragment.entity.metadata.clone(),
                    },
                    group,
                },
                embedding_source: item.embedding_source,
            }
        })
        .collect()
}

fn remap_script_block_range(range: &Range, start_line: usize, start_offset: usize) -> Range {
    match range {
        Range::Text {
            start_line: start,
            end_line: end,
            start_offset: start_off,
            end_offset: end_off,
        } => Range::Text {
            start_line: start_line + start - 1,
            end_line: start_line + end - 1,
            start_offset: start_offset + start_off,
            end_offset: start_offset + end_off,
        },
        Range::File
        | Range::Byte { .. }
        | Range::Page { .. }
        | Range::PageText { .. }
        | Range::PageRegion { .. } => range.clone(),
    }
}
