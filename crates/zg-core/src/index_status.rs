//! Index completion/refresh derivation helpers.
//!
//! Ports `engine/index-status.ts`: pure functions that project a persisted
//! [`WorkspaceIndexStatus`] (optionally overlaid with live [`IndexProgress`])
//! into completion counters for progress reporting.

use serde::{Deserialize, Serialize};

use crate::types::{IndexProgress, WorkspaceIndexStatus};

/// `{ completed, total }` progress counter pair.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexCompletion {
    pub completed: usize,
    pub total: usize,
}

/// Lifecycle state of an indexing job, mirroring the TS `"queued" | ...`
/// string union.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IndexJobState {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

/// True when the status reports any adds, modifications, deletions, pending
/// files, or failures that warrant a refresh.
pub fn index_status_needs_refresh(status: Option<&WorkspaceIndexStatus>) -> bool {
    status.is_some_and(|status| {
        status.files_added > 0
            || status.files_modified > 0
            || status.files_deleted > 0
            || status.files_pending > 0
            || status.files_failed > 0
    })
}

/// Derives `{ completed: filesUnchanged, total: filesScanned }` from a
/// persisted status; `None` when there is no status.
pub fn index_completion_from_status(
    status: Option<&WorkspaceIndexStatus>,
) -> Option<IndexCompletion> {
    status.map(|status| IndexCompletion {
        completed: status.files_unchanged,
        total: status.files_scanned,
    })
}

/// Overlays live progress onto a base completion.
///
/// Net succeeded files are `filesIndexed - filesFailed` (saturating at zero);
/// without a base completion the progress must carry both numbers, otherwise
/// the succeeded count is added to the base and clamped to its total.
pub fn merge_index_completion(
    completion: Option<&IndexCompletion>,
    progress: Option<&IndexProgress>,
) -> Option<IndexCompletion> {
    let succeeded = progress
        .and_then(|progress| progress.files_indexed)
        .map(|indexed| {
            indexed.saturating_sub(
                progress
                    .and_then(|progress| progress.files_failed)
                    .unwrap_or(0),
            )
        });
    let Some(base) = completion else {
        let total = progress.and_then(|progress| progress.files_total)?;
        return succeeded.map(|completed| IndexCompletion { completed, total });
    };
    Some(IndexCompletion {
        completed: base.total.min(base.completed + succeeded.unwrap_or(0)),
        total: base.total,
    })
}

/// Applies the progress overlay only while the job is running; any other
/// state (or no state) returns the base completion unchanged.
pub fn index_completion_for_job(
    completion: Option<IndexCompletion>,
    state: Option<IndexJobState>,
    progress: Option<&IndexProgress>,
) -> Option<IndexCompletion> {
    if state == Some(IndexJobState::Running) {
        merge_index_completion(completion.as_ref(), progress)
    } else {
        completion
    }
}
