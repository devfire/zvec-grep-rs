//! Indexing progress and result types.

use serde::{Deserialize, Serialize};

use crate::types::{FileInfo, FileScanDiagnostics};

/// One aggregated timing entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimingEntry {
    pub name: String,
    pub duration_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IndexProgressPhase {
    Scanning,
    Indexing,
    Done,
}

/// Embedding model lifecycle stage reported through progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EmbeddingStage {
    Preparing,
    Downloading,
    Ready,
    Warning,
}

/// Embedding-specific progress payload.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexEmbeddingProgress {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub concurrency: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_concurrency: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retryable_failures: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage: Option<EmbeddingStage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub downloaded_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Progress event emitted during indexing.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexProgress {
    pub phase: Option<IndexProgressPhase>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files_total: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files_indexed: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files_failed: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding: Option<IndexEmbeddingProgress>,
}

/// Result of a completed index run.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexResult {
    pub files_scanned: usize,
    pub files_added: usize,
    pub files_modified: usize,
    pub files_pending: usize,
    pub files_deleted: usize,
    pub files_unchanged: usize,
    pub files_failed: usize,
    pub entities_created: usize,
    pub duration_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timings: Option<Vec<TimingEntry>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scan_diagnostics: Option<FileScanDiagnostics>,
}

/// Persistent status derived from the manifest and file metadata.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceIndexStatus {
    pub files_scanned: usize,
    pub files_added: usize,
    pub files_modified: usize,
    pub files_pending: usize,
    pub files_deleted: usize,
    pub files_unchanged: usize,
    pub files_failed: usize,
    pub pending_files: Vec<FileInfo>,
    pub failed_files: Vec<FileInfo>,
    pub added_files: Vec<FileInfo>,
    pub modified_files: Vec<FileInfo>,
    pub deleted_files: Vec<FileInfo>,
    pub files_stored: usize,
    pub entities_indexed: usize,
    pub fragments_truncated: usize,
}
