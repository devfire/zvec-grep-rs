//! Indexing chunk budgets derived from the model token window.
//!
//! Port of `engine/pipeline/indexing/input-budget.ts` (`indexChunkOptions`):
//! the model's `maxInputTokens` converts to a character budget at ~185
//! chars/100 tokens (100/100 for token-dense text), with 15% overlap.

use crate::extraction::ChunkOptions;

const DEFAULT_CHARS_PER_100_TOKENS: usize = 185;
const TOKEN_DENSE_CHARS_PER_100_TOKENS: usize = 100;
const TOKEN_DENSE_WINDOW_CHARS: usize = 16 * 1024;
const TOKEN_DENSE_WINDOW_STEP_CHARS: usize = 8 * 1024;
const TOKEN_DENSE_PERCENT: usize = 30;
const CHUNK_OVERLAP_PERCENT: usize = 15;

/// Chunk options for one source text, or defaults when the model declares no
/// token window (mirrors `indexChunkOptions` returning `{}`).
pub fn index_chunk_options(max_input_tokens: Option<usize>, text: Option<&str>) -> ChunkOptions {
    let Some(max_input_tokens) = max_input_tokens else {
        return ChunkOptions::default();
    };
    let chars_per_100_tokens = if is_token_dense_text(text, max_input_tokens) {
        TOKEN_DENSE_CHARS_PER_100_TOKENS
    } else {
        DEFAULT_CHARS_PER_100_TOKENS
    };
    let max_chunk_chars = max_input_tokens.saturating_mul(chars_per_100_tokens) / 100;
    let chunk_overlap_chars = max_chunk_chars.saturating_mul(CHUNK_OVERLAP_PERCENT) / 100;
    ChunkOptions {
        max_chunk_chars: Some(max_chunk_chars),
        overlap_chars: Some(chunk_overlap_chars),
    }
}

fn is_token_dense_text(text: Option<&str>, max_input_tokens: usize) -> bool {
    let Some(text) = text else {
        return false;
    };
    let len = text.chars().count();
    if len <= max_input_tokens.saturating_mul(TOKEN_DENSE_CHARS_PER_100_TOKENS) / 100 {
        return false;
    }
    if len <= TOKEN_DENSE_WINDOW_CHARS {
        return is_token_dense_window(text, 0, len);
    }
    let last_window_start = len - TOKEN_DENSE_WINDOW_CHARS;
    let mut start = 0usize;
    while start <= last_window_start {
        if is_token_dense_window(text, start, TOKEN_DENSE_WINDOW_CHARS) {
            return true;
        }
        start += TOKEN_DENSE_WINDOW_STEP_CHARS;
    }
    last_window_start % TOKEN_DENSE_WINDOW_STEP_CHARS != 0
        && is_token_dense_window(text, last_window_start, TOKEN_DENSE_WINDOW_CHARS)
}

fn is_token_dense_window(text: &str, start: usize, length: usize) -> bool {
    let required = length.saturating_mul(TOKEN_DENSE_PERCENT).div_ceil(100);
    let mut dense = 0usize;
    for ch in text.chars().skip(start).take(length) {
        if !(ch.is_ascii_alphabetic() || ch == ' ' || ch == '\t') {
            dense += 1;
            if dense >= required {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_window_yields_defaults() {
        let options = index_chunk_options(None, Some("hello"));
        assert_eq!(options.max_chunk_chars, None);
    }

    #[test]
    fn sparse_text_uses_default_ratio() {
        let options = index_chunk_options(Some(1000), Some(&"a ".repeat(10000)));
        assert_eq!(options.max_chunk_chars, Some(1850));
        assert_eq!(options.overlap_chars, Some(277));
    }
}
