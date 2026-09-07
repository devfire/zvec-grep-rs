//! Ripgrep-style glob compiler and path-pattern matching.
//!
//! Ports `utils/glob.ts` exactly: pattern normalization, a hand-rolled
//! glob→regex translator (`**`, `*`, `?`, `[...]` classes, `{a,b}`
//! alternation), directory-prefix semantics for literal patterns, and
//! descendant-might-match heuristics used to prune walks.

use regex::Regex;

/// Sentinel appended to a directory when testing whether a pattern could match
/// something inside it.
const DESCENDANT_SENTINEL: &str = "__zvec_grep_descendant__";

/// Trims, converts backslashes to `/`, collapses duplicate slashes. Absolute
/// patterns are returned untouched after that; relative ones strip leading
/// `./` repetitions.
pub fn normalize_path_pattern(pattern: &str) -> String {
    let collapsed = collapse_slashes(&pattern.trim().replace('\\', "/"));
    if is_absolute_path_pattern(&collapsed) {
        return collapsed;
    }
    let mut result = collapsed.as_str();
    while let Some(stripped) = result.strip_prefix("./") {
        result = stripped;
    }
    result.to_owned()
}

/// Backslashes to `/` plus slash collapsing for candidate paths.
pub fn normalize_path_for_match(path: &str) -> String {
    collapse_slashes(&path.replace('\\', "/"))
}

/// True for `/`-rooted or `X:/` patterns.
pub fn is_absolute_path_pattern(pattern: &str) -> bool {
    if pattern.starts_with('/') {
        return true;
    }
    let bytes = pattern.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'/' || bytes[2] == b'\\')
}

fn collapse_slashes(value: &str) -> String {
    let mut result = String::with_capacity(value.len());
    let mut previous_slash = false;
    for ch in value.chars() {
        if ch == '/' {
            if !previous_slash {
                result.push(ch);
            }
            previous_slash = true;
        } else {
            result.push(ch);
            previous_slash = false;
        }
    }
    result
}

/// True when the pattern contains glob metacharacters.
pub fn has_path_glob(pattern: &str) -> bool {
    pattern.contains(['*', '?', '['])
}

// -----------------------------------------------------------------------------
// Glob → regex translation
// -----------------------------------------------------------------------------

/// Compiles a normalized glob into a regex string.
///
/// Patterns containing `/` anchor at the start; otherwise they match on any
/// basename (`^(?:.*/)?`).
pub fn glob_to_regex(pattern: &str) -> String {
    let mut expression = if pattern.contains('/') {
        String::from("^")
    } else {
        String::from("^(?:.*/)?")
    };
    expression.push_str(&glob_fragment_to_regex(pattern));
    expression.push('$');
    expression
}

fn compile_matcher(pattern: &str, case_insensitive: bool) -> Option<Regex> {
    regex::RegexBuilder::new(&glob_to_regex(pattern))
        .case_insensitive(case_insensitive)
        .build()
        .ok()
}

fn glob_fragment_to_regex(pattern: &str) -> String {
    let mut expression = String::with_capacity(pattern.len() * 2);
    let mut chars = pattern.char_indices().peekable();
    while let Some((index, ch)) = chars.next() {
        match ch {
            '*' if pattern[index..].starts_with("**/") => {
                expression.push_str("(?:.*/)?");
                chars.next();
                chars.next();
            }
            '*' if pattern[index..].starts_with("**") => {
                expression.push_str(".*");
                chars.next();
            }
            '*' => expression.push_str("[^/]*"),
            '?' => expression.push_str("[^/]"),
            '[' => match read_glob_character_class(pattern, index) {
                Some(class) => {
                    expression.push_str(&class.expression);
                    // Skip from just after `[` through the closing `]`.
                    for _ in 1..=(class.end_index - index) {
                        chars.next();
                    }
                }
                None => expression.push_str("\\["),
            },
            '{' => match read_glob_alternation(pattern, index) {
                Some(alternation) => {
                    let branches: Vec<String> = alternation
                        .alternatives
                        .iter()
                        .map(|alt| glob_fragment_to_regex(alt))
                        .collect();
                    let alternation_expression = format!("(?:{})", branches.join("|"));
                    expression.push_str(&alternation_expression);
                    // Skip from just after `{` through the closing `}`.
                    for _ in 1..=(alternation.end_index - index) {
                        chars.next();
                    }
                }
                None => expression.push_str("\\{"),
            },
            other => expression.push_str(&escape_regex_char(other)),
        }
    }
    expression
}

struct CharacterClass {
    expression: String,
    /// Byte index of the closing `]`.
    end_index: usize,
}

fn read_glob_character_class(pattern: &str, start_index: usize) -> Option<CharacterClass> {
    let end_index = pattern[start_index + 1..].find(']')? + start_index + 1;
    let mut content = &pattern[start_index + 1..end_index];
    if content.is_empty() || content == "!" || content == "^" {
        return None;
    }
    let negated = content.starts_with('!') || content.starts_with('^');
    if negated {
        content = &content[1..];
    }
    let escaped = content.replace('\\', "\\\\").replace('/', "\\/");
    Some(CharacterClass {
        expression: format!("[{}{}]", if negated { "^" } else { "" }, escaped),
        end_index,
    })
}

struct Alternation {
    alternatives: Vec<String>,
    /// Byte index of the closing `}`.
    end_index: usize,
}

fn read_glob_alternation(pattern: &str, start_index: usize) -> Option<Alternation> {
    let mut alternatives: Vec<String> = Vec::new();
    let mut depth = 0usize;
    let mut alternative_start = start_index + 1;
    let bytes = pattern.as_bytes();
    let mut index = start_index + 1;
    while index < bytes.len() {
        let ch = bytes[index] as char;
        match ch {
            '{' => {
                depth += 1;
                index += 1;
                continue;
            }
            '}' if depth > 0 => {
                depth -= 1;
                index += 1;
                continue;
            }
            ',' if depth == 0 => {
                alternatives.push(pattern[alternative_start..index].to_owned());
                alternative_start = index + 1;
                index += 1;
                continue;
            }
            '}' if depth == 0 => {
                if alternatives.is_empty() {
                    return None;
                }
                alternatives.push(pattern[alternative_start..index].to_owned());
                return Some(Alternation {
                    alternatives,
                    end_index: index,
                });
            }
            _ => {}
        }
        index += 1;
    }
    None
}

fn escape_regex_char(ch: char) -> String {
    if ch.is_ascii_alphanumeric() {
        ch.to_string()
    } else {
        format!("\\{ch}")
    }
}

// -----------------------------------------------------------------------------
// Matching entrypoints
// -----------------------------------------------------------------------------

fn glob_pattern_matches(pattern: &str, path: &str, case_insensitive: bool) -> bool {
    if let Some(directory_pattern) = pattern.strip_suffix("/**") {
        if let Some(matcher) = compile_matcher(directory_pattern, case_insensitive)
            && matcher.is_match(path)
        {
            return true;
        }
    }
    compile_matcher(pattern, case_insensitive).is_some_and(|m| m.is_match(path))
}

/// Ripgrep-style glob matching (case-sensitive). Empty patterns never match.
pub fn ripgrep_glob_matches(pattern: &str, path: &str) -> bool {
    ripgrep_glob_matches_with_case(pattern, path, false)
}

/// Ripgrep-style glob matching (case-insensitive).
pub fn ripgrep_glob_matches_case_insensitive(pattern: &str, path: &str) -> bool {
    ripgrep_glob_matches_with_case(pattern, path, true)
}

fn ripgrep_glob_matches_with_case(pattern: &str, path: &str, case_insensitive: bool) -> bool {
    let normalized_pattern = normalize_path_pattern(pattern);
    if normalized_pattern.is_empty() {
        return false;
    }
    glob_pattern_matches(
        &normalized_pattern,
        &normalize_path_for_match(path),
        case_insensitive,
    )
}

/// Literal or glob pattern with directory-prefix semantics: a literal pattern
/// matches itself and everything underneath it.
pub fn path_pattern_matches(pattern: &str, path: &str) -> bool {
    path_pattern_matches_with_case(pattern, path, false)
}

/// Case-insensitive [`path_pattern_matches`].
pub fn path_pattern_matches_case_insensitive(pattern: &str, path: &str) -> bool {
    path_pattern_matches_with_case(pattern, path, true)
}

fn path_pattern_matches_with_case(pattern: &str, path: &str, case_insensitive: bool) -> bool {
    let normalized_pattern = normalize_path_pattern(pattern);
    let normalized_path = normalize_path_for_match(path);
    if normalized_pattern.is_empty() {
        return false;
    }
    if has_path_glob(&normalized_pattern) {
        return glob_pattern_matches(&normalized_pattern, &normalized_path, case_insensitive);
    }
    let candidate = if case_insensitive {
        normalized_path.to_lowercase()
    } else {
        normalized_path
    };
    let expected = if case_insensitive {
        normalized_pattern.to_lowercase()
    } else {
        normalized_pattern
    };
    let expected_prefix = if expected.ends_with('/') {
        None
    } else {
        Some(format!("{expected}/"))
    };
    candidate == expected || expected_prefix.is_some_and(|prefix| candidate.starts_with(&prefix))
}

/// Cheap check used to prune directory walks: might anything inside `dir`
/// match `pattern`?
pub fn path_pattern_might_match_descendant(pattern: &str, directory_path: &str) -> bool {
    let normalized_pattern = normalize_path_pattern(pattern);
    let normalized_directory = normalize_path_for_match(directory_path)
        .trim_end_matches('/')
        .to_owned();
    if normalized_directory.is_empty() {
        return true;
    }
    path_pattern_matches(pattern, &normalized_directory)
        || path_pattern_matches(
            pattern,
            &format!("{normalized_directory}/{DESCENDANT_SENTINEL}"),
        )
        || pattern_prefix_might_match_descendant(&normalized_pattern, &normalized_directory)
}

fn pattern_prefix_might_match_descendant(pattern: &str, directory_path: &str) -> bool {
    let directory_prefix = format!("{directory_path}/");
    let variants = if let Some(stripped) = pattern.strip_prefix("**/") {
        vec![pattern.to_owned(), stripped.to_owned()]
    } else {
        vec![pattern.to_owned()]
    };
    for variant in &variants {
        if !has_path_glob(variant) {
            if variant.starts_with(&directory_prefix) {
                return true;
            }
            continue;
        }
        let literal_prefix = literal_prefix_before_first_glob(variant);
        if !literal_prefix.is_empty()
            && (literal_prefix.starts_with(&directory_prefix)
                || directory_prefix.starts_with(literal_prefix))
        {
            return true;
        }
    }
    false
}

fn literal_prefix_before_first_glob(pattern: &str) -> &str {
    let star = pattern.find('*');
    let question = pattern.find('?');
    let end = match (star, question) {
        (Some(a), Some(b)) => a.min(b),
        (Some(a), None) => a,
        (None, Some(b)) => b,
        (None, None) => pattern.len(),
    };
    &pattern[..end]
}

// -----------------------------------------------------------------------------
// Precompiled hot-path matcher
// -----------------------------------------------------------------------------

/// Precompiled glob used on hot paths.
pub struct CompiledGlob {
    /// `None` when the pattern fails to compile: matches nothing.
    regex: Option<Regex>,
    dir_prefix: Option<Regex>,
}

impl CompiledGlob {
    /// Compiles a pattern for repeated matching.
    pub fn new(pattern: &str, case_insensitive: bool) -> Self {
        let normalized = normalize_path_pattern(pattern);
        let regex = compile_matcher(&normalized, case_insensitive);
        let dir_prefix = normalized
            .strip_suffix("/**")
            .and_then(|prefix| compile_matcher(prefix, case_insensitive));
        Self { regex, dir_prefix }
    }

    pub fn matches(&self, path: &str) -> bool {
        let normalized = normalize_path_for_match(path);
        self.dir_prefix
            .as_ref()
            .is_some_and(|m| m.is_match(&normalized))
            || self.regex.as_ref().is_some_and(|m| m.is_match(&normalized))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn double_star_prefix_matches_root() {
        assert!(glob_matches("**/x", "x"));
        assert!(glob_matches("**/x", "a/b/x"));
    }

    #[test]
    fn dir_prefix_suffix_matches_directory_itself() {
        assert!(glob_matches("foo/**", "foo"));
        assert!(glob_matches("foo/**", "foo/bar"));
        assert!(!glob_matches("foo/**", "foobar"));
    }

    #[test]
    fn question_mark_excludes_separator() {
        assert!(glob_matches("a?c", "abc"));
        assert!(!glob_matches("a?c", "a/c"));
    }

    #[test]
    fn char_classes() {
        assert!(glob_matches("a[bc]d", "abd"));
        assert!(!glob_matches("a[bc]d", "aed"));
        assert!(glob_matches("a[!bc]d", "aed"));
        assert!(!glob_matches("a[!bc]d", "abd"));
    }

    #[test]
    fn empty_char_class_is_literal_including_bracket() {
        assert!(glob_matches("a[]d", "a[]d"));
    }

    #[test]
    fn alternation() {
        assert!(glob_matches("*.{ts,tsx}", "x.ts"));
        assert!(glob_matches("*.{ts,tsx}", "x.tsx"));
        assert!(!glob_matches("*.{ts,tsx}", "x.js"));
        assert!(glob_matches("{a,b{1,2}}", "b2"));
    }

    #[test]
    fn literal_brace_without_comma() {
        assert!(glob_matches("a{b}", "a{b}"));
    }

    #[test]
    fn case_insensitive_matching() {
        assert!(ripgrep_glob_matches_case_insensitive("*.Rs", "a/b.rs"));
        assert!(path_pattern_matches_case_insensitive("/SRC", "/src/x.rs"));
    }

    #[test]
    fn path_patterns_have_directory_semantics() {
        assert!(path_pattern_matches("/a/b", "/a/b"));
        assert!(path_pattern_matches("/a/b", "/a/b/c/d.txt"));
        assert!(!path_pattern_matches("/a/b", "/a/bc"));
        assert!(path_pattern_matches("/a/b/*.rs", "/a/b/lib.rs"));
    }

    #[test]
    fn descendant_heuristics() {
        assert!(path_pattern_might_match_descendant("/a/b/c.rs", "/a"));
        assert!(path_pattern_might_match_descendant("/a/**", "/a/b"));
        assert!(path_pattern_might_match_descendant("src/**/*.rs", "src"));
        // `**`-prefixed patterns with no literal prefix are not prunable by
        // this heuristic in the TS implementation either.
        assert!(!path_pattern_might_match_descendant("**/*.rs", "/anything"));
        assert!(path_pattern_might_match_descendant("x", "/"));
    }

    #[test]
    fn empty_pattern_never_matches() {
        assert!(!ripgrep_glob_matches("", "x"));
        assert!(!path_pattern_matches("", "x"));
        assert!(!path_pattern_matches("  ", "x"));
    }

    #[test]
    fn regex_metacharacters_are_escaped() {
        assert!(glob_matches("a.b", "a.b"));
        assert!(!glob_matches("a.b", "axb"));
        assert!(glob_matches("a+b", "a+b"));
    }

    fn glob_matches(pattern: &str, path: &str) -> bool {
        ripgrep_glob_matches(pattern, path)
    }
}
