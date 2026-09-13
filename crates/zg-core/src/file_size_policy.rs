//! Per-kind default max file sizes and override resolution.

use crate::types::FileKind;

/// Default cap for code files: 1 MiB.
pub const DEFAULT_MAX_CODE_FILE_SIZE_BYTES: u64 = 1_048_576;
/// Default cap for text files: 256 MiB.
pub const DEFAULT_MAX_TEXT_FILE_SIZE_BYTES: u64 = 268_435_456;
/// Default cap for data files: 16 MiB.
pub const DEFAULT_MAX_DATA_FILE_SIZE_BYTES: u64 = 16_777_216;
/// Default cap for image files: 10 MiB.
pub const DEFAULT_MAX_IMAGE_FILE_SIZE_BYTES: u64 = 10_485_760;

/// Resolves the effective size cap: an explicit override always wins,
/// otherwise the per-kind default applies.
#[must_use]
pub fn resolve_max_file_size_bytes(kind: FileKind, explicit: Option<u64>) -> u64 {
    explicit.unwrap_or(match kind {
        FileKind::Code => DEFAULT_MAX_CODE_FILE_SIZE_BYTES,
        FileKind::Text => DEFAULT_MAX_TEXT_FILE_SIZE_BYTES,
        FileKind::Data => DEFAULT_MAX_DATA_FILE_SIZE_BYTES,
        FileKind::Image => DEFAULT_MAX_IMAGE_FILE_SIZE_BYTES,
    })
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
