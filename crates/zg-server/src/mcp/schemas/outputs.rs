//! Job and index wire outputs: lifecycle states, error payloads, scan
//! diagnostics, and the index/index-drop response shapes.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::Serialize;

/// Index job lifecycle state (mirrors the TS state strings).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum JobStateOutput {
    /// Job is queued.
    Queued,
    /// Job is running.
    Running,
    /// Job succeeded.
    Succeeded,
    /// Job failed.
    Failed,
    /// Job was cancelled.
    Cancelled,
}

impl From<zg_core::index_status::IndexJobState> for JobStateOutput {
    fn from(state: zg_core::index_status::IndexJobState) -> Self {
        match state {
            zg_core::index_status::IndexJobState::Queued => Self::Queued,
            zg_core::index_status::IndexJobState::Running => Self::Running,
            zg_core::index_status::IndexJobState::Succeeded => Self::Succeeded,
            zg_core::index_status::IndexJobState::Failed => Self::Failed,
            zg_core::index_status::IndexJobState::Cancelled => Self::Cancelled,
        }
    }
}

/// Terminal job error payload (mirrors `jobErrorSchema`).
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct JobErrorOutput {
    /// Frozen error code.
    pub code: String,
    /// One-line message.
    pub message: String,
    /// Redacted engine context, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    /// Redacted cause chain, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cause: Option<String>,
}

impl From<&crate::job_scheduler::IndexJobError> for JobErrorOutput {
    fn from(error: &crate::job_scheduler::IndexJobError) -> Self {
        Self {
            code: error.code.clone(),
            message: error.message.clone(),
            context: error.context.clone(),
            cause: error.cause.clone(),
        }
    }
}

/// One skipped-file sample (mirrors the TS camelCase sample shape).
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SkippedFileSampleOutput {
    /// Absolute path.
    pub absolute_path: String,
    /// Root-relative path.
    pub relative_path: String,
    /// Skip reason (`empty` | `too_large` | `unsupported` | `binary`).
    pub reason: String,
    /// File size, when measured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    /// Limit that excluded the file, when applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit_bytes: Option<u64>,
}

/// Scan diagnostics payload (mirrors the TS `scan_diagnostics` shape).
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ScanDiagnosticsOutput {
    /// Files skipped during the scan.
    #[serde(rename = "skippedFiles")]
    pub skipped_files: usize,
    /// Skip counts by snake_case reason.
    #[serde(rename = "skippedByReason")]
    pub skipped_by_reason: BTreeMap<String, usize>,
    /// Bounded skip samples.
    #[serde(rename = "skippedSamples")]
    pub skipped_samples: Vec<SkippedFileSampleOutput>,
}

impl From<&zg_core::types::FileScanDiagnostics> for ScanDiagnosticsOutput {
    fn from(diagnostics: &zg_core::types::FileScanDiagnostics) -> Self {
        Self {
            skipped_files: diagnostics.skipped_files,
            skipped_by_reason: diagnostics
                .skipped_by_reason
                .iter()
                .map(|(reason, count)| (skipped_reason_name(*reason).to_owned(), *count))
                .collect(),
            skipped_samples: diagnostics
                .skipped_samples
                .iter()
                .map(|sample| SkippedFileSampleOutput {
                    absolute_path: sample.absolute_path.clone(),
                    relative_path: sample.relative_path.clone(),
                    reason: skipped_reason_name(sample.reason).to_owned(),
                    size_bytes: sample.size_bytes,
                    limit_bytes: sample.limit_bytes,
                })
                .collect(),
        }
    }
}

/// Snake_case reason name matching the TS `reason` enum.
const fn skipped_reason_name(reason: zg_core::types::SkippedFileReason) -> &'static str {
    match reason {
        zg_core::types::SkippedFileReason::Empty => "empty",
        zg_core::types::SkippedFileReason::TooLarge => "too_large",
        zg_core::types::SkippedFileReason::Unsupported => "unsupported",
        zg_core::types::SkippedFileReason::Binary => "binary",
    }
}

/// Index action taken (mirrors `"index" | "drop"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum IndexActionOutput {
    /// Index was created or updated.
    Index,
    /// Index was dropped.
    Drop,
}

/// `zvec_grep_index` structured output.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct IndexOutput {
    /// Indexed root.
    pub root: String,
    /// Submitted job id.
    #[serde(rename = "job_id")]
    pub job_id: String,
    /// Job state at return time.
    pub state: JobStateOutput,
    /// True when an existing live job was reused.
    pub reused: bool,
    /// Action taken, when the request selected one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<IndexActionOutput>,
    /// True when the request dropped the index.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dropped: Option<bool>,
    /// Terminal job error, when failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<JobErrorOutput>,
    /// Skipped-file diagnostics (with `debug` after a completed job).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scan_diagnostics: Option<ScanDiagnosticsOutput>,
}

/// `zvec_grep_index_drop` structured output.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct IndexDropOutput {
    /// Indexed root.
    pub root: String,
    /// True when storage existed and was removed.
    pub removed: bool,
}
