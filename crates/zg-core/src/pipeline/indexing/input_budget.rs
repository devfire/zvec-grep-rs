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
#[must_use]
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
        let required = len.saturating_mul(TOKEN_DENSE_PERCENT).div_ceil(100);
        let mut dense = 0usize;
        for ch in text.chars() {
            if is_dense_char(ch) {
                dense += 1;
                if dense >= required {
                    return true;
                }
            }
        }
        return false;
    }
    let required = TOKEN_DENSE_WINDOW_CHARS
        .saturating_mul(TOKEN_DENSE_PERCENT)
        .div_ceil(100);
    let mut leading = text.chars();
    let mut trailing = text.chars();
    let mut dense = 0usize;
    for ch in leading.by_ref().take(TOKEN_DENSE_WINDOW_CHARS) {
        if is_dense_char(ch) {
            dense += 1;
            if dense >= required {
                return true;
            }
        }
    }
    let last_window_start = len - TOKEN_DENSE_WINDOW_CHARS;
    let mut window_start = 0usize;
    while window_start.saturating_add(TOKEN_DENSE_WINDOW_STEP_CHARS) <= last_window_start {
        let dropped = trailing
            .by_ref()
            .take(TOKEN_DENSE_WINDOW_STEP_CHARS)
            .filter(|ch| is_dense_char(*ch))
            .count();
        dense = dense.saturating_sub(dropped);
        for ch in leading.by_ref().take(TOKEN_DENSE_WINDOW_STEP_CHARS) {
            if is_dense_char(ch) {
                dense += 1;
                if dense >= required {
                    return true;
                }
            }
        }
        window_start = window_start.saturating_add(TOKEN_DENSE_WINDOW_STEP_CHARS);
    }
    if window_start != last_window_start {
        let delta = last_window_start.saturating_sub(window_start);
        let dropped = trailing
            .by_ref()
            .take(delta)
            .filter(|ch| is_dense_char(*ch))
            .count();
        dense = dense.saturating_sub(dropped);
        for ch in leading.by_ref().take(delta) {
            if is_dense_char(ch) {
                dense += 1;
                if dense >= required {
                    return true;
                }
            }
        }
    }
    false
}

#[must_use]
fn is_dense_char(ch: char) -> bool {
    !(ch.is_ascii_alphabetic() || ch == ' ' || ch == '\t')
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
