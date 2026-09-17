//! Per-kind default max file sizes and override resolution.

use std::path::Path;

use crate::error::{EngineError, EngineErrorCode, EngineResult};
use crate::types::FileKind;

/// Default cap for code files: 1 MiB.
pub const DEFAULT_MAX_CODE_FILE_SIZE_BYTES: u64 = 1_048_576;
/// Default cap for text files: 256 MiB.
pub const DEFAULT_MAX_TEXT_FILE_SIZE_BYTES: u64 = 268_435_456;
/// Default cap for data files: 16 MiB.
pub const DEFAULT_MAX_DATA_FILE_SIZE_BYTES: u64 = 16_777_216;
/// Default cap for image files: 10 MiB.
pub const DEFAULT_MAX_IMAGE_FILE_SIZE_BYTES: u64 = 10_485_760;
/// Hard ceiling for any explicit override: 512 MiB. Caps unbounded overrides
/// (which would otherwise disable the size guard entirely) while staying
/// above the largest per-kind default.
pub const HARD_MAX_FILE_SIZE_BYTES: u64 = 536_870_912;

/// Resolves the effective size cap: an explicit override wins (clamped to
/// [`HARD_MAX_FILE_SIZE_BYTES`]), otherwise the per-kind default applies.
#[must_use]
pub fn resolve_max_file_size_bytes(kind: FileKind, explicit: Option<u64>) -> u64 {
    explicit
        .map(|cap| cap.min(HARD_MAX_FILE_SIZE_BYTES))
        .unwrap_or(match kind {
            FileKind::Code => DEFAULT_MAX_CODE_FILE_SIZE_BYTES,
            FileKind::Text => DEFAULT_MAX_TEXT_FILE_SIZE_BYTES,
            FileKind::Data => DEFAULT_MAX_DATA_FILE_SIZE_BYTES,
            FileKind::Image => DEFAULT_MAX_IMAGE_FILE_SIZE_BYTES,
        })
}

/// Rejects a zero explicit cap (`Some(0)` would silently skip every file).
/// Oversized overrides need no error: [`resolve_max_file_size_bytes`]
/// clamps them to [`HARD_MAX_FILE_SIZE_BYTES`].
///
/// # Errors
///
/// Returns [`EngineErrorCode::LexicalSearchFailed`] when `explicit` is `Some(0)`.
pub fn validate_max_file_size_bytes(explicit: Option<u64>) -> EngineResult<()> {
    if explicit == Some(0) {
        return Err(EngineError::new(
            EngineErrorCode::LexicalSearchFailed,
            "max file size must be greater than zero",
        ));
    }
    Ok(())
}

/// Shared size gate for the search walk and structural enrichment: resolves
/// the per-kind default when `explicit` is `None` (via
/// [`resolve_max_file_size_bytes`]) and reports whether `size_bytes`
/// exceeds the effective cap. Files whose type cannot be detected fall back
/// to the explicit cap only.
#[must_use]
pub fn file_size_exceeds_cap(path: &Path, size_bytes: u64, explicit: Option<u64>) -> bool {
    match crate::file_type::detect_file_type(path) {
        Some(detected) => size_bytes > resolve_max_file_size_bytes(detected.kind, explicit),
        None => explicit.is_some_and(|cap| size_bytes > cap.min(HARD_MAX_FILE_SIZE_BYTES)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_per_kind() {
        assert_eq!(resolve_max_file_size_bytes(FileKind::Code, None), 1_048_576);
        assert_eq!(
            resolve_max_file_size_bytes(FileKind::Text, None),
            268_435_456
        );
        assert_eq!(
            resolve_max_file_size_bytes(FileKind::Data, None),
            16_777_216
        );
        assert_eq!(
            resolve_max_file_size_bytes(FileKind::Image, None),
            10_485_760
        );
    }

    #[test]
    fn explicit_wins_without_validation() {
        assert_eq!(resolve_max_file_size_bytes(FileKind::Code, Some(7)), 7);
    }
}
