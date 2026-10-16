//! grep-searcher sink turning line matches into ranked context items.
//!
//! Rank is consumed per raw match, mirroring the TS pipeline where
//! `parseLine(line, items.length + 1)` runs ahead of `matchesModifiedTime`
//! (mtime is enforced per file before `search_path` here, so every raw match
//! is kept). The global limit stops the walk one item past the cap so the
//! caller can report `truncated`, mirroring the TS kill-after-limit behavior.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use grep_matcher::Matcher as _;
use grep_searcher::{Sink, SinkMatch};

use super::context::{LineCache, expand_context_item, utf16_len};
use super::options::LexicalSearchOptions;
use super::search_paths::display_paths;

/// grep-searcher sink turning line matches into ranked context items.
/// See the module docs for the ranking/limit contract.
pub(crate) struct MatchSink<'a> {
    pub(crate) items: Vec<crate::service::types::ContextItem>,
    pub(crate) next_rank: usize,
    pub(crate) limit: Option<usize>,
    pub(crate) truncated: bool,
    pub(crate) root: &'a Path,
    pub(crate) options: &'a LexicalSearchOptions,
    pub(crate) matcher: &'a grep_regex::RegexMatcher,
    pub(crate) line_cache: LineCache,
    pub(crate) per_file_counts: HashMap<PathBuf, usize>,
}

impl MatchSink<'_> {
    fn stop_requested(&self) -> bool {
        self.limit.is_some_and(|limit| self.items.len() > limit)
    }

    fn file_match_count(&self, path: &Path) -> usize {
        self.per_file_counts.get(path).copied().unwrap_or(0)
    }
}

impl grep_searcher::SinkError for crate::error::EngineError {
    fn error_message<T: std::fmt::Display>(message: T) -> Self {
        crate::error::EngineError::new(
            crate::error::EngineErrorCode::LexicalSearchFailed,
            message.to_string(),
        )
    }
}

impl Sink for MatchSink<'_> {
    type Error = crate::error::EngineError;

    fn matched(
        &mut self,
        _searcher: &grep_searcher::Searcher,
        mat: &SinkMatch<'_>,
    ) -> Result<bool, Self::Error> {
        if self.stop_requested() {
            self.truncated = true;
            return Ok(false);
        }
        let Some(line_number) = mat.line_number() else {
            return Ok(true);
        };
        let path = current_sink_path();
        // Per-file `--max-count` cap (`-m`): stop this file once reached.
        if self
            .options
            .max_count
            .is_some_and(|cap| self.file_match_count(&path) >= cap)
        {
            return Ok(false);
        }
        // First submatch decides the column range, mirroring
        // `parseRipgrepJsonLine` (`submatches[0]`).
        let first = self.matcher.find(mat.bytes()).ok().flatten();
        let line_text = String::from_utf8_lossy(mat.bytes());
        let line_text = line_text.trim_end_matches(['\r', '\n']);
        // Columns are UTF-16 code units over the lossy decoding, computed
        // from the raw bytes: applying a raw byte offset to the decoded
        // string would shift the column after any invalid UTF-8 sequence
        // (each bad byte decodes to one U+FFFD regardless of byte count).
        let start_column = first
            .as_ref()
            .map_or(0, |m| utf16_column(mat.bytes(), m.start()));
        let end_column = first.as_ref().map_or_else(
            || utf16_len(line_text),
            |m| utf16_column(mat.bytes(), m.end()),
        );
        let rank = self.next_rank;
        self.next_rank += 1;
        let mut item = build_context_item(
            self.root,
            &path,
            usize::try_from(line_number).unwrap_or(usize::MAX),
            start_column,
            end_column,
            line_text.to_owned(),
            rank,
        );
        expand_context_item(&mut item, self.options, &mut self.line_cache);
        *self.per_file_counts.entry(path).or_insert(0) += 1;
        self.items.push(item);
        if self.stop_requested() {
            self.truncated = true;
            return Ok(false);
        }
        Ok(true)
    }
}

fn build_context_item(
    root: &Path,
    path: &Path,
    line_number: usize,
    start_column: usize,
    end_column: usize,
    line_text: String,
    rank: usize,
) -> crate::service::types::ContextItem {
    use crate::service::types::{ContentStatus, ContextFile, ContextItem, ContextItemKind};

    let (relative, absolute_display) = display_paths(root, path);
    ContextItem {
        kind: ContextItemKind::RgMatch,
        rank,
        file: ContextFile {
            absolute_path: absolute_display,
            relative_path: relative,
            root_path: crate::paths::to_display_path(root),
        },
        range: crate::types::Range::Text {
            start_line: line_number,
            end_line: line_number,
            start_offset: start_column,
            end_offset: end_column,
        },
        excerpt_range: None,
        content: crate::types::Content::Text { text: line_text },
        content_role: None,
        outline: None,
        status: ContentStatus::Fresh,
        score: None,
        matched_by: Some("lexical".to_owned()),
        metadata: None,
        entity_id: None,
        trace: None,
        query_groups: Vec::new(),
        container: None,
        selection_reason: None,
        coverage_group: None,
    }
}

// The `Sink` API does not hand the file path to `matched`, so the driver
// records the path of the file currently being searched just before each
// `search_path` call. The whole walk runs synchronously on one thread
// (callers move it with `spawn_blocking`), so a thread-local is sufficient
// and no lock is taken on the hot path.
thread_local! {
    static CURRENT_SEARCH_PATH: std::cell::RefCell<PathBuf> =
        std::cell::RefCell::new(PathBuf::new());
}

pub(crate) fn record_file(path: &Path) {
    CURRENT_SEARCH_PATH.with(|slot| {
        path.clone_into(&mut slot.borrow_mut());
    });
}

fn current_sink_path() -> PathBuf {
    CURRENT_SEARCH_PATH.with(|slot| slot.borrow().clone())
}
/// Raw byte offset → UTF-16 column in the lossy decoding of `bytes`,
/// mirroring upstream `textPositionAtByteOffset` on the decoded line
/// (multiline prefixes cannot occur: the searcher runs without multiline
/// mode). Decoding only the prefix keeps the offset in raw-byte space, so
/// invalid sequences before the match (each decoded to U+FFFD) cannot
/// shift the result.
fn utf16_column(bytes: &[u8], byte_offset: usize) -> usize {
    let end = byte_offset.min(bytes.len());
    let prefix = bytes.get(..end).unwrap_or(bytes);
    String::from_utf8_lossy(prefix)
        .chars()
        .map(char::len_utf16)
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_column_counts_emoji_as_two_units() {
        // "a" + emoji (4 raw bytes, 2 UTF-16 units) + "b": byte offset of
        // "b" is 5, its UTF-16 column is 3.
        let line = "a\u{1F600}b".as_bytes();
        assert_eq!(utf16_column(line, 5), 3);
        assert_eq!(utf16_column(line, 1), 1);
        assert_eq!(utf16_column(line, 0), 0);
    }

    #[test]
    fn utf16_column_unshifted_by_invalid_utf8() {
        // 0xFF decodes to one U+FFFD (1 UTF-16 unit); "needle" starts at
        // raw byte 3 after two bad bytes and a space.
        let line = b"\xff\xfe needle";
        assert_eq!(utf16_column(line, 3), 3);
        assert_eq!(utf16_column(line, 9), 9);
    }
}
