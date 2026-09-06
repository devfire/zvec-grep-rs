//! Code extractor: tree-sitter AST walk, fragmenting, outline generation.

pub mod adapter;
pub mod extractor;
pub mod families;
pub mod languages;

use crate::error::EngineResult;
use crate::extraction::{ChunkOptions, ExtractedFragment};
use crate::types::FileInfo;

/// Structural code extraction; see [`extractor`].
pub fn extract_for_indexing(
    file: &FileInfo,
    text: &str,
    options: &ChunkOptions,
) -> EngineResult<Option<Vec<ExtractedFragment>>> {
    extractor::extract_for_indexing(file, text, options)
}
