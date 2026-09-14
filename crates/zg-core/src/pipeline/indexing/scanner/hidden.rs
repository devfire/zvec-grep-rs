//! Hidden-file selection and single-segment glob helpers.

use super::ignore::{IgnoreRule, ignored_path_explicitly_included, match_ignore_rules};
use super::types::{HARD_SKIP_HIDDEN_NAMES, file_name_of};
use crate::pipeline::indexing::root_paths::{matches_root_exclude_patterns, matches_root_patterns};
use crate::types::RootPath;
use crate::utils::glob::{
    normalize_path_pattern, path_pattern_matches, path_pattern_might_match_descendant,
};

pub(crate) fn path_can_be_scanned(
    root: &RootPath,
    relative_path: &str,
    name: &str,
    is_directory: bool,
    ignore_rules: &[IgnoreRule],
) -> bool {
    if relative_path
        .split('/')
        .any(|segment| HARD_SKIP_HIDDEN_NAMES.contains(&segment))
    {
        return false;
    }
    let ignore_match = match_ignore_rules(relative_path, is_directory, ignore_rules);
    if ignore_match.ignored && !ignored_path_explicitly_included(relative_path, root, ignore_match)
    {
        return false;
    }
    if matches_root_exclude_patterns(relative_path, root) {
        return false;
    }
    if is_directory {
        return !should_skip_hidden_directory(name, relative_path, root);
    }
    !should_skip_hidden_file(name, relative_path, root)
        && matches_root_patterns(relative_path, root)
}

fn is_hidden_name(name: &str) -> bool {
    name.starts_with('.') && name != "." && name != ".."
}

pub(crate) fn should_skip_hidden_directory(
    name: &str,
    relative_path: &str,
    root: &RootPath,
) -> bool {
    if !is_hidden_name(name) || root.hidden.unwrap_or(false) {
        return false;
    }
    !has_include_descendant(relative_path, &root.include)
}

pub(crate) fn should_skip_hidden_file(name: &str, relative_path: &str, root: &RootPath) -> bool {
    is_hidden_name(name)
        && !root.hidden.unwrap_or(false)
        && !has_explicit_hidden_file_include(relative_path, &root.include)
}

fn has_include_descendant(relative_path: &str, include: &[String]) -> bool {
    include.iter().any(|pattern| {
        include_pattern_declares_hidden_directory(pattern, relative_path)
            && path_pattern_might_match_descendant(pattern, relative_path)
    })
}

fn has_explicit_hidden_file_include(relative_path: &str, include: &[String]) -> bool {
    include.iter().any(|pattern| {
        include_pattern_declares_hidden_directory(pattern, relative_path)
            && path_pattern_matches(pattern, relative_path)
    })
}

fn include_pattern_declares_hidden_directory(pattern: &str, relative_path: &str) -> bool {
    let name = file_name_of(relative_path);
    if !is_hidden_name(&name) {
        return true;
    }
    normalize_path_pattern(pattern)
        .split('/')
        .any(|segment| hidden_pattern_segment_matches(segment, &name))
}

fn hidden_pattern_segment_matches(pattern_segment: &str, name: &str) -> bool {
    if !pattern_segment.starts_with('.') {
        return false;
    }
    if !pattern_segment.contains(['*', '?']) {
        return pattern_segment == name;
    }
    segment_glob_matches(pattern_segment, name)
}

/// Single-segment glob (`*` any run, `?` one char, no `/` crossing).
fn segment_glob_matches(pattern: &str, name: &str) -> bool {
    fn go(p: &[u8], n: &[u8]) -> bool {
        let Some((&first, rest_p)) = p.split_first() else {
            return n.is_empty();
        };
        match first {
            b'*' => {
                let mut rest = rest_p;
                while let Some((&b'*', tail)) = rest.split_first() {
                    rest = tail;
                }
                for split in 0..=n.len() {
                    if n.get(split..).is_some_and(|tail| go(rest, tail)) {
                        return true;
                    }
                }
                false
            }
            b'?' => match n.split_first() {
                Some((_, rest_n)) => go(rest_p, rest_n),
                None => false,
            },
            b'\\' if !rest_p.is_empty() => {
                let Some((&escaped, after_escape)) = rest_p.split_first() else {
                    return false;
                };
                let Some((&first_n, rest_n)) = n.split_first() else {
                    return false;
                };
                escaped == first_n && go(after_escape, rest_n)
            }
            c => match n.split_first() {
                Some((&first_n, rest_n)) => first_n == c && go(rest_p, rest_n),
                None => false,
            },
        }
    }
    go(pattern.as_bytes(), name.as_bytes())
}
