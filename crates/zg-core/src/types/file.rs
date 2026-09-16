//! File classification and scan diagnostics types.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::ids::FileId;
use crate::types::UnixMillis;
/// Coarse classification driving extraction and size policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FileKind {
    Text,
    Code,
    Data,
    Image,
}

impl FileKind {
    #[must_use]
    pub fn is_code(&self) -> bool {
        matches!(self, Self::Code)
    }
}

/// Specific detected format identifier (e.g. `rust`, `markdown`, `text`).
///
/// The field is private: [`FileFormat::parse`] trims the raw identifier and
/// falls back to `"text"` when blank, so a `FileFormat` is never empty.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FileFormat(String);

impl FileFormat {
    /// Builds a format from a raw identifier (extension, catalog entry, or
    /// markdown fence tag). Blank input becomes `"text"`, mirroring the
    /// unknown-extension fallback in [`crate::file_type::detect_file_type`].
    pub fn parse(raw: impl AsRef<str>) -> Self {
        let trimmed = raw.as_ref().trim();
        if trimmed.is_empty() {
            Self("text".to_owned())
        } else {
            Self(trimmed.to_owned())
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for FileFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Per-file index bookkeeping stored alongside file metadata.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileIndexStatus {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub indexed_time: Option<UnixMillis>,
    pub entity_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_count: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncated_fragment_count: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// One indexed root directory with scan options.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RootPath {
    pub absolute_path: String,
    pub recursive: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub include: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub globs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub insensitive_globs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub file_types: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub excluded_file_types: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hidden: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub no_ignore: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ignore_files: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_depth: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_file_size_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follow: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_nested_git: Option<bool>,
}

/// A file discovered by the scanner.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileInfo {
    pub id: FileId,
    pub absolute_path: String,
    pub relative_path: String,
    pub root_path: String,
    pub size_bytes: u64,
    pub last_modified_time: UnixMillis,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    pub kind: FileKind,
    pub format: FileFormat,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index_status: Option<FileIndexStatus>,
}

/// Why the scanner skipped a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkippedFileReason {
    Empty,
    TooLarge,
    Unsupported,
    Binary,
}

/// A skipped file sample recorded in scan diagnostics.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkippedFile {
    pub absolute_path: String,
    pub relative_path: String,
    pub reason: SkippedFileReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit_bytes: Option<u64>,
}

/// Aggregate skip diagnostics for one scan.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileScanDiagnostics {
    pub skipped_files: usize,
    pub skipped_by_reason: BTreeMap<SkippedFileReason, usize>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skipped_samples: Vec<SkippedFile>,
}
