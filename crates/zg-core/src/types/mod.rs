//! Core domain types shared across the engine, mirrored from the TypeScript
//! `src/engine/types.ts`. JSON shapes use camelCase field names to stay
//! byte-compatible with indexes and configs written by the TS implementation.

mod content;
mod diagnostics;
mod entity;
mod file;
mod indexing;
mod search;
mod workspace;

pub use content::{Content, ImageFormat};
pub use diagnostics::EntitySearchDiagnosis;
pub use entity::{
    CodeEntityMetadata, CodeEntityModifier, CodeSymbolType, Entity, EntityFragment, EntityMetadata,
    MarkdownEntityMetadata, Range,
};
pub use file::{
    FileFormat, FileIndexStatus, FileInfo, FileKind, FileScanDiagnostics, RootPath, SkippedFile,
    SkippedFileReason,
};
pub use indexing::{
    EmbeddingStage, IndexEmbeddingProgress, IndexProgress, IndexProgressPhase, IndexResult,
    TimingEntry, WorkspaceIndexStatus,
};
pub use search::{
    ResolvedSearchPlan, ResolvedSearchPlanRoute, SearchFinalTrace, SearchHit, SearchHitEvidence,
    SearchHitTrace, SearchMatchedBy, SearchPlan, SearchPlanResult, SearchPlanRoute,
    SearchPlanRouteMode, SearchRecallTrace, SearchStageTrace,
};
pub use workspace::{
    CURRENT_INDEX_VERSION, SearchMetric, WorkspaceIndexEmbeddingSchema, WorkspaceIndexInfo,
    WorkspaceIndexPolicy, double_option,
};

/// Milliseconds since the Unix epoch.
///
/// The field is private so wall-clock values flow through [`UnixMillis::now`]
/// or the explicitly-untrusted [`UnixMillis::from_millis`]; readers use
/// [`UnixMillis::as_millis`] (M2).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default,
)]
pub struct UnixMillis(i64);

impl UnixMillis {
    /// Current wall-clock time.
    #[must_use]
    pub fn now() -> Self {
        Self(chrono::Utc::now().timestamp_millis())
    }

    /// Wraps a raw millisecond count (file mtimes, wire values).
    #[must_use]
    pub fn from_millis(millis: i64) -> Self {
        Self(millis)
    }

    /// The wrapped millisecond count.
    #[must_use]
    pub fn as_millis(self) -> i64 {
        self.0
    }
}

impl std::fmt::Display for UnixMillis {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

use serde::{Deserialize, Serialize};
