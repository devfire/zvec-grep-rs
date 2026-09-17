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
