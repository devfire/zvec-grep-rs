//! Embedding pipeline: batching, scoped-thread waves, serial commit, fallback chain.

mod fragments;
mod orchestrator;
mod unit;
mod wave;

#[cfg(test)]
mod tests;

pub(crate) use orchestrator::index_files;

/// Embedded vectors for one file, or the stage-tagged failure reason.
struct FileEmbed {
    vectors: Vec<Vec<f32>>,
    truncated_fragment_count: usize,
}

/// Per-file embedding result: vectors on success, or the originating error on
/// non-fail-fast failure. The stage-tagged reason string is derived once at
/// commit time via `mark_file_failed`, so storage and stats stay consistent.
enum FileOutcome {
    Embedded(FileEmbed),
    Failed(crate::error::EngineError),
}

/// Outcome of embedding one unit (batch or oversized single file).
enum UnitOutcome {
    Embedded(Vec<FileOutcome>),
    Failed(crate::error::EngineError),
}
