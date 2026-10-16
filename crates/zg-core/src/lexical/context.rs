//! Before/after context expansion for lexical matches.
//!
//! Mirrors `expandContextItem`: the match range widens to whole lines, the
//! original range moves to `excerptRange`, and content becomes the joined
//! window. Whole-file line splits are cached per search, bounded by entry
//! count and total cached bytes so a broad search cannot hold the full
//! text of every matched file for its whole duration.

use std::collections::{HashMap, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};

use super::options::LexicalSearchOptions;

/// Maximum number of files whose split lines are cached at once.
const MAX_CACHED_FILES: usize = 32;
/// Maximum total bytes of split lines held in the cache at once.
const MAX_CACHED_BYTES: usize = 4 * 1024 * 1024;

/// Bounded LRU cache of whole-file line splits for one search.
///
/// Eviction is least-recently-used. A single file larger than
/// [`MAX_CACHED_BYTES`] is served from a one-slot scratch buffer instead
/// of evicting every other entry, and is dropped on the next miss.
#[derive(Default)]
pub(crate) struct LineCache {
    entries: HashMap<PathBuf, Option<Vec<String>>>,
    /// Insertion/recency order: front is least recently used.
    order: VecDeque<PathBuf>,
    cached_bytes: usize,
    scratch: Option<(PathBuf, Vec<String>)>,
}

impl LineCache {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    fn touch(&mut self, path: &Path) {
        if let Some(position) = self.order.iter().position(|queued| queued == path)
            && let Some(entry) = self.order.remove(position)
        {
            self.order.push_back(entry);
        }
    }

    fn evict_to_limits(&mut self) {
        while self.entries.len() > MAX_CACHED_FILES || self.cached_bytes > MAX_CACHED_BYTES {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            if let Some(lines) = self.entries.remove(&oldest) {
                let removed = lines.as_deref().map_or(0, lines_bytes);
                self.cached_bytes = self.cached_bytes.saturating_sub(removed);
            }
        }
    }

    #[cfg(test)]
    fn entry_count(&self) -> usize {
        self.entries.len()
    }
}

fn lines_bytes(lines: &[String]) -> usize {
    lines.iter().map(String::len).sum()
}

/// Number of UTF-16 code units in `text`, the column unit used by the
/// upstream TypeScript implementation (`String.length`).
#[must_use]
pub(crate) fn utf16_len(text: &str) -> usize {
    text.chars().map(char::len_utf16).sum()
}

/// Expands a match with before/after context lines, mirroring
/// `expandContextItem`: the range widens to whole lines, the original range
/// moves to `excerptRange`, and content becomes the joined window.
pub(crate) fn expand_context_item(
    item: &mut crate::service::types::ContextItem,
    options: &LexicalSearchOptions,
    cache: &mut LineCache,
) {
    if options.before_context == 0 && options.after_context == 0 {
        return;
    }
    let crate::types::Range::Text {
        start_line,
        end_line,
        ..
    } = item.range
    else {
        return;
    };
    let path = PathBuf::from(&item.file.absolute_path);
    let lines = read_text_lines(&path, cache);
    let Some(lines) = lines else { return };
    if lines.is_empty() {
        return;
    }
    let start = start_line.max(1).min(lines.len().saturating_add(1));
    let end = end_line.max(1).min(lines.len());
    let window_start = start.saturating_sub(options.before_context).max(1);
    let window_end = end.saturating_add(options.after_context).min(lines.len());
    if window_start > window_end {
        return;
    }
    let original = std::mem::replace(
        &mut item.range,
        crate::types::Range::Text {
            start_line: window_start,
            end_line: window_end,
            start_offset: 0,
            end_offset: lines.get(window_end - 1).map_or(0, |line| utf16_len(line)),
        },
    );
    item.excerpt_range = Some(original);
    item.content = crate::types::Content::Text {
        text: lines
            .get(window_start - 1..window_end)
            .map(|window| window.join("\n"))
            .unwrap_or_default(),
    };
}

/// Loads one file's lines (lossy UTF-8, one trailing empty line dropped),
/// consulting and updating the bounded cache.
fn load_lines(path: &Path) -> Option<Vec<String>> {
    fs::read(path).ok().map(|bytes| {
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let mut parts: Vec<String> = text
            .split('\n')
            .map(|line| line.strip_suffix('\r').unwrap_or(line).to_owned())
            .collect();
        if parts.last().is_some_and(String::is_empty) {
            parts.pop();
        }
        parts
    })
}

fn read_text_lines<'a>(path: &Path, cache: &'a mut LineCache) -> Option<&'a [String]> {
    if cache.entries.contains_key(path) {
        cache.touch(path);
        return cache.entries.get(path).and_then(|lines| lines.as_deref());
    }
    if cache
        .scratch
        .as_ref()
        .is_some_and(|(cached, _)| cached == path)
    {
        return cache.scratch.as_ref().map(|(_, lines)| lines.as_slice());
    }
    let Some(lines) = load_lines(path) else {
        // Negative entry: an unreadable file stays unreadable for the rest
        // of this search; caching the failure avoids one open per match.
        cache.entries.insert(path.to_owned(), None);
        cache.order.push_back(path.to_owned());
        cache.evict_to_limits();
        return None;
    };
    if lines_bytes(&lines) > MAX_CACHED_BYTES {
        cache.scratch = Some((path.to_owned(), lines));
        return cache.scratch.as_ref().map(|(_, lines)| lines.as_slice());
    }
    cache.cached_bytes = cache.cached_bytes.saturating_add(lines_bytes(&lines));
    cache.entries.insert(path.to_owned(), Some(lines));
    cache.order.push_back(path.to_owned());
    cache.evict_to_limits();
    // Eviction cannot remove the just-inserted entry: it is most-recently
    // used and on its own fits within the byte cap.
    cache.entries.get(path).and_then(|lines| lines.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn write_lines(dir: &Path, name: &str, lines: usize) -> PathBuf {
        let path = dir.join(name);
        let mut file = fs::File::create(&path).expect("create file");
        for index in 0..lines {
            writeln!(file, "line {index}").expect("write line");
        }
        path
    }

    #[test]
    fn cache_evicts_least_recently_used_beyond_entry_cap() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut cache = LineCache::new();
        let mut first = None;
        for index in 0..=MAX_CACHED_FILES {
            let path = write_lines(dir.path(), &format!("f{index}.txt"), 2);
            if index == 0 {
                first = Some(path.clone());
            }
            assert!(read_text_lines(&path, &mut cache).is_some());
        }
        assert_eq!(cache.entry_count(), MAX_CACHED_FILES);
        let first = first.expect("first path");
        assert!(!cache.entries.contains_key(&first));
    }

    #[test]
    fn utf16_len_counts_surrogate_pairs() {
        assert_eq!(utf16_len("abc"), 3);
        assert_eq!(utf16_len("a\u{1F600}b"), 4);
        assert_eq!(utf16_len(""), 0);
    }
}
