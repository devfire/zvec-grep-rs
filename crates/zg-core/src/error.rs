//! Engine error taxonomy mirroring `ZVEC_GREP.ENGINE.*` codes from the
//! TypeScript implementation, with context lines and secret redaction.

use std::borrow::Cow;
use std::fmt;

/// Prefix shared by every engine error code.
pub const ENGINE_ERROR_CODE_PREFIX: &str = "ZVEC_GREP.ENGINE";

/// Fully-qualified engine error code, e.g. `ZVEC_GREP.ENGINE.CONFIG.INVALID`.
///
/// The wire string is the contract and never changes; the representation is
/// a `&'static str` suffix so codes are `Copy`, allocation-free, and
/// exhaustiveness-checkable at the call site. There is no constructor taking
/// a runtime string, so assembling a code from one is impossible by
/// construction — every code in the tree is a literal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize)]
pub struct EngineErrorCode(&'static str);

impl EngineErrorCode {
    /// Builds a code from a dotted suffix (e.g. `CONFIG.INVALID`).
    ///
    /// `const` so domain error enums can map variants to codes in `const fn`.
    #[must_use]
    pub const fn from_static(suffix: &'static str) -> Self {
        Self(suffix)
    }

    /// The dotted suffix without the `ZVEC_GREP.ENGINE.` prefix.
    #[must_use]
    pub const fn suffix(self) -> &'static str {
        self.0
    }

    /// The fully-qualified wire string, e.g. `ZVEC_GREP.ENGINE.CONFIG.INVALID`.
    #[must_use]
    pub fn qualified(self) -> String {
        format!("{ENGINE_ERROR_CODE_PREFIX}.{}", self.0)
    }
}

impl fmt::Display for EngineErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{ENGINE_ERROR_CODE_PREFIX}.{}", self.0)
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

    #[must_use]
    pub fn code(&self) -> &EngineErrorCode {
        &self.code
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    #[must_use]
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
///
/// Every constructor is `const` and takes no runtime input: codes are
/// literals, never assembled. The `extractor` escape hatch that once took a
/// runtime `suffix: &str` is gone; each extractor code is its own literal
/// below (M1). See `tests/golden/error-codes.txt` for the full registry.
pub mod codes {
    use super::EngineErrorCode;

    #[must_use]
    pub const fn config_invalid() -> EngineErrorCode {
        EngineErrorCode::from_static("CONFIG.INVALID")
    }

    #[must_use]
    pub const fn config_invalid_embedding_runtime() -> EngineErrorCode {
        EngineErrorCode::from_static("CONFIG.INVALID_EMBEDDING_RUNTIME")
    }

    #[must_use]
    pub const fn manifest_invalid() -> EngineErrorCode {
        EngineErrorCode::from_static("MANIFEST.INVALID")
    }

    #[must_use]
    pub const fn lock_busy() -> EngineErrorCode {
        EngineErrorCode::from_static("LOCK.BUSY")
    }

    #[must_use]
    pub const fn daemon_lease_active() -> EngineErrorCode {
        EngineErrorCode::from_static("DAEMON_LEASE_ACTIVE")
    }

    /// A `spawn_blocking` body panicked or was aborted (daemon join failure).
    #[must_use]
    pub const fn daemon_blocking_join_failed() -> EngineErrorCode {
        EngineErrorCode::from_static("DAEMON.BLOCKING_JOIN_FAILED")
    }

    #[must_use]
    pub const fn service_read_session_closed() -> EngineErrorCode {
        EngineErrorCode::from_static("SERVICE.READ_SESSION_CLOSED")
    }

    #[must_use]
    pub const fn extractor_code_invalid_chunk_size() -> EngineErrorCode {
        EngineErrorCode::from_static("EXTRACTORS.CODE_INVALID_CHUNK_SIZE")
    }

    #[must_use]
    pub const fn extractor_code_invalid_chunk_overlap() -> EngineErrorCode {
        EngineErrorCode::from_static("EXTRACTORS.CODE_INVALID_CHUNK_OVERLAP")
    }

    #[must_use]
    pub const fn extractor_markdown_invalid_chunk_size() -> EngineErrorCode {
        EngineErrorCode::from_static("EXTRACTORS.MARKDOWN_INVALID_CHUNK_SIZE")
    }

    #[must_use]
    pub const fn extractor_markdown_invalid_chunk_overlap() -> EngineErrorCode {
        EngineErrorCode::from_static("EXTRACTORS.MARKDOWN_INVALID_CHUNK_OVERLAP")
    }

    #[must_use]
    pub const fn extractor_text_invalid_chunk_size() -> EngineErrorCode {
        EngineErrorCode::from_static("EXTRACTORS.TEXT_INVALID_CHUNK_SIZE")
    }

    #[must_use]
    pub const fn extractor_text_invalid_chunk_overlap() -> EngineErrorCode {
        EngineErrorCode::from_static("EXTRACTORS.TEXT_INVALID_CHUNK_OVERLAP")
    }

    #[must_use]
    pub const fn extractor_empty_file_id() -> EngineErrorCode {
        EngineErrorCode::from_static("EXTRACTORS.EMPTY_FILE_ID")
    }

    #[must_use]
    pub const fn extractor_empty_absolute_path() -> EngineErrorCode {
        EngineErrorCode::from_static("EXTRACTORS.EMPTY_ABSOLUTE_PATH")
    }

    #[must_use]
    pub const fn extractor_empty_relative_path() -> EngineErrorCode {
        EngineErrorCode::from_static("EXTRACTORS.EMPTY_RELATIVE_PATH")
    }

    #[must_use]
    pub const fn extractor_image_empty_data() -> EngineErrorCode {
        EngineErrorCode::from_static("EXTRACTORS.IMAGE_EMPTY_DATA")
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
#[must_use]
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
#[must_use]
pub fn workspace_index_detail(name: &str) -> String {
    format!("workspaceIndex={name}")
}

/// Redacts credentials from arbitrary error text, then truncates to `max_length`
/// with an ellipsis. Returns a borrow when nothing needs redacting or cutting,
/// so the common no-secret path allocates zero times. Mirrors the five passes
/// of the TS implementation (the URL
/// userinfo pass is a hand-rolled scanner because the Rust regex crate has no
/// lookbehind).
///
/// # Panics
///
/// Panics only when a `static` redaction regex fails to compile. The patterns
/// are literal constants that ship with the crate, so construction cannot fail
/// at runtime; Phase 2 converts these to `LazyLock<Result<Regex>>` with a
/// skip-pass fallback and removes this section.
// Phase 2 debt: `panic!` in `LazyLock` init must become typed error /
// skip-pass fallback (`LazyLock<Result<Regex>>`). Allowed here so the
// `panic = "deny"` firewall stays green until then.
#[allow(clippy::panic)]
#[must_use]
pub fn redact_error_text(value: &str, max_length: usize) -> Cow<'_, str> {
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

    // Each pass borrows when it matches nothing, so the common no-secret
    // path allocates zero times; only an actual redaction forces ownership
    // (reassigned solely in the `Owned` arm, where nothing borrows `out`).
    let mut out = redact_url_userinfo(value);
    for (pattern, replacement) in [
        (&KEY_VALUE, "$1\"[redacted]\""),
        (&BEARER, "$1 [redacted]"),
        (&SK_TOKEN, "sk-[redacted]"),
    ] {
        if let Cow::Owned(owned) = pattern.replace_all(&out, replacement) {
            out = Cow::Owned(owned);
        }
    }
    truncate_cow(out, max_length)
}

/// Replaces `scheme://user@` userinfo with `[redacted]@`. The scheme must start
/// with an ASCII letter, not end in a punctuation char, and the byte before it
/// must not be a scheme character (mirrors the TS lookbehind).
fn redact_url_userinfo(value: &str) -> Cow<'_, str> {
    // Fast path: no scheme separator means no userinfo — borrow.
    if !value.contains("://") {
        return Cow::Borrowed(value);
    }
    let bytes = value.as_bytes();
    let mut result = String::with_capacity(value.len());
    let mut cursor = 0usize;

    while let Some(offset) = value.get(cursor..).unwrap_or("").find("://") {
        let separator = cursor + offset;
        let mut start = separator;
        while start > 0 {
            let Some(b) = bytes.get(start - 1) else {
                break;
            };
            if b.is_ascii_alphanumeric() || matches!(b, b'+' | b'.' | b'-') {
                start -= 1;
            } else {
                break;
            }
        }
        let scheme_valid = start < separator
            && bytes.get(start).is_some_and(u8::is_ascii_alphabetic)
            && separator
                .checked_sub(1)
                .and_then(|index| bytes.get(index))
                .is_some_and(|b| !matches!(b, b'+' | b'.' | b'-'));

        let mut end = separator + 3;
        let mut terminator = None;
        while let Some(&b) = bytes.get(end) {
            match b {
                b'@' => {
                    terminator = Some(end);
                    break;
                }
                b'/' | b':' | b'?' | b'#' | b' ' | b'\t' | b'\n' | b'\r' => break,
                _ => end += 1,
            }
        }

        if scheme_valid && terminator.is_some() {
            result.push_str(value.get(cursor..start).unwrap_or(""));
            result.push_str("[redacted]@");
            cursor = end + 1;
        } else {
            let copy_through = (separator + 3).min(bytes.len());
            result.push_str(value.get(cursor..copy_through).unwrap_or(""));
            cursor = copy_through;
        }
    }
    result.push_str(value.get(cursor..).unwrap_or(""));
    Cow::Owned(result)
}

/// Truncates to `max_length` Unicode scalar values, keeping `max_length - 1`
/// leading chars plus `…`; never splits a char. Passes borrowing through
/// when nothing is cut.
fn truncate_cow(value: Cow<'_, str>, max_length: usize) -> Cow<'_, str> {
    if value.chars().count() <= max_length {
        return value;
    }
    let mut truncated: String = value.chars().take(max_length.saturating_sub(1)).collect();
    truncated.push('\u{2026}');
    Cow::Owned(truncated)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_roundtrip() {
        let code = EngineErrorCode::from_static("CONFIG.INVALID");
        assert_eq!(code.suffix(), "CONFIG.INVALID");
        assert_eq!(code.qualified(), "ZVEC_GREP.ENGINE.CONFIG.INVALID");
        assert_eq!(code.to_string(), "ZVEC_GREP.ENGINE.CONFIG.INVALID");
        // `Copy`, not just `Clone`: codes move freely into error values.
        let copied = code;
        assert_eq!(copied, code);
    }

    #[test]
    fn redacts_bearer_and_api_keys() {
        let text = "GET /x Authorization: Bearer abc123.-_ and api_key=\"s3cr3t\" tail";
        let redacted = redact_error_text(text, 200);
        assert!(redacted.contains("Bearer [redacted]"));
        assert!(!redacted.contains("s3cr3t"));
    }

    #[test]
    fn clean_input_borrows_without_allocating() {
        assert!(matches!(
            redact_error_text("nothing secret here", 200),
            Cow::Borrowed(_)
        ));
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
