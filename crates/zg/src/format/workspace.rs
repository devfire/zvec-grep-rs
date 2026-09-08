//! Workspace index state for `status` (`format/status.ts`).
//!
//! [`WorkspaceState`] mirrors `WorkspaceIndexState`.
//! [`format_workspace_info`] renders the labeled lines and returns the
//! state for `--check-ready`. Field painting reuses the shared
//! [`human_field`](super::fields) helper so the status view cannot drift
//! from the human context view.

use zg_core::service::types::ZvecGrepInfoResult;

use super::fields::human_field;

/// Workspace index state for `status`, mirroring `WorkspaceIndexState`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceState {
    /// Fresh and searchable.
    Ready,
    /// Behind the filesystem.
    Stale,
    /// Latest run failed.
    Failed,
    /// Indexing disabled by policy.
    Disabled,
    /// No index yet.
    Unindexed,
    /// No status recorded.
    Undecided,
}

impl WorkspaceState {
    /// Wire string for messages and `--check-ready` output.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Stale => "stale",
            Self::Failed => "failed",
            Self::Disabled => "disabled",
            Self::Unindexed => "unindexed",
            Self::Undecided => "undecided",
        }
    }
}

/// Derives the workspace state from an info result.
#[must_use]
pub fn workspace_state(info: &ZvecGrepInfoResult) -> WorkspaceState {
    if info.index_policy == Some(zg_core::types::WorkspaceIndexPolicy::Disabled) {
        return WorkspaceState::Disabled;
    }
    if !info.indexed {
        return WorkspaceState::Unindexed;
    }
    let Some(status) = &info.status else {
        return WorkspaceState::Undecided;
    };
    if !status.failed_files.is_empty() {
        return WorkspaceState::Failed;
    }
    if status.files_pending > 0 || !status.pending_files.is_empty() {
        return WorkspaceState::Stale;
    }
    WorkspaceState::Ready
}

/// Renders workspace info as labeled lines; returns the state for
/// `--check-ready`.
#[must_use]
pub fn format_workspace_info(info: &ZvecGrepInfoResult, color: bool) -> (String, WorkspaceState) {
    let state = workspace_state(info);
    let mut lines = vec![
        human_field("root", &info.root, color),
        human_field("state", state.as_str(), color),
    ];
    if let Some(embedding) = &info.embedding {
        let metric = format!("{:?}", embedding.metric);
        lines.push(human_field(
            "embedding",
            &format!(
                "{}/{} (dim {}, {metric})",
                embedding.provider, embedding.model, embedding.dimension
            ),
            color,
        ));
    }
    if let Some(status) = &info.status {
        lines.push(human_field(
            "files",
            &format!(
                "{} stored, {} entities",
                status.files_stored, status.entities_indexed
            ),
            color,
        ));
        if status.files_failed > 0 {
            lines.push(human_field(
                "failed",
                &status.files_failed.to_string(),
                color,
            ));
        }
        if status.files_pending > 0 {
            lines.push(human_field(
                "pending",
                &status.files_pending.to_string(),
                color,
            ));
        }
    }
    if let Some(suggestion) = &info.suggestion {
        lines.push(human_field("suggestion", suggestion, color));
    }
    (lines.join("\n"), state)
}

/// Prints workspace info; returns the state.
pub fn print_workspace_info(info: &ZvecGrepInfoResult, color: bool) -> WorkspaceState {
    let (text, state) = format_workspace_info(info, color);
    println!("{text}");
    state
}
