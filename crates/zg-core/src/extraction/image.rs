//! Image pass-through extractor: one file-range fragment with raw bytes.
//!
//! Mirrors `engine/extraction/image/extractor.ts`: validates the source file,
//! rejects empty data, and emits a single fragment whose range covers the
//! whole file and whose content carries the raw image bytes.

use crate::error::{EngineError, EngineResult, codes};
use crate::extraction::{make_entity_id, validate_source_file};
use crate::types::{Content, Entity, EntityFragment, FileInfo, ImageFormat, Range};

/// Extracts the single file-range fragment for image `data`.
pub fn extract_fragment(
    file: &FileInfo,
    data: &[u8],
    format: ImageFormat,
) -> EngineResult<EntityFragment> {
    validate_source_file(file)?;

    if data.is_empty() {
        return Err(EngineError::new(
            codes::extractor("IMAGE_EMPTY_DATA"),
            "Image extractor requires non-empty image data",
        )
        .with_context(format!("fileId={} format={}", file.id, format_name(format))));
    }

    Ok(EntityFragment {
        entity: Entity {
            id: make_entity_id(&file.id, 0),
            file_id: file.id.clone(),
            range: Range::File,
            content: Content::Image {
                data: data.to_vec(),
                format,
            },
            metadata: None,
        },
        group: None,
    })
}

fn format_name(format: ImageFormat) -> &'static str {
    match format {
        ImageFormat::Png => "png",
        ImageFormat::Jpeg => "jpeg",
        ImageFormat::Webp => "webp",
        ImageFormat::Gif => "gif",
    }
}
