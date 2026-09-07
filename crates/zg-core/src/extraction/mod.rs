//! File → entity-fragment extraction.
//!
//! Routing mirrors `extraction/runtime.ts`: image → pass-through, code →
//! tree-sitter walk, markdown → sectioning, everything else → plain-text
//! chunking. Unsupported code formats and empty extractions fall back to the
//! plain-text chunker.

pub mod code;
pub mod image;
pub mod markdown;
pub mod text;
pub mod vector_content;

use crate::error::EngineResult;
use crate::ids::FileId;
use crate::types::{Content, EntityFragment, FileInfo, ImageFormat};

/// Chunking limits for the text-bearing extractors.
///
/// Defaults: max 3600 chars, overlap 540 chars.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ChunkOptions {
    pub max_chunk_chars: Option<usize>,
    pub overlap_chars: Option<usize>,
}

impl ChunkOptions {
    pub const DEFAULT_MAX_CHUNK_CHARS: usize = 3600;
    pub const DEFAULT_OVERLAP_CHARS: usize = 540;

    pub fn max_chunk_chars(&self) -> usize {
        self.max_chunk_chars
            .unwrap_or(Self::DEFAULT_MAX_CHUNK_CHARS)
    }

    pub fn overlap_chars(&self) -> usize {
        self.overlap_chars.unwrap_or(Self::DEFAULT_OVERLAP_CHARS)
    }
}

/// A file plus its decoded content, ready for extraction (borrowed, zero-copy).
#[derive(Debug)]
pub enum Source<'a> {
    Text {
        file: &'a FileInfo,
        text: &'a str,
    },
    Image {
        file: &'a FileInfo,
        data: &'a [u8],
        format: ImageFormat,
    },
}

impl Source<'_> {
    pub fn file(&self) -> &FileInfo {
        match self {
            Self::Text { file, .. } | Self::Image { file, .. } => file,
        }
    }
}

/// One extraction output: a fragment plus the optional content that should be
/// embedded for it (code route compacts whitespace; other routes embed the
/// fragment's own text).
#[derive(Debug, Clone, PartialEq)]
pub struct ExtractedFragment {
    pub fragment: EntityFragment,
    pub embedding_source: Option<Content>,
}

/// Extracts fragments exactly as stored in the index.
pub fn extract(source: &Source<'_>, options: &ChunkOptions) -> EngineResult<Vec<EntityFragment>> {
    extract_internal(source, options).map(|out| out.into_fragments())
}

/// Extracts fragments plus per-fragment embedding content for indexing.
pub fn extract_for_indexing(
    source: &Source<'_>,
    options: &ChunkOptions,
) -> EngineResult<Vec<ExtractedFragment>> {
    extract_internal(source, options).map(|out| out.fragments)
}

/// Internal carrier so both public functions share one routing pass.
struct ExtractionOutput {
    fragments: Vec<ExtractedFragment>,
}

impl ExtractionOutput {
    fn into_fragments(self) -> Vec<EntityFragment> {
        self.fragments.into_iter().map(|f| f.fragment).collect()
    }
}

fn extract_internal(source: &Source<'_>, options: &ChunkOptions) -> EngineResult<ExtractionOutput> {
    validate_source_file(source.file())?;
    let file = source.file();
    let plain = |text: &str| -> EngineResult<ExtractionOutput> {
        let fragments = text::extract_fragments(file, text, options)?;
        Ok(ExtractionOutput {
            fragments: fragments
                .into_iter()
                .map(|fragment| ExtractedFragment {
                    embedding_source: None,
                    fragment,
                })
                .collect(),
        })
    };
    match source {
        Source::Image { data, format, .. } => Ok(ExtractionOutput {
            fragments: vec![ExtractedFragment {
                embedding_source: None,
                fragment: image::extract_fragment(file, data, *format)?,
            }],
        }),
        Source::Text { text, .. } => {
            if file.kind.is_code() {
                if let Some(fragments) = code::extract_for_indexing(file, text, options)? {
                    return Ok(ExtractionOutput { fragments });
                }
                return plain(text);
            }
            if file.format.as_str() == "markdown" {
                if let Some(fragments) = markdown::extract_for_indexing(file, text, options)? {
                    return Ok(ExtractionOutput { fragments });
                }
                return plain(text);
            }
            plain(text)
        }
    }
}

/// Validates the file identity fields shared by every extractor.
pub(crate) fn validate_source_file(file: &FileInfo) -> EngineResult<()> {
    use crate::error::{EngineError, codes};

    if file.id.as_str().trim().is_empty() {
        return Err(EngineError::new(
            codes::extractor_empty_file_id(),
            "source file id must not be empty",
        ));
    }
    if file.absolute_path.trim().is_empty() {
        return Err(EngineError::new(
            codes::extractor_empty_absolute_path(),
            "source absolute path must not be empty",
        ));
    }
    if file.relative_path.trim().is_empty() {
        return Err(EngineError::new(
            codes::extractor_empty_relative_path(),
            "source relative path must not be empty",
        ));
    }
    Ok(())
}

/// Computes the sequential entity id for the fragment at `index` within `file`.
pub(crate) fn make_entity_id(file_id: &FileId, index: usize) -> crate::ids::EntityId {
    crate::ids::make_entity_id(file_id, index)
}
