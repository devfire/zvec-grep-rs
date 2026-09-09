//! Status wire outputs: index/server status shapes and the daemon-status
//! formatter.

use schemars::JsonSchema;
use serde::Serialize;

use super::outputs::{JobErrorOutput, JobStateOutput};
use crate::backend::DaemonIndexStatus;

/// Index policy (mirrors `"enabled" | "disabled" | "undecided"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum IndexPolicyOutput {
    /// Indexing enabled.
    Enabled,
    /// Indexing disabled.
    Disabled,
    /// No policy recorded.
    Undecided,
}

impl From<Option<zg_core::types::WorkspaceIndexPolicy>> for IndexPolicyOutput {
    fn from(policy: Option<zg_core::types::WorkspaceIndexPolicy>) -> Self {
        match policy {
            Some(zg_core::types::WorkspaceIndexPolicy::Enabled) => Self::Enabled,
            Some(zg_core::types::WorkspaceIndexPolicy::Disabled) => Self::Disabled,
            None => Self::Undecided,
        }
    }
}

/// Index source (mirrors `"index" | "unindexed"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum IndexSourceOutput {
    /// Results come from an index.
    Index,
    /// No index exists.
    Unindexed,
}

/// One root-path entry (mirrors the TS snake_case `root_paths` items).
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct RootPathOutput {
    /// Absolute path.
    pub absolute_path: String,
    /// Whether the path is scanned recursively.
    pub recursive: bool,
    /// Include rules, when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include: Option<Vec<String>>,
    /// Exclude rules, when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclude: Option<Vec<String>>,
    /// Case-sensitive glob rules, when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub globs: Option<Vec<String>>,
    /// Case-insensitive glob rules, when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub insensitive_globs: Option<Vec<String>>,
    /// File types to include, when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_types: Option<Vec<String>>,
    /// File types to exclude, when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub excluded_file_types: Option<Vec<String>>,
    /// Include hidden paths, when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hidden: Option<bool>,
    /// Ignore-files disabled, when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub no_ignore: Option<bool>,
    /// Additional ignore files, when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ignore_files: Option<Vec<String>>,
    /// Maximum directory depth, when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_depth: Option<u32>,
    /// Maximum file size, when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_file_size_bytes: Option<u64>,
    /// Follow symlinks, when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follow: Option<bool>,
}

impl From<&zg_core::types::RootPath> for RootPathOutput {
    fn from(path: &zg_core::types::RootPath) -> Self {
        Self {
            absolute_path: path.absolute_path.clone(),
            recursive: path.recursive,
            include: nonempty(path.include.clone()),
            exclude: nonempty(path.exclude.clone()),
            globs: nonempty(path.globs.clone()),
            insensitive_globs: nonempty(path.insensitive_globs.clone()),
            file_types: nonempty(path.file_types.clone()),
            excluded_file_types: nonempty(path.excluded_file_types.clone()),
            hidden: path.hidden,
            no_ignore: path.no_ignore,
            ignore_files: nonempty(path.ignore_files.clone()),
            max_depth: path.max_depth,
            max_file_size_bytes: path.max_file_size_bytes,
            follow: path.follow,
        }
    }
}

fn nonempty(items: Vec<String>) -> Option<Vec<String>> {
    if items.is_empty() { None } else { Some(items) }
}

/// Embedding identity in status output.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct StatusEmbeddingOutput {
    /// Provider name.
    pub provider: String,
    /// Model reference.
    pub model: String,
    /// Vector dimension.
    pub dimension: usize,
    /// Distance metric.
    pub metric: String,
}

impl From<&zg_core::service::types::EmbeddingInfo> for StatusEmbeddingOutput {
    fn from(embedding: &zg_core::service::types::EmbeddingInfo) -> Self {
        Self {
            provider: embedding.provider.clone(),
            model: embedding.model.clone(),
            dimension: embedding.dimension,
            metric: format!("{:?}", embedding.metric).to_lowercase(),
        }
    }
}

/// Workspace index identity in status output.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct StatusWorkspaceIndexOutput {
    /// Index id.
    pub id: String,
    /// Index name.
    pub name: String,
    /// Storage path.
    pub path: String,
    /// Configured root paths.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub root_paths: Vec<RootPathOutput>,
    /// Embedding identity, when recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding: Option<StatusEmbeddingOutput>,
    /// Index version, when recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index_version: Option<i64>,
    /// Creation time, epoch millis.
    pub created_time: i64,
    /// Update time, epoch millis.
    pub updated_time: i64,
}

/// File counters in status output.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct StatusFilesOutput {
    /// Files stored.
    pub stored: usize,
    /// Files scanned.
    pub scanned: usize,
    /// Entities indexed.
    pub indexed: usize,
    /// Files pending.
    pub pending: usize,
    /// Files failed.
    pub failed: usize,
    /// Files added.
    pub added: usize,
    /// Files modified.
    pub modified: usize,
    /// Files deleted.
    pub deleted: usize,
    /// Files unchanged.
    pub unchanged: usize,
    /// Entities indexed.
    pub entities: usize,
    /// Truncated fragments.
    pub truncated_fragments: usize,
}

impl From<&zg_core::types::WorkspaceIndexStatus> for StatusFilesOutput {
    fn from(status: &zg_core::types::WorkspaceIndexStatus) -> Self {
        Self {
            stored: status.files_stored,
            scanned: status.files_scanned,
            indexed: status.entities_indexed,
            pending: status.files_pending,
            failed: status.files_failed,
            added: status.files_added,
            modified: status.files_modified,
            deleted: status.files_deleted,
            unchanged: status.files_unchanged,
            entities: status.entities_indexed,
            truncated_fragments: status.fragments_truncated,
        }
    }
}

/// Persistent block of the status output.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct PersistentStatusOutput {
    /// Daemon home directory.
    pub home: String,
    /// Workspace index storage path (empty when unindexed).
    pub index_path: String,
    /// Manifest identity, when an index exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_index: Option<StatusWorkspaceIndexOutput>,
    /// File counters, when an index exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files: Option<StatusFilesOutput>,
    /// Suggested next action, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<String>,
}

/// Live progress in status output.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct StatusProgressOutput {
    /// Lifecycle phase.
    pub phase: String,
    /// Files in scope, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files_total: Option<usize>,
    /// Files indexed, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files_indexed: Option<usize>,
    /// Files failed, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files_failed: Option<usize>,
    /// Detail message, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl From<&zg_core::types::IndexProgress> for StatusProgressOutput {
    fn from(progress: &zg_core::types::IndexProgress) -> Self {
        Self {
            phase: progress
                .phase
                .map(|phase| format!("{phase:?}").to_lowercase())
                .unwrap_or_else(|| "indexing".to_owned()),
            files_total: progress.files_total,
            files_indexed: progress.files_indexed,
            files_failed: progress.files_failed,
            detail: progress.detail.clone(),
        }
    }
}

/// Completion counters in status output.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct StatusCompletionOutput {
    /// Up-to-date files.
    pub completed: usize,
    /// Total files in scope.
    pub total: usize,
}

/// Runtime block of the status output (mirrors `formatIndexStatus`).
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct RuntimeStatusOutput {
    /// Filesystem watcher is active.
    pub watcher_active: bool,
    /// Dirty revision counter.
    pub dirty_revision: u64,
    /// Newest indexed revision counter.
    pub indexed_revision: u64,
    /// Active job id, when a job is live.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_job_id: Option<String>,
    /// Active job state, when a job is live.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_state: Option<JobStateOutput>,
    /// Live progress, when reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<StatusProgressOutput>,
    /// Completion overlay, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion: Option<StatusCompletionOutput>,
    /// Terminal job error, when failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<JobErrorOutput>,
}

/// `zvec_grep_index_status` structured output.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct IndexStatusOutput {
    /// Status root.
    pub root: String,
    /// Whether an index covers the root.
    pub indexed: bool,
    /// Index policy.
    pub index_policy: IndexPolicyOutput,
    /// Result source.
    pub source: IndexSourceOutput,
    /// Persistent index state.
    pub persistent: PersistentStatusOutput,
    /// Live runtime state, when the daemon owns the root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<RuntimeStatusOutput>,
}

/// Model pool summary in server status output.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ServerModelsOutput {
    /// Resident models.
    pub loaded: usize,
    /// Active leases.
    pub active_leases: usize,
}

/// `zvec_grep_server_status` structured output.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ServerStatusOutput {
    /// Server version.
    pub version: String,
    /// Milliseconds since backend creation.
    pub uptime_ms: u64,
    /// Whether shutdown is underway.
    pub shutting_down: bool,
    /// Live root actors.
    pub active_runtimes: usize,
    /// Queued jobs.
    pub queued_jobs: usize,
    /// Running jobs.
    pub running_jobs: usize,
    /// Model pool summary.
    pub models: ServerModelsOutput,
}

/// Formats a daemon status into the MCP status output (mirrors TS
/// `formatIndexStatus`).
pub fn format_index_status(root: &str, status: &DaemonIndexStatus) -> IndexStatusOutput {
    let indexed = status.info.as_ref().is_some_and(|info| info.indexed);
    let job = status.job.as_ref();
    IndexStatusOutput {
        root: root.to_owned(),
        indexed,
        index_policy: status
            .info
            .as_ref()
            .and_then(|info| info.index_policy)
            .into(),
        source: if indexed {
            IndexSourceOutput::Index
        } else {
            IndexSourceOutput::Unindexed
        },
        persistent: PersistentStatusOutput {
            home: crate::logger::daemon_home().to_string_lossy().into_owned(),
            index_path: status
                .info
                .as_ref()
                .and_then(|info| info.workspace_index.as_ref())
                .map(|index| index.path.clone())
                .unwrap_or_default(),
            workspace_index: status
                .info
                .as_ref()
                .and_then(|info| info.workspace_index.as_ref())
                .map(|index| StatusWorkspaceIndexOutput {
                    id: index.id.clone(),
                    name: index.name.clone(),
                    path: index.path.clone(),
                    root_paths: index.root_paths.iter().map(RootPathOutput::from).collect(),
                    embedding: index.embedding.clone().flatten().as_ref().map(|schema| {
                        StatusEmbeddingOutput {
                            provider: schema.provider.clone(),
                            model: schema.model.clone(),
                            dimension: schema.dimension,
                            metric: format!("{:?}", schema.metric).to_lowercase(),
                        }
                    }),
                    index_version: index.index_version,
                    created_time: index.created_time.as_millis(),
                    updated_time: index.updated_time.as_millis(),
                }),
            files: Some(StatusFilesOutput::from(&status.status)),
            suggestion: status
                .info
                .as_ref()
                .and_then(|info| info.suggestion.clone()),
        },
        runtime: Some(RuntimeStatusOutput {
            watcher_active: status.watcher_active,
            dirty_revision: status.dirty_revision.get(),
            indexed_revision: status.indexed_revision.get(),
            active_job_id: job.map(|job| job.id.to_string()),
            job_state: job.map(|job| JobStateOutput::from(job.state)),
            progress: job
                .and_then(|job| job.progress.as_ref())
                .map(StatusProgressOutput::from),
            completion: status
                .completion
                .as_ref()
                .map(|completion| StatusCompletionOutput {
                    completed: completion.completed,
                    total: completion.total,
                }),
            error: job
                .and_then(|job| job.error.as_ref())
                .map(JobErrorOutput::from),
        }),
    }
}
