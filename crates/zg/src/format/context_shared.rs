//! Shared context-rendering pieces: empty-result details and source labels.
//!
//! Imported by the agent/human renderers and the `print_*` dispatch, so
//! the two layouts cannot drift on empty reasons or source names.

use zg_core::service::types::{ContextCoverage, ContextSource, ZvecGrepContextResult};

/// Empty-result detail lines: the engine reason plus the `--rg` fallback tip.
pub(crate) fn empty_detail_lines(result: &ZvecGrepContextResult) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(reason) = &result.diagnostics.empty_reason {
        lines.push(reason.clone());
    }
    if result.source == ContextSource::Index && result.coverage == ContextCoverage::RankedSample {
        lines.push("Try --rg for exhaustive lexical search.".to_owned());
    }
    lines
}

/// Wire source name for the human view; every variant spelled out so a new
/// source fails compilation here instead of rendering blank.
pub(crate) fn source_label(source: &ContextSource) -> String {
    match source {
        ContextSource::Index => "index".to_owned(),
        ContextSource::Rg => "rg".to_owned(),
    }
}
