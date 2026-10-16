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
    // Large input: slide a 16K-char window across the whole text (upstream
    // steps by 8K; checking every position is a superset, so a dense region
    // anywhere — not only at sampled offsets — qualifies).
    sliding_window_is_dense(text)
}

/// True when any 16K-char window reaches the 30% dense-char bar.
///
/// Single char pass with a ring of the last window's dense flags: O(n)
/// time, O(window) space, no per-window rescan. The bar is in chars, so
/// multi-byte text (CJK, emoji) is judged on the same scale as ASCII —
/// a byte-sized window would hold too few 4-byte chars to ever qualify.
fn sliding_window_is_dense(text: &str) -> bool {
    let required = TOKEN_DENSE_WINDOW_CHARS
        .saturating_mul(TOKEN_DENSE_PERCENT)
        .div_ceil(100);
    let mut ring = vec![false; TOKEN_DENSE_WINDOW_CHARS];
    let mut dense = 0usize;
    for (index, ch) in text.chars().enumerate() {
        let slot = index % TOKEN_DENSE_WINDOW_CHARS;
        // Slot is in bounds by construction: ring has exactly
        // TOKEN_DENSE_WINDOW_CHARS cells and slot is a modulo of it.
        let Some(cell) = ring.get_mut(slot) else {
            continue;
        };
        if index >= TOKEN_DENSE_WINDOW_CHARS && *cell {
            dense -= 1;
        }
        let is_dense = is_dense_char(ch);
        *cell = is_dense;
        if is_dense {
            dense += 1;
        }
        if index + 1 >= TOKEN_DENSE_WINDOW_CHARS && dense >= required {
            return true;
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
    fn four_byte_dense_text_qualifies() {
        // 4-byte chars: a 16KiB byte window holds only ~4K chars, below
        // the 4916-char bar, so byte sampling could never flag this.
        let dense = "\u{1F600}".repeat(100_000);
        assert_eq!(
            index_chunk_options(Some(1000), Some(&dense)).max_chunk_chars,
            Some(1000)
        );
    }

    #[test]
    fn dense_region_buried_between_sparse_text_qualifies() {
        // Dense block sits away from every fixed sample offset.
        let mut text = "a ".repeat(30_000);
        text.push_str(&";".repeat(20_000));
        text.push_str(&"a ".repeat(30_000));
        assert_eq!(
            index_chunk_options(Some(1000), Some(&text)).max_chunk_chars,
            Some(1000)
        );
    }

    #[test]
    fn large_inputs_classify_by_density() {
        // 100KB-class inputs take the sliding-window path.
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
