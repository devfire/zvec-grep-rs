//! Submit reason: drives dedupe absorption and queue priority.

/// Why an index job was submitted. Priority order mirrors TS `priority`:
/// manual (4) > fresh-query (3) > watch (2) > reconcile/background (1).
/// Serialized snake_case (`fresh_query`), matching the TS wire strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JobReason {
    /// Filesystem watcher event.
    Watch,
    /// Periodic or resume reconciliation.
    Reconcile,
    /// Low-priority background reconciliation.
    BackgroundReconcile,
    /// Explicit user/CLI request.
    Manual,
    /// A query observed a stale index.
    FreshQuery,
}

impl JobReason {
    /// Queue priority: higher runs first; ties break by creation time.
    pub(crate) const fn priority(self) -> u32 {
        match self {
            Self::Manual => 4,
            Self::FreshQuery => 3,
            Self::Watch => 2,
            Self::Reconcile | Self::BackgroundReconcile => 1,
        }
    }
}
