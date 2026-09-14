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

use super::context::expand_context_item;
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
    pub(crate) line_cache: HashMap<PathBuf, Option<Vec<String>>>,
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
        let start_column = first
            .as_ref()
            .map_or(0, |m| char_column(line_text, m.start()));
        let end_column = first.as_ref().map_or_else(
            || line_text.chars().count(),
            |m| char_column(line_text, m.end()),
        );
        let rank = self.next_rank;
        self.next_rank += 1;
        let mut item = build_context_item(
            self.root,
            &path,
            line_number as usize,
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
        *slot.borrow_mut() = path.to_owned();
    });
}

fn current_sink_path() -> PathBuf {
    CURRENT_SEARCH_PATH.with(|slot| slot.borrow().clone())
}
/// Byte offset → character column, mirroring `textPositionAtByteOffset` for
/// the single-line case (multiline prefixes cannot occur: the searcher runs
/// without multiline mode).
fn char_column(line: &str, byte_offset: usize) -> usize {
    let mut end = byte_offset.min(line.len());
    while end > 0 && !line.is_char_boundary(end) {
        end -= 1;
    }
    line[..end].chars().count()
}
