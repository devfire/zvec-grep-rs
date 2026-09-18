//! Indexing chunk budgets derived from the model token window.
//!
//! Port of `engine/pipeline/indexing/input-budget.ts` (`indexChunkOptions`):
//! the model's `maxInputTokens` converts to a character budget at ~185
//! chars/100 tokens (100/100 for token-dense text), with 15% overlap.

use crate::extraction::ChunkOptions;

const DEFAULT_CHARS_PER_100_TOKENS: usize = 185;
const TOKEN_DENSE_CHARS_PER_100_TOKENS: usize = 100;
const TOKEN_DENSE_WINDOW_CHARS: usize = 16 * 1024;
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
    let threshold = max_input_tokens.saturating_mul(TOKEN_DENSE_CHARS_PER_100_TOKENS) / 100;
    // Bounded probes, never a full char pass: `take(n).count()` stops after
    // `n + 1` chars, so 100MB-class inputs cost O(window), not O(file).
    if text.chars().take(threshold.saturating_add(1)).count() <= threshold {
        return false;
    }
    if text.chars().take(TOKEN_DENSE_WINDOW_CHARS + 1).count() <= TOKEN_DENSE_WINDOW_CHARS {
        // Small input (at most one window of chars): exact legacy scan, so
        // behavior for small files is bit-identical to before.
        let len = text.chars().count();
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
    // Large input: dense verdict from bounded leading/trailing/interior
    // samples with early exit, instead of sliding a window across the file.
    sample_windows(text).any(window_is_dense)
}

/// Leading, trailing, and two interior byte windows snapped to char
/// boundaries. Bounded: four 16KiB samples regardless of input size.
fn sample_windows(text: &str) -> impl Iterator<Item = &str> {
    let bytes = text.len();
    let mut starts = vec![
        0,
        bytes / 3,
        bytes * 2 / 3,
        bytes.saturating_sub(TOKEN_DENSE_WINDOW_CHARS),
    ];
    starts.sort_unstable();
    starts.dedup();
    starts
        .into_iter()
        .filter_map(|start| snap_window(text, start, TOKEN_DENSE_WINDOW_CHARS))
}

fn snap_window(text: &str, start: usize, len: usize) -> Option<&str> {
    if start >= text.len() {
        return None;
    }
    let mut end = start.saturating_add(len).min(text.len());
    let mut snapped = start;
    while snapped < end && !text.is_char_boundary(snapped) {
        snapped += 1;
    }
    while end > snapped && !text.is_char_boundary(end) {
        end -= 1;
    }
    if snapped >= end {
        return None;
    }
    Some(&text[snapped..end])
}

/// True when one sample window reaches the 30% dense-char bar, bailing on
/// the first dense evidence instead of scanning the whole window.
fn window_is_dense(window: &str) -> bool {
    let required = TOKEN_DENSE_WINDOW_CHARS
        .saturating_mul(TOKEN_DENSE_PERCENT)
        .div_ceil(100);
    let mut dense = 0usize;
    for ch in window.chars() {
        if is_dense_char(ch) {
            dense += 1;
            if dense >= required {
                return true;
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
    #[test]
    fn large_inputs_classify_from_bounded_samples() {
        // 100KB-class inputs take the sampling path (no full char passes).
        let sparse = "a ".repeat(50_000);
        assert_eq!(
            index_chunk_options(Some(1000), Some(&sparse)).max_chunk_chars,
            Some(1850)
        );
        let dense = ";".repeat(100_000);
        assert_eq!(
            index_chunk_options(Some(1000), Some(&dense)).max_chunk_chars,
            Some(1000)
        );
    }
}
