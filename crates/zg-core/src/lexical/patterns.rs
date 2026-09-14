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
                EngineErrorCode::from_static("LEXICAL.PATTERN_FILE_UNREADABLE"),
                format!("unable to read pattern file {}", file.display()),
            )
            .with_context(format!("error={error}"))
        })?;
        for line in text.split('\n') {
            merged.push(line.strip_suffix('\r').unwrap_or(line).to_owned());
        }
    }
    Ok(merged.into_iter().filter(|p| !p.is_empty()).collect())
}

pub(crate) fn build_matcher(
    options: &LexicalSearchOptions,
    patterns: &[String],
) -> EngineResult<grep_regex::RegexMatcher> {
    use crate::error::{EngineError, EngineErrorCode};

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
        .build(&combined)
        .map_err(|error| {
            EngineError::new(
                EngineErrorCode::from_static("LEXICAL.INVALID_PATTERN"),
                "invalid search pattern",
            )
            .with_context(format!("error={error}"))
        })
}
