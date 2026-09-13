//! Code extractor: tree-sitter AST walk, fragmenting, outline generation.

pub mod adapter;
pub mod extractor;
pub mod families;
pub mod languages;

use crate::error::EngineResult;
use crate::extraction::{ChunkOptions, ExtractedFragment};
use crate::types::FileInfo;

/// Structural code extraction; see [`extractor`].
///
/// # Errors
///
/// Returns `EXTRACTORS.CODE_INVALID_CHUNK_SIZE` when the chunk size is zero, or
/// `EXTRACTORS.CODE_INVALID_CHUNK_OVERLAP` when overlap is not smaller than the chunk size.
pub fn extract_for_indexing(
    file: &FileInfo,
    text: &str,
    options: &ChunkOptions,
) -> EngineResult<Option<Vec<ExtractedFragment>>> {
    extractor::extract_for_indexing(file, text, options)
}
