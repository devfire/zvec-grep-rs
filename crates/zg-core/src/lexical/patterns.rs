//! Pattern loading and matcher construction.
//!
//! Inline patterns merge with `--file` pattern files (one pattern per line,
//! mirroring ripgrep), then combine into a single alternation with optional
//! word boundaries.

use std::fs;
use std::path::PathBuf;

use grep_regex::RegexMatcherBuilder;

use crate::error::EngineResult;

use super::options::LexicalSearchOptions;

/// Maximum number of patterns in one search (inline plus `--file` lines).
/// Bounds matcher build time and memory against pattern-flooding.
pub(crate) const MAX_PATTERN_COUNT: usize = 1000;
/// Maximum length of a single pattern in bytes.
pub(crate) const MAX_PATTERN_LEN_BYTES: usize = 4096;
/// Maximum length of the combined alternation in bytes.
pub(crate) const MAX_COMBINED_PATTERN_LEN_BYTES: usize = 65_536;
/// Cap on the compiled regex size passed to `RegexMatcherBuilder`, bounding
/// catastrophic (ReDoS) automata.
const REGEX_SIZE_LIMIT_BYTES: usize = 10 << 20;

/// Merges inline patterns with `--file` pattern files. Every line of a
/// pattern file is one pattern (only the line break is stripped), matching
/// ripgrep's `--file` handling.
///
/// Caps: at most [`MAX_PATTERN_COUNT`] patterns in total (inline plus file
/// lines) after dropping empties. More is rejected with
/// `LEXICAL.INVALID_PATTERN`; an unreadable file is rejected with
/// `LEXICAL.PATTERN_FILE_UNREADABLE`. Per-pattern and combined byte caps
/// are enforced later by [`build_matcher`].
///
/// # Errors
///
/// Returns [`crate::error::EngineErrorCode::LexicalPatternFileUnreadable`]
/// when a pattern file cannot be read, or
/// [`crate::error::EngineErrorCode::LexicalInvalidPattern`] when the merged
/// list exceeds [`MAX_PATTERN_COUNT`].
pub(crate) fn load_patterns(
    patterns: &[String],
    pattern_files: &[PathBuf],
) -> EngineResult<Vec<String>> {
    use crate::error::{EngineError, EngineErrorCode};

    let mut merged: Vec<String> = patterns.to_vec();
    for file in pattern_files {
        let text = fs::read_to_string(file).map_err(|error| {
            EngineError::new(
                EngineErrorCode::LexicalPatternFileUnreadable,
                format!("unable to read pattern file {}", file.display()),
            )
            .with_context(format!("error={error}"))
        })?;
        for line in text.split('\n') {
            merged.push(line.strip_suffix('\r').unwrap_or(line).to_owned());
        }
    }
    let merged: Vec<String> = merged.into_iter().filter(|p| !p.is_empty()).collect();
    if merged.len() > MAX_PATTERN_COUNT {
        return Err(EngineError::new(
            EngineErrorCode::LexicalInvalidPattern,
            format!("too many search patterns (limit {MAX_PATTERN_COUNT})"),
        ));
    }
    Ok(merged)
}

/// Combines `patterns` into one alternation (`(?:a|b)`, plus `\b..\b` with
/// `word_regexp`, plus `^(?:..)$` with `whole_line`) and compiles it with a
/// 10 MiB regex size limit (ReDoS bound).
///
/// Caps (all rejected with `LEXICAL.INVALID_PATTERN`): at most
/// [`MAX_PATTERN_COUNT`] patterns, each at most [`MAX_PATTERN_LEN_BYTES`]
/// bytes, with the wrapped combined alternation at most
/// [`MAX_COMBINED_PATTERN_LEN_BYTES`] bytes. A pattern the regex compiler
/// itself rejects also maps to `LEXICAL.INVALID_PATTERN`.
///
/// # Errors
///
/// Returns [`crate::error::EngineErrorCode::LexicalInvalidPattern`] when any
/// cap is exceeded or the combined pattern does not compile.
pub(crate) fn build_matcher(
    options: &LexicalSearchOptions,
    patterns: &[String],
) -> EngineResult<grep_regex::RegexMatcher> {
    use crate::error::{EngineError, EngineErrorCode};

    if patterns.len() > MAX_PATTERN_COUNT {
        return Err(EngineError::new(
            EngineErrorCode::LexicalInvalidPattern,
            format!("too many search patterns (limit {MAX_PATTERN_COUNT})"),
        ));
    }
    for pattern in patterns {
        if pattern.len() > MAX_PATTERN_LEN_BYTES {
            return Err(EngineError::new(
                EngineErrorCode::LexicalInvalidPattern,
                format!("search pattern exceeds {MAX_PATTERN_LEN_BYTES} bytes"),
            ));
        }
    }
    let mut alternatives: Vec<String> = Vec::with_capacity(patterns.len());
    for pattern in patterns {
        if options.fixed_strings {
            alternatives.push(regex::escape(pattern));
        } else {
            alternatives.push(pattern.clone());
        }
    }
    let mut combined = alternatives.join("|");
    combined = format!("(?:{combined})");
    if options.word_regexp {
        combined = format!(r"\b{combined}\b");
    }
    if options.whole_line {
        combined = format!("^(?:{combined})$");
    }
    if combined.len() > MAX_COMBINED_PATTERN_LEN_BYTES {
        return Err(EngineError::new(
            EngineErrorCode::LexicalInvalidPattern,
            format!("combined search pattern exceeds {MAX_COMBINED_PATTERN_LEN_BYTES} bytes"),
        ));
    }
    let case_insensitive = options.ignore_case
        || (options.smart_case
            && !patterns
                .iter()
                .flat_map(|pattern| pattern.chars())
                .any(|ch| ch.is_uppercase()));
    RegexMatcherBuilder::new()
        .case_insensitive(case_insensitive)
        .unicode(true)
        .multi_line(false)
        .size_limit(REGEX_SIZE_LIMIT_BYTES)
        .build(&combined)
        .map_err(|error| {
            EngineError::new(
                EngineErrorCode::LexicalInvalidPattern,
                "invalid search pattern",
            )
            .with_context(format!("error={error}"))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::EngineErrorCode;

    fn numbered_patterns(count: usize) -> Vec<String> {
        (0..count).map(|index| format!("pat{index}")).collect()
    }

    #[test]
    fn pattern_count_limit_plus_minus_one() {
        let ok = numbered_patterns(MAX_PATTERN_COUNT);
        assert_eq!(
            load_patterns(&ok, &[])
                .expect("1000 patterns must load")
                .len(),
            MAX_PATTERN_COUNT
        );
        let over = numbered_patterns(MAX_PATTERN_COUNT + 1);
        let err = load_patterns(&over, &[]).expect_err("1001 patterns must fail");
        assert_eq!(*err.code(), EngineErrorCode::LexicalInvalidPattern);
    }

    #[test]
    fn matcher_rejects_over_count_with_code() {
        let options = LexicalSearchOptions::default();
        let over = numbered_patterns(MAX_PATTERN_COUNT + 1);
        let err = build_matcher(&options, &over).expect_err("1001 patterns must fail");
        assert_eq!(*err.code(), EngineErrorCode::LexicalInvalidPattern);
    }

    #[test]
    fn single_pattern_len_limit_plus_minus_one() {
        let options = LexicalSearchOptions::default();
        let ok = vec!["a".repeat(MAX_PATTERN_LEN_BYTES)];
        assert!(build_matcher(&options, &ok).is_ok());
        let over = vec!["a".repeat(MAX_PATTERN_LEN_BYTES + 1)];
        let err = build_matcher(&options, &over).expect_err("4097-byte pattern must fail");
        assert_eq!(*err.code(), EngineErrorCode::LexicalInvalidPattern);
    }

    #[test]
    fn combined_pattern_len_limit_plus_minus_one() {
        // Combined size is `sum + separators + "(?:)"` wrapper (4 bytes):
        // 999 64-byte patterns plus one 597-byte pattern combine to exactly
        // 65_536 (accepted); a 598-byte tail reaches 65_537 (rejected).
        // Both stay under the per-pattern and count caps so only the
        // combined bound is exercised.
        let options = LexicalSearchOptions::default();
        let mut ok: Vec<String> = (0..MAX_PATTERN_COUNT - 1).map(|_| "b".repeat(64)).collect();
        ok.push("b".repeat(597));
        assert!(build_matcher(&options, &ok).is_ok());
        let mut over: Vec<String> = (0..MAX_PATTERN_COUNT - 1).map(|_| "b".repeat(64)).collect();
        over.push("b".repeat(598));
        let err =
            build_matcher(&options, &over).expect_err("65_537-byte combined pattern must fail");
        assert_eq!(*err.code(), EngineErrorCode::LexicalInvalidPattern);
    }
}
