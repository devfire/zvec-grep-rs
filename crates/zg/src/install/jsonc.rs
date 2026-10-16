//! Minimal JSONC support for installer configs (upstream #107):
//! comment/trailing-comma tolerant parsing, plus textual upsert /
//! removal so unrelated comments and formatting survive a rewrite.
// Byte scanners index only behind explicit `index < len` guards.
#![allow(clippy::indexing_slicing)]

use std::path::Path;

use crate::error::CliError;

/// Parses JSONC: `None` for empty text, otherwise the object or a
/// refusal error matching the plain-JSON wording.
pub fn parse_jsonc_object(path: &Path, text: &str) -> Result<Option<serde_json::Value>, CliError> {
    if text.trim().is_empty() {
        return Ok(None);
    }
    let stripped = strip_jsonc(text);
    let value: serde_json::Value = serde_json::from_str(&stripped).map_err(|_| {
        CliError::install_refused(format!(
            "{} is not valid JSON. Fix it or re-run with --force to replace it.",
            path.display()
        ))
    })?;
    if value.is_object() {
        Ok(Some(value))
    } else {
        Err(CliError::install_refused(format!(
            "Expected {} to hold a JSON object",
            path.display()
        )))
    }
}

/// True when a serde rewrite would lose information (comments or
/// trailing commas present outside strings).
#[must_use]
pub fn needs_preservation(text: &str) -> bool {
    contains_comment(text) || has_trailing_comma(text)
}

fn contains_comment(text: &str) -> bool {
    scan(text).comment
}

fn has_trailing_comma(text: &str) -> bool {
    scan(text).trailing_comma
}

struct Scan {
    comment: bool,
    trailing_comma: bool,
}

/// Single pass over `text` tracking string state; comment bytes are
/// noted, and a comma whose next non-whitespace/comment token is a
/// closing bracket counts as trailing.
fn scan(text: &str) -> Scan {
    let bytes = text.as_bytes();
    let mut result = Scan {
        comment: false,
        trailing_comma: false,
    };
    let mut index = 0;
    let mut in_string = false;
    let mut escaped = false;
    while index < bytes.len() {
        let byte = bytes[index];
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            index += 1;
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'/' if bytes
                .get(index + 1)
                .is_some_and(|next| *next == b'/' || *next == b'*') =>
            {
                result.comment = true;
                index = skip_comment(bytes, index);
                continue;
            }
            b',' => {
                let mut next = index + 1;
                loop {
                    match bytes.get(next) {
                        Some(b'/')
                            if bytes
                                .get(next + 1)
                                .is_some_and(|n| *n == b'/' || *n == b'*') =>
                        {
                            next = skip_comment(bytes, next);
                        }
                        Some(b) if b.is_ascii_whitespace() => next += 1,
                        Some(b'}' | b']') => {
                            result.trailing_comma = true;
                            break;
                        }
                        _ => break,
                    }
                }
            }
            _ => {}
        }
        index += 1;
    }
    result
}

fn skip_comment(bytes: &[u8], start: usize) -> usize {
    if bytes.get(start + 1) == Some(&b'*') {
        let mut index = start + 2;
        while index + 1 < bytes.len() && !(bytes[index] == b'*' && bytes[index + 1] == b'/') {
            index += 1;
        }
        (index + 2).min(bytes.len())
    } else {
        let mut index = start;
        while index < bytes.len() && bytes[index] != b'\n' {
            index += 1;
        }
        index
    }
}

/// Removes comments (replaced by one space) and trailing commas so
/// `serde_json` can parse JSONC text.
fn strip_jsonc(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    let mut in_string = false;
    let mut escaped = false;
    while index < bytes.len() {
        let byte = bytes[index];
        if in_string {
            out.push(byte);
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            index += 1;
            continue;
        }
        match byte {
            b'"' => {
                in_string = true;
                out.push(byte);
            }
            b'/' if bytes
                .get(index + 1)
                .is_some_and(|next| *next == b'/' || *next == b'*') =>
            {
                out.push(b' ');
                index = skip_comment(bytes, index);
                continue;
            }
            b',' => {
                let mut next = index + 1;
                let mut trailing = false;
                loop {
                    match bytes.get(next) {
                        Some(b'/')
                            if bytes
                                .get(next + 1)
                                .is_some_and(|n| *n == b'/' || *n == b'*') =>
                        {
                            next = skip_comment(bytes, next);
                        }
                        Some(b) if b.is_ascii_whitespace() => next += 1,
                        Some(b'}' | b']') => {
                            trailing = true;
                            break;
                        }
                        _ => break,
                    }
                }
                if !trailing {
                    out.push(byte);
                }
            }
            _ => out.push(byte),
        }
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Byte span of a JSON value starting at or after `start` (first
/// non-whitespace byte). Handles strings, comments, nested brackets.
fn value_span(text: &str, start: usize) -> Option<(usize, usize)> {
    let bytes = text.as_bytes();
    let mut index = start;
    while index < bytes.len() && bytes[index].is_ascii_whitespace() {
        index += 1;
    }
    let begin = index;
    match *bytes.get(index)? {
        b'{' | b'[' => {
            let mut depth = 0usize;
            let mut in_string = false;
            let mut escaped = false;
            while index < bytes.len() {
                let byte = bytes[index];
                if in_string {
                    if escaped {
                        escaped = false;
                    } else if byte == b'\\' {
                        escaped = true;
                    } else if byte == b'"' {
                        in_string = false;
                    }
                    index += 1;
                    continue;
                }
                match byte {
                    b'"' => in_string = true,
                    b'/' if bytes
                        .get(index + 1)
                        .is_some_and(|n| *n == b'/' || *n == b'*') =>
                    {
                        index = skip_comment(bytes, index);
                        continue;
                    }
                    b'{' | b'[' => depth += 1,
                    b'}' | b']' => {
                        depth = depth.saturating_sub(1);
                        if depth == 0 {
                            return Some((begin, index + 1));
                        }
                    }
                    _ => {}
                }
                index += 1;
            }
            None
        }
        b'"' => {
            index += 1;
            let mut escaped = false;
            while index < bytes.len() {
                if escaped {
                    escaped = false;
                } else if bytes[index] == b'\\' {
                    escaped = true;
                } else if bytes[index] == b'"' {
                    return Some((begin, index + 1));
                }
                index += 1;
            }
            None
        }
        _ => {
            while index < bytes.len()
                && !matches!(bytes[index], b',' | b'}' | b']')
                && !bytes[index].is_ascii_whitespace()
            {
                index += 1;
            }
            Some((begin, index))
        }
    }
}

/// Finds `"key"` as a property name at the top level of the object
/// spanning `object_span`; returns `(key_start, value_span)`.
fn find_property(
    text: &str,
    object_span: (usize, usize),
    key: &str,
) -> Option<(usize, (usize, usize))> {
    let needle = format!("\"{key}\"");
    let mut search_from = object_span.0 + 1;
    // Depth relative to the object: only depth-0 colons count, but a
    // plain substring scan for the quoted key followed by `:` is
    // sufficient for installer-owned top-level containers.
    while let Some(found) = text[search_from..object_span.1].find(&needle) {
        let key_start = search_from + found;
        let after = key_start + needle.len();
        let mut index = after;
        let bytes = text.as_bytes();
        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        if bytes.get(index) == Some(&b':')
            && let Some(span) = value_span(text, index + 1)
        {
            return Some((key_start, span));
        }
        search_from = after;
    }
    None
}

fn root_span(text: &str) -> Option<(usize, usize)> {
    value_span(text, 0).filter(|span| text.as_bytes().get(span.0) == Some(&b'{'))
}

/// Inserts or replaces `container_key.zvec_grep = entry` textually,
/// preserving every other byte (comments, trailing commas, order).
#[must_use]
pub fn upsert_entry(text: &str, container_key: &str, entry: &serde_json::Value) -> String {
    let entry_json = serde_json::to_string_pretty(entry).unwrap_or_else(|_| "{}".to_owned());
    let property = format!("\"zvec_grep\": {entry_json}");
    let Some(root) = root_span(text) else {
        return format!("{{\"{container_key}\": {{\"zvec_grep\": {entry_json}}}}}");
    };
    if let Some((_, container_span)) = find_property(text, root, container_key)
        && text.as_bytes().get(container_span.0) == Some(&b'{')
    {
        if let Some((_, value)) = find_property(text, container_span, "zvec_grep") {
            return format!("{}{}{}", &text[..value.0], entry_json, &text[value.1..]);
        }
        let insert_at = container_span.0 + 1;
        return format!("{}{}, {}", &text[..insert_at], property, &text[insert_at..]);
    }
    let insert_at = root.0 + 1;
    format!(
        "{}\"{container_key}\": {{\"zvec_grep\": {entry_json}}}, {}",
        &text[..insert_at],
        &text[insert_at..]
    )
}

/// Removes `container_key.zvec_grep` textually; `None` when absent.
#[must_use]
pub fn remove_entry(text: &str, container_key: &str) -> Option<String> {
    let root = root_span(text)?;
    let (_, container_span) = find_property(text, root, container_key)?;
    let (key_start, value) = find_property(text, container_span, "zvec_grep")?;
    let bytes = text.as_bytes();
    // Swallow one following comma, else one preceding comma.
    let mut end = value.1;
    let mut next = end;
    while next < bytes.len() && bytes[next].is_ascii_whitespace() {
        next += 1;
    }
    if bytes.get(next) == Some(&b',') {
        end = next + 1;
        return Some(format!("{}{}", &text[..key_start], &text[end..]));
    }
    let mut start = key_start;
    let mut prev = start;
    while prev > 0 && bytes[prev - 1].is_ascii_whitespace() {
        prev -= 1;
    }
    if prev > 0 && bytes.get(prev - 1) == Some(&b',') {
        start = prev - 1;
    }
    Some(format!("{}{}", &text[..start], &text[value.1..]))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn parses_comments_and_trailing_commas() {
        let parsed = parse_jsonc_object(Path::new("x.jsonc"), "{ // hi\n \"a\": 1,\n}")
            .unwrap()
            .unwrap();
        assert_eq!(parsed["a"], 1);
        assert!(needs_preservation("{ // hi\n}"));
        assert!(needs_preservation("{\"a\": 1,}"));
        assert!(!needs_preservation("{\"a\": \"http://x\", \"b\": 1}"));
    }

    #[test]
    fn upsert_preserves_comments() {
        let text = "{\n// keep me\n\"mcp\": { \"other\": {\"command\": \"x\",}, },\n}";
        let out = upsert_entry(text, "mcp", &serde_json::json!({"command": "zg"}));
        assert!(out.contains("// keep me"), "{out}");
        assert!(out.contains("\"other\""), "{out}");
        let parsed = parse_jsonc_object(Path::new("x"), &out).unwrap().unwrap();
        assert_eq!(parsed["mcp"]["zvec_grep"]["command"], "zg");
        // Replace path also preserves.
        let out2 = upsert_entry(&out, "mcp", &serde_json::json!({"command": "zg2"}));
        assert!(out2.contains("// keep me"));
        assert_eq!(
            parse_jsonc_object(Path::new("x"), &out2).unwrap().unwrap()["mcp"]["zvec_grep"]["command"],
            "zg2"
        );
        let removed = remove_entry(&out2, "mcp").unwrap();
        assert!(removed.contains("// keep me"));
        assert!(
            parse_jsonc_object(Path::new("x"), &removed)
                .unwrap()
                .unwrap()["mcp"]
                .get("zvec_grep")
                .is_none()
        );
    }
}
