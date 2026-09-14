//! Entity fragment <-> zvec document field encoding.
//!
//! Ports `fragmentToFields`, `contentToFields`, `metadataToFields`,
//! `parseContent`, `parseMetadata`, `readCodeModifiers`, `fragmentToEntity`,
//! `publicEntityId(s)`, and `validateFragmentGroups` from
//! `engine/storage/zvec.ts`.
//!
//! Divergences: range JSON round-trips through the Rust [`Range`] serde
//! shape (snake_case); image payloads use a strict built-in base64 codec
//! instead of `Buffer`.

use std::collections::{HashMap, HashSet};

use zvec_rust::Doc;

use crate::error::{EngineError, EngineErrorCode, EngineResult};
use crate::ids::{EntityId, FileId};
use crate::types::{
    CodeEntityModifier, CodeSymbolType, Content, Entity, EntityFragment, EntityMetadata, FileInfo,
    ImageFormat, Range,
};

use super::schema::{ENTITY_TEXT_FIELD, ENTITY_VECTOR_FIELD};

/// A decoded fragment joined with its owning file.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredFragment {
    pub fragment: EntityFragment,
    pub file: FileInfo,
}

/// Encodes one fragment plus its embedding vector as a zvec document.
/// `fragment_index` is the fragment's position within its file.
///
/// # Errors
///
/// Returns `STORAGE.DOC_ENCODE_FAILED` when the fragment id contains a null byte, a document
/// field cannot be set, or the entity range cannot be serialized.
pub fn fragment_to_doc(
    file: &FileInfo,
    fragment: &EntityFragment,
    vector: &[f32],
    fragment_index: i32,
) -> EngineResult<Doc> {
    let pk = fragment.entity.id.as_str();
    if pk.contains('\0') {
        return Err(EngineError::new(
            EngineErrorCode::from_static("STORAGE.DOC_ENCODE_FAILED"),
            "fragment id contains a null byte",
        )
        .with_context(format!("fragmentId={pk}")));
    }
    let mut doc = Doc::new().map_err(|error| {
        EngineError::new(
            EngineErrorCode::from_static("STORAGE.DOC_ENCODE_FAILED"),
            "failed to create entity document",
        )
        .with_context(format!("fragmentId={pk} error={error}"))
    })?;
    // `set_pk` panics on interior null bytes; guarded above.
    doc.set_pk(pk);
    let entity = &fragment.entity;
    add_optional_string(&mut doc, pk, "group", fragment.group.as_deref())?;
    add_string(&mut doc, pk, "file_id", entity.file_id.as_str())?;
    add_string(
        &mut doc,
        pk,
        "content_kind",
        match entity.content {
            Content::Text { .. } => "text",
            Content::Image { .. } => "image",
        },
    )?;
    add_optional_string(&mut doc, pk, "content_hash", file.content_hash.as_deref())?;
    encode_metadata(&mut doc, pk, entity.metadata.as_ref())?;
    doc.add_i32("fragment_index", fragment_index)
        .map_err(|error| doc_field_error(pk, "fragment_index", &error.to_string()))?;
    let range_json = serde_json::to_string(&entity.range).map_err(|error| {
        EngineError::new(
            EngineErrorCode::from_static("STORAGE.DOC_ENCODE_FAILED"),
            "failed to serialize entity range",
        )
        .with_context(format!("fragmentId={pk} error={error}"))
    })?;
    add_string(&mut doc, pk, "range_json", &range_json)?;
    match &entity.content {
        Content::Text { text } => {
            add_string(&mut doc, pk, ENTITY_TEXT_FIELD, text)?;
        }
        Content::Image { data, format } => {
            add_string(
                &mut doc,
                pk,
                ENTITY_TEXT_FIELD,
                &format!("[image:{}]", image_format_value(*format)),
            )?;
            add_string(&mut doc, pk, "content_base64", &base64_encode(data))?;
            add_string(&mut doc, pk, "image_format", image_format_value(*format))?;
        }
    }
    doc.add_vector_f32(ENTITY_VECTOR_FIELD, vector)
        .map_err(|error| doc_field_error(pk, ENTITY_VECTOR_FIELD, &error.to_string()))?;
    Ok(doc)
}

/// Decodes a stored document, returning `None` when its file is unknown
/// (mirrors the TypeScript `null` for orphan documents).
///
/// # Errors
///
/// Returns `STORAGE.DOC_DECODE_FAILED` when the primary key, a required field, or the range or
/// image payload is missing or invalid, or `STORAGE.UNSUPPORTED_STORED_CONTENT_KIND` for unknown
/// content kinds or image formats.
pub fn doc_to_stored_fragment(
    doc: &Doc,
    files_by_id: &HashMap<String, FileInfo>,
) -> EngineResult<Option<StoredFragment>> {
    let pk = doc.get_pk().unwrap_or_default().to_owned();
    if pk.is_empty() {
        return Err(EngineError::new(
            EngineErrorCode::from_static("STORAGE.DOC_DECODE_FAILED"),
            "stored entity document has no primary key",
        ));
    }
    let file_id = optional_string_field(doc, "file_id", &pk)?;
    let Some(file_id) = file_id.filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let Some(file) = files_by_id.get(&file_id) else {
        return Ok(None);
    };
    let group = optional_string_field(doc, "group", &pk)?.filter(|value| !value.is_empty());
    let range_json = required_string_field(doc, "range_json", &pk)?;
    let range: Range = serde_json::from_str(&range_json).map_err(|error| {
        EngineError::new(
            EngineErrorCode::from_static("STORAGE.DOC_DECODE_FAILED"),
            "stored entity has an invalid range",
        )
        .with_context(format!("fragmentId={pk} error={error}"))
    })?;
    let content = parse_content(doc, &pk)?;
    let metadata = parse_metadata(doc, &pk)?;
    Ok(Some(StoredFragment {
        fragment: EntityFragment {
            entity: Entity {
                id: EntityId::from_raw(pk),
                file_id: FileId::from_raw(file_id),
                range,
                content,
                metadata,
            },
            group,
        },
        file: file.clone(),
    }))
}

/// Collapses a fragment to its public entity (group id when set).
#[must_use]
pub fn fragment_to_entity(fragment: &EntityFragment) -> Entity {
    Entity {
        id: EntityId::from_raw(public_entity_id(fragment).to_owned()),
        file_id: fragment.entity.file_id.clone(),
        range: fragment.entity.range.clone(),
        content: fragment.entity.content.clone(),
        metadata: fragment.entity.metadata.clone(),
    }
}

/// Public identity used for group collapse.
#[must_use]
pub fn public_entity_id(fragment: &EntityFragment) -> &str {
    fragment
        .group
        .as_deref()
        .unwrap_or_else(|| fragment.entity.id.as_str())
}

/// Deduplicated public ids of fragments that own their group (majors and
/// ungrouped fragments), preserving first-seen order.
pub fn public_entity_ids<'a>(
    fragments: impl IntoIterator<Item = &'a EntityFragment>,
) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut ids = Vec::new();
    for fragment in fragments {
        let is_major = fragment
            .group
            .as_deref()
            .is_none_or(|group| group == fragment.entity.id.as_str());
        if is_major && seen.insert(public_entity_id(fragment).to_owned()) {
            ids.push(public_entity_id(fragment).to_owned());
        }
    }
    ids
}

/// Rejects fragments from another file, duplicate ids, and groups without
/// exactly one major fragment.
///
/// # Errors
///
/// Returns `STORAGE.FRAGMENT_FILE_MISMATCH` for fragments from another file,
/// `STORAGE.DUPLICATE_FRAGMENT_ID` for repeated ids, or `STORAGE.INVALID_FRAGMENT_GROUP` when a
/// group lacks exactly one major fragment.
pub fn validate_fragment_groups<'a>(
    file_id: &FileId,
    fragments: impl IntoIterator<Item = &'a EntityFragment>,
) -> EngineResult<()> {
    let mut ids = HashSet::new();
    let mut groups: HashMap<&str, Vec<&EntityFragment>> = HashMap::new();
    for fragment in fragments {
        if fragment.entity.file_id != *file_id {
            return Err(EngineError::new(
                EngineErrorCode::from_static("STORAGE.FRAGMENT_FILE_MISMATCH"),
                "entity fragment belongs to the wrong file",
            )
            .with_context(format!(
                "fileId={} fragmentId={} fragmentFileId={}",
                file_id.as_str(),
                fragment.entity.id.as_str(),
                fragment.entity.file_id.as_str()
            )));
        }
        if !ids.insert(fragment.entity.id.as_str()) {
            return Err(EngineError::new(
                EngineErrorCode::from_static("STORAGE.DUPLICATE_FRAGMENT_ID"),
                "duplicate entity fragment id",
            )
            .with_context(format!(
                "fileId={} fragmentId={}",
                file_id.as_str(),
                fragment.entity.id.as_str()
            )));
        }
        if let Some(group) = fragment.group.as_deref() {
            groups.entry(group).or_default().push(fragment);
        }
    }
    for (group_id, group) in &groups {
        let major_count = group
            .iter()
            .filter(|fragment| fragment.entity.id.as_str() == *group_id)
            .count();
        if major_count != 1 {
            return Err(EngineError::new(
                EngineErrorCode::from_static("STORAGE.INVALID_FRAGMENT_GROUP"),
                "fragment group must have exactly one major fragment",
            )
            .with_context(format!(
                "fileId={} group={group_id} majorCount={major_count}",
                file_id.as_str()
            )));
        }
    }
    Ok(())
}

fn encode_metadata(doc: &mut Doc, pk: &str, metadata: Option<&EntityMetadata>) -> EngineResult<()> {
    let Some(metadata) = metadata else {
        return Ok(());
    };
    match metadata {
        EntityMetadata::Code(code) => {
            add_string(&mut *doc, pk, "metadata_kind", "code")?;
            add_string(
                &mut *doc,
                pk,
                "symbol_type",
                symbol_type_value(code.symbol_type),
            )?;
            add_optional_string(&mut *doc, pk, "symbol_name", code.symbol_name.as_deref())?;
            add_optional_string(&mut *doc, pk, "symbol_scope", code.scope.as_deref())?;
            add_optional_string(&mut *doc, pk, "symbol_signature", code.signature.as_deref())?;
            add_optional_string(&mut *doc, pk, "symbol_doc", code.doc.as_deref())?;
            if !code.modifiers.is_empty() {
                let joined: Vec<&str> = code.modifiers.iter().map(modifier_value).collect();
                add_string(&mut *doc, pk, "symbol_modifiers", &joined.join(" "))?;
            }
            add_optional_string(&mut *doc, pk, "node_type", code.node_type.as_deref())?;
        }
        EntityMetadata::Markdown(markdown) => {
            add_string(&mut *doc, pk, "metadata_kind", "markdown")?;
            add_optional_string(&mut *doc, pk, "symbol_scope", markdown.scope.as_deref())?;
            add_optional_string(&mut *doc, pk, "heading", markdown.heading.as_deref())?;
            if let Some(level) = markdown.level {
                doc.add_i32("heading_level", level)
                    .map_err(|error| doc_field_error(pk, "heading_level", &error.to_string()))?;
            }
        }
    }
    Ok(())
}

fn parse_content(doc: &Doc, pk: &str) -> EngineResult<Content> {
    let kind = optional_string_field(doc, "content_kind", pk)?.unwrap_or_default();
    match kind.as_str() {
        "text" => Ok(Content::Text {
            text: optional_string_field(doc, ENTITY_TEXT_FIELD, pk)?.unwrap_or_default(),
        }),
        "image" => {
            let encoded = required_string_field(doc, "content_base64", pk)?;
            let format = required_string_field(doc, "image_format", pk)?;
            Ok(Content::Image {
                data: base64_decode(&encoded).map_err(|detail| {
                    EngineError::new(
                        EngineErrorCode::from_static("STORAGE.DOC_DECODE_FAILED"),
                        "stored entity has invalid image data",
                    )
                    .with_context(format!("fragmentId={pk} error={detail}"))
                })?,
                format: parse_image_format(&format).ok_or_else(|| {
                    EngineError::new(
                        EngineErrorCode::from_static("STORAGE.UNSUPPORTED_STORED_CONTENT_KIND"),
                        "stored entity has unsupported image format",
                    )
                    .with_context(format!("fragmentId={pk} imageFormat={format}"))
                })?,
            })
        }
        _ => Err(EngineError::new(
            EngineErrorCode::from_static("STORAGE.UNSUPPORTED_STORED_CONTENT_KIND"),
            "stored entity has unsupported content kind",
        )
        .with_context(format!("fragmentId={pk} contentKind={kind}"))),
    }
}

fn parse_metadata(doc: &Doc, pk: &str) -> EngineResult<Option<EntityMetadata>> {
    let kind = optional_string_field(doc, "metadata_kind", pk)?.unwrap_or_default();
    match kind.as_str() {
        "" => Ok(None),
        "code" => {
            let symbol_type = required_string_field(doc, "symbol_type", pk)?;
            Ok(Some(EntityMetadata::Code(
                crate::types::CodeEntityMetadata {
                    symbol_type: parse_symbol_type(&symbol_type).ok_or_else(|| {
                        EngineError::new(
                            EngineErrorCode::from_static("STORAGE.DOC_DECODE_FAILED"),
                            "stored entity has unsupported symbol type",
                        )
                        .with_context(format!("fragmentId={pk} symbolType={symbol_type}"))
                    })?,
                    symbol_name: optional_string_field(doc, "symbol_name", pk)?
                        .filter(|value| !value.is_empty()),
                    scope: optional_string_field(doc, "symbol_scope", pk)?
                        .filter(|value| !value.is_empty()),
                    node_type: optional_string_field(doc, "node_type", pk)?
                        .filter(|value| !value.is_empty()),
                    signature: optional_string_field(doc, "symbol_signature", pk)?
                        .filter(|value| !value.is_empty()),
                    doc: optional_string_field(doc, "symbol_doc", pk)?
                        .filter(|value| !value.is_empty()),
                    modifiers: parse_modifiers(
                        optional_string_field(doc, "symbol_modifiers", pk)?
                            .as_deref()
                            .unwrap_or_default(),
                    ),
                },
            )))
        }
        "markdown" => Ok(Some(EntityMetadata::Markdown(
            crate::types::MarkdownEntityMetadata {
                heading: optional_string_field(doc, "heading", pk)?
                    .filter(|value| !value.is_empty()),
                level: optional_i32_field(doc, "heading_level", pk)?.filter(|value| *value > 0),
                scope: optional_string_field(doc, "symbol_scope", pk)?
                    .filter(|value| !value.is_empty()),
            },
        ))),
        _ => Ok(None),
    }
}

fn parse_modifiers(value: &str) -> Vec<CodeEntityModifier> {
    value
        .split_ascii_whitespace()
        .filter_map(|token| match token {
            "exported" => Some(CodeEntityModifier::Exported),
            "async" => Some(CodeEntityModifier::Async),
            "static" => Some(CodeEntityModifier::Static),
            "public" => Some(CodeEntityModifier::Public),
            "private" => Some(CodeEntityModifier::Private),
            "protected" => Some(CodeEntityModifier::Protected),
            "internal" => Some(CodeEntityModifier::Internal),
            _ => None,
        })
        .collect()
}

fn symbol_type_value(symbol_type: CodeSymbolType) -> &'static str {
    match symbol_type {
        CodeSymbolType::Module => "module",
        CodeSymbolType::Class => "class",
        CodeSymbolType::Interface => "interface",
        CodeSymbolType::Function => "function",
        CodeSymbolType::Value => "value",
        CodeSymbolType::Alias => "alias",
    }
}

fn parse_symbol_type(value: &str) -> Option<CodeSymbolType> {
    match value {
        "module" => Some(CodeSymbolType::Module),
        "class" => Some(CodeSymbolType::Class),
        "interface" => Some(CodeSymbolType::Interface),
        "function" => Some(CodeSymbolType::Function),
        "value" => Some(CodeSymbolType::Value),
        "alias" => Some(CodeSymbolType::Alias),
        _ => None,
    }
}

fn modifier_value(modifier: &CodeEntityModifier) -> &'static str {
    match modifier {
        CodeEntityModifier::Exported => "exported",
        CodeEntityModifier::Async => "async",
        CodeEntityModifier::Static => "static",
        CodeEntityModifier::Public => "public",
        CodeEntityModifier::Private => "private",
        CodeEntityModifier::Protected => "protected",
        CodeEntityModifier::Internal => "internal",
    }
}

fn image_format_value(format: ImageFormat) -> &'static str {
    match format {
        ImageFormat::Png => "png",
        ImageFormat::Jpeg => "jpeg",
        ImageFormat::Webp => "webp",
        ImageFormat::Gif => "gif",
    }
}

fn parse_image_format(value: &str) -> Option<ImageFormat> {
    match value {
        "png" => Some(ImageFormat::Png),
        "jpeg" => Some(ImageFormat::Jpeg),
        "webp" => Some(ImageFormat::Webp),
        "gif" => Some(ImageFormat::Gif),
        _ => None,
    }
}

fn add_string(doc: &mut Doc, pk: &str, field: &str, value: &str) -> EngineResult<()> {
    doc.add_string(field, value)
        .map_err(|error| doc_field_error(pk, field, &error.to_string()))
}

fn add_optional_string(
    doc: &mut Doc,
    pk: &str,
    field: &str,
    value: Option<&str>,
) -> EngineResult<()> {
    if let Some(value) = value {
        add_string(doc, pk, field, value)?;
    }
    Ok(())
}

fn optional_string_field(doc: &Doc, field: &str, pk: &str) -> EngineResult<Option<String>> {
    // Unset fields are absent from the document (writers skip `None`), and
    // reading an absent field errors — so absence is `None`, not failure.
    if !doc.has_field(field) || doc.is_field_null(field) {
        return Ok(None);
    }
    doc.get_string(field)
        .map_err(|error| doc_field_error(pk, field, &error.to_string()))
}

fn required_string_field(doc: &Doc, field: &str, pk: &str) -> EngineResult<String> {
    match optional_string_field(doc, field, pk)? {
        Some(value) if !value.is_empty() => Ok(value),
        _ => Err(EngineError::new(
            EngineErrorCode::from_static("STORAGE.DOC_DECODE_FAILED"),
            "stored entity document is missing a required field",
        )
        .with_context(format!("fragmentId={pk} field={field}"))),
    }
}

fn optional_i32_field(doc: &Doc, field: &str, pk: &str) -> EngineResult<Option<i32>> {
    if !doc.has_field(field) || doc.is_field_null(field) {
        return Ok(None);
    }
    doc.get_i32(field)
        .map_err(|error| doc_field_error(pk, field, &error.to_string()))
}

fn doc_field_error(pk: &str, field: &str, detail: &str) -> EngineError {
    EngineError::new(
        EngineErrorCode::from_static("STORAGE.DOC_FIELD_FAILED"),
        "entity document field operation failed",
    )
    .with_context(format!("fragmentId={pk} field={field} error={detail}"))
}

const BASE64_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard-base64 encoding without external dependencies.
#[must_use]
pub fn base64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let Some((&b0, rest)) = chunk.split_first() else {
            continue;
        };
        let b1 = rest.first().copied().unwrap_or(0);
        let b2 = rest.get(1).copied().unwrap_or(0);
        let triple = (u32::from(b0) << 16) | (u32::from(b1) << 8) | u32::from(b2);
        if let Some(&sextet) = BASE64_ALPHABET.get((triple >> 18) as usize & 63) {
            out.push(sextet as char);
        }
        if let Some(&sextet) = BASE64_ALPHABET.get((triple >> 12) as usize & 63) {
            out.push(sextet as char);
        }
        if chunk.len() > 1 {
            if let Some(&sextet) = BASE64_ALPHABET.get((triple >> 6) as usize & 63) {
                out.push(sextet as char);
            }
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            if let Some(&sextet) = BASE64_ALPHABET.get(triple as usize & 63) {
                out.push(sextet as char);
            }
        } else {
            out.push('=');
        }
    }
    out
}

/// Standard-base64 decoding; ASCII whitespace is ignored.
///
/// # Errors
///
/// Returns an error string for excess padding, data after padding, an invalid character, or a
/// truncated final quantum.
pub fn base64_decode(text: &str) -> Result<Vec<u8>, String> {
    let mut sextets: Vec<u8> = Vec::with_capacity(text.len().div_ceil(4) * 3);
    let mut padding = 0usize;
    for byte in text.bytes().filter(|b| !b.is_ascii_whitespace()) {
        if byte == b'=' {
            padding += 1;
            if padding > 2 {
                return Err("excess padding".to_owned());
            }
            sextets.push(0);
            continue;
        }
        if padding > 0 {
            return Err("data after padding".to_owned());
        }
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return Err(format!("invalid character 0x{byte:02x}")),
        };
        sextets.push(value);
    }
    if !sextets.len().is_multiple_of(4) {
        return Err("truncated input".to_owned());
    }
    if padding > 2 {
        return Err("excess padding".to_owned());
    }
    let mut out = Vec::with_capacity(sextets.len() / 4 * 3);
    for &[q0, q1, q2, q3] in sextets.as_chunks::<4>().0 {
        let triple =
            (u32::from(q0) << 18) | (u32::from(q1) << 12) | (u32::from(q2) << 6) | u32::from(q3);
        out.push((triple >> 16) as u8);
        out.push((triple >> 8) as u8);
        out.push(triple as u8);
    }
    for _ in 0..padding {
        out.pop();
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trip_covers_remainder_lengths() {
        for len in 0..12usize {
            let data: Vec<u8> = (0..len as u8).map(|b| b.wrapping_mul(37)).collect();
            let decoded = base64_decode(&base64_encode(&data));
            assert_eq!(decoded, Ok(data));
        }
    }

    #[test]
    fn base64_matches_known_vectors() {
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_decode("Zg=="), Ok(vec![b'f']));
        assert!(base64_decode("!!!").is_err());
    }

    #[test]
    fn modifiers_drop_unknown_tokens() {
        assert_eq!(
            parse_modifiers("exported bogus async"),
            vec![CodeEntityModifier::Exported, CodeEntityModifier::Async]
        );
        assert!(parse_modifiers("").is_empty());
    }
}
