//! Engine error taxonomy mirroring `ZVEC_GREP.ENGINE.*` codes from the
//! TypeScript implementation, with context lines and secret redaction.

use std::fmt;

/// Prefix shared by every engine error code.
pub const ENGINE_ERROR_CODE_PREFIX: &str = "ZVEC_GREP.ENGINE";

/// Fully-qualified engine error code, e.g. `ZVEC_GREP.ENGINE.CONFIG.INVALID`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct EngineErrorCode(String);

impl EngineErrorCode {
    /// Builds a code from a dotted suffix (e.g. `CONFIG.INVALID`).
    pub fn new(suffix: &str) -> Self {
        Self(format!("{ENGINE_ERROR_CODE_PREFIX}.{suffix}"))
    }

    /// Parses a full code string; accepts only strings with the engine prefix.
    pub fn parse(value: &str) -> Option<Self> {
        value
            .strip_prefix(ENGINE_ERROR_CODE_PREFIX)
            .and_then(|rest| rest.strip_prefix('.'))
            .filter(|suffix| !suffix.is_empty())
            .map(|suffix| Self(format!("{ENGINE_ERROR_CODE_PREFIX}.{suffix}")))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for EngineErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Engine error with a dotted code, message, and multi-line context detail.
#[derive(Debug, Clone)]
pub struct EngineError {
    code: EngineErrorCode,
    message: String,
    /// Optional multi-line `key=value` context block.
    context: Option<String>,
}

impl EngineError {
    pub fn new(code: EngineErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            context: None,
        }
    }

    pub fn with_context(mut self, context: impl Into<String>) -> Self {
        self.context = Some(context.into());
        self
    }

    pub fn code(&self) -> &EngineErrorCode {
        &self.code
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn context(&self) -> Option<&str> {
        self.context.as_deref()
    }
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)?;
        if let Some(context) = &self.context {
            write!(f, "\n{context}")?;
        }
        Ok(())
    }
}

impl std::error::Error for EngineError {}

pub type EngineResult<T> = Result<T, EngineError>;

/// Well-known engine error codes.
pub mod codes {
    use super::EngineErrorCode;

    pub fn config_invalid() -> EngineErrorCode {
        EngineErrorCode::new("CONFIG.INVALID")
    }

    pub fn extractor(suffix: &str) -> EngineErrorCode {
        EngineErrorCode::new(&format!("EXTRACTORS.{suffix}"))
    }

    pub fn config_invalid_embedding_runtime() -> EngineErrorCode {
        EngineErrorCode::new("CONFIG.INVALID_EMBEDDING_RUNTIME")
    }

    pub fn manifest_invalid() -> EngineErrorCode {
        EngineErrorCode::new("MANIFEST.INVALID")
    }

    pub fn lock_busy() -> EngineErrorCode {
        EngineErrorCode::new("LOCK.BUSY")
    }

    pub fn daemon_lease_active() -> EngineErrorCode {
        EngineErrorCode::new("DAEMON_LEASE_ACTIVE")
    }
}

/// One entry for [`error_details`]: either a free-form line or a key/value pair.
pub enum DetailEntry<'a> {
    Line(&'a str),
    Pair(&'a str, DetailValue<'a>),
}

/// Scalar value allowed in error detail pairs.
#[derive(Debug, Clone, Copy)]
pub enum DetailValue<'a> {
    Str(&'a str),
    Int(i64),
    Uint(u64),
    Float(f64),
    Bool(bool),
    Null,
}

impl<'a> DetailValue<'a> {
    fn render(&self) -> String {
        match self {
            Self::Str(s) => (*s).to_owned(),
            Self::Int(v) => v.to_string(),
            Self::Uint(v) => v.to_string(),
            Self::Float(v) => v.to_string(),
            Self::Bool(v) => v.to_string(),
            Self::Null => "null".to_owned(),
        }
    }
}

/// Joins non-empty detail entries with newlines; `None` when nothing remains.
pub fn error_details(entries: Vec<DetailEntry<'_>>) -> Option<String> {
    let mut lines = Vec::new();
    for entry in entries {
        match entry {
            DetailEntry::Line(text) => {
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    lines.push(trimmed.to_owned());
                }
            }
            DetailEntry::Pair(key, value) => {
                if matches!(value, DetailValue::Null) {
                    continue;
                }
                lines.push(format!("{key}={}", value.render()));
            }
        }
    }
    if lines.is_empty() {
        None
    } else {
        Some(lines.join("\n"))
    }
}

/// `workspaceIndex=<name>` detail helper.
pub fn workspace_index_detail(name: &str) -> String {
    format!("workspaceIndex={name}")
}

/// Redacts credentials from arbitrary error text, then truncates to `max_length`
/// with an ellipsis. Mirrors the five passes of the TS implementation (the URL
/// userinfo pass is a hand-rolled scanner because the Rust regex crate has no
/// lookbehind).
pub fn redact_error_text(value: &str, max_length: usize) -> String {
    use std::sync::LazyLock;

    use regex::Regex;

    static KEY_VALUE: LazyLock<Regex> = LazyLock::new(|| {
        // A credential-shaped key (optionally access/refresh/id-prefixed),
        // optionally quoted, followed by a quoted value.
        Regex::new(
            r#"(?i)(["']?(?:(?:access|refresh|id)[_ -]?)?(?:api[_ -]?key|token|authorization|password|secret)["']?\s*[:=]\s*)["'][^"']*["']"#,
        )
        .unwrap_or_else(|e| panic!("static redaction regex must compile: {e}"))
    });
    static BEARER: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)\b(bearer|basic)\s+[a-z0-9._\-+/=]+")
            .unwrap_or_else(|e| panic!("static redaction regex must compile: {e}"))
    });
    static SK_TOKEN: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)\bsk-[a-z0-9_\-]{8,}")
            .unwrap_or_else(|e| panic!("static redaction regex must compile: {e}"))
    });

    let mut out = redact_url_userinfo(value);
    out = KEY_VALUE.replace_all(&out, "$1\"[redacted]\"").into_owned();
    out = BEARER.replace_all(&out, "$1 [redacted]").into_owned();
    out = SK_TOKEN.replace_all(&out, "sk-[redacted]").into_owned();
    truncate_chars(&out, max_length)
}

/// Replaces `scheme://user@` userinfo with `[redacted]@`. The scheme must start
/// with an ASCII letter, not end in a punctuation char, and the byte before it
/// must not be a scheme character (mirrors the TS lookbehind).
fn redact_url_userinfo(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut result = String::with_capacity(value.len());
    let mut cursor = 0usize;

    while let Some(offset) = value[cursor..].find("://") {
        let separator = cursor + offset;
        let mut start = separator;
        while start > 0 {
            let b = bytes[start - 1];
            if b.is_ascii_alphanumeric() || matches!(b, b'+' | b'.' | b'-') {
                start -= 1;
            } else {
                break;
            }
        }
        let scheme_valid = start < separator
            && bytes[start].is_ascii_alphabetic()
            && !matches!(bytes[separator - 1], b'+' | b'.' | b'-');

        let mut end = separator + 3;
        let mut terminator = None;
        while end < bytes.len() {
            match bytes[end] {
                b'@' => {
                    terminator = Some(end);
                    break;
                }
                b'/' | b':' | b'?' | b'#' | b' ' | b'\t' | b'\n' | b'\r' => break,
                _ => end += 1,
            }
        }

        if scheme_valid && terminator.is_some() {
            result.push_str(&value[cursor..start]);
            result.push_str("[redacted]@");
            cursor = end + 1;
        } else {
            let copy_through = (separator + 3).min(bytes.len());
            result.push_str(&value[cursor..copy_through]);
            cursor = copy_through;
        }
    }
    result.push_str(&value[cursor..]);
    result
}

/// Truncates to `max_length` Unicode scalar values, keeping `max_length - 1`
/// leading chars plus `…`; never splits a char.
fn truncate_chars(value: &str, max_length: usize) -> String {
    if value.chars().count() <= max_length {
        return value.to_owned();
    }
    let mut truncated: String = value.chars().take(max_length.saturating_sub(1)).collect();
    truncated.push('\u{2026}');
    truncated
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_roundtrip() {
        let code = EngineErrorCode::new("CONFIG.INVALID");
        assert_eq!(code.as_str(), "ZVEC_GREP.ENGINE.CONFIG.INVALID");
        let parsed = EngineErrorCode::parse(code.as_str());
        assert_eq!(parsed.as_ref(), Some(&code));
        assert!(EngineErrorCode::parse("OTHER.X").is_none());
    }

    #[test]
    fn redacts_bearer_and_api_keys() {
        let text = "GET /x Authorization: Bearer abc123.-_ and api_key=\"s3cr3t\" tail";
        let redacted = redact_error_text(text, 200);
        assert!(redacted.contains("Bearer [redacted]"));
        assert!(!redacted.contains("s3cr3t"));
    }

    #[test]
    fn truncates_with_ellipsis() {
        let redacted = redact_error_text("abcdefghij", 5);
        assert_eq!(redacted.chars().count(), 5);
        assert!(redacted.ends_with('\u{2026}'));
    }

    #[test]
    fn details_skip_empty_and_null() {
        let details = error_details(vec![
            DetailEntry::Line("  "),
            DetailEntry::Line("path=/tmp/x"),
            DetailEntry::Pair("count", DetailValue::Uint(3)),
            DetailEntry::Pair("missing", DetailValue::Null),
        ]);
        assert_eq!(details.as_deref(), Some("path=/tmp/x\ncount=3"));
    }
}
