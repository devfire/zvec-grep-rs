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

    /// Fallible wall-clock read: `None` when the system clock predates the
    /// Unix epoch (`duration_since` fails) or overflows `i64` millis.
    ///
    /// This is the single clock authority behind every `*_ms` timestamp:
    /// the eight byte-identical `now_ms` copies (plus the nonce seed in
    /// `request_state`) routed through here, so a broken clock surfaces as
    /// `None` instead of a silent `0` sentinel. Each call site documents
    /// its own failure direction; security/TTL paths fail closed on `None`.
    #[must_use]
    pub fn try_now() -> Option<Self> {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .and_then(|elapsed| i64::try_from(elapsed.as_millis()).ok())
            .map(Self)
    }

    /// Best-effort millis for non-security paths (displays, idle eviction,
    /// progress seeds): `fallback` when [`UnixMillis::try_now`] is `None`.
    /// Security/TTL paths must use `try_now` and fail closed instead.
    #[must_use]
    pub fn now_ms_or(fallback: u64) -> u64 {
        // `try_now` only yields non-negative values, so the `as u64` cast
        // is exact; the fallback is chosen (and justified) per call site.
        Self::try_now()
            .map(|stamped| stamped.as_millis() as u64)
            .unwrap_or(fallback)
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
