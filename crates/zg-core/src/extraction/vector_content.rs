//! Embedding text assembly: metadata header + content within a 25% budget.
//!
//! Mirrors `engine/extraction/vector-content.ts`. Text fragments are embedded
//! as `<metadata lines>\n<content>` where the metadata header is capped at
//! 25% of the chunk budget; non-text content passes through untouched.
//!
//! Lengths count Unicode scalar values. The TypeScript original counts UTF-16
//! code units and guards against splitting surrogate pairs in
//! `fitTextToChars` — slicing by `char` boundary makes that guard vacuous
//! here.

use crate::types::{
    CodeEntityMetadata, CodeEntityModifier, CodeSymbolType, Content, EntityFragment,
    EntityMetadata, MarkdownEntityMetadata,
};

/// Fraction of the chunk budget reserved for the metadata header.
pub const MAX_METADATA_BUDGET_RATIO: f64 = 0.25;

/// Resolved chunk limits after subtracting the metadata header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedChunkOptions {
    pub max_chunk_chars: usize,
    pub chunk_overlap_chars: usize,
}

/// Builds the embedding text for `fragment`: the metadata header prepended to
/// the embedding content, or the content unchanged when there is no metadata.
/// Defaults to the fragment's own content when `embedding_content` is `None`.
pub fn vector_content_for_fragment(
    fragment: &EntityFragment,
    embedding_content: Option<&Content>,
    max_chars: Option<usize>,
) -> Content {
    let embedding = embedding_content.unwrap_or(&fragment.entity.content);
    let Content::Text { text } = embedding else {
        return embedding.clone();
    };

    let metadata = vector_metadata_text(
        fragment.entity.metadata.as_ref(),
        metadata_budget(max_chars),
    );
    if metadata.is_empty() {
        return embedding.clone();
    }

    Content::Text {
        text: format!("{metadata}\n{text}"),
    }
}

/// Shrinks chunk limits to leave room for the metadata header (mirrors
/// `chunkOptionsForMetadata`).
pub fn chunk_options_for_metadata(
    max_chunk_chars: usize,
    chunk_overlap_chars: usize,
    metadata: Option<&EntityMetadata>,
) -> ResolvedChunkOptions {
    let metadata_text = vector_metadata_text(metadata, metadata_budget(Some(max_chunk_chars)));
    let separator_chars = usize::from(!metadata_text.is_empty());
    let max_chunk_chars = max_chunk_chars
        .saturating_sub(metadata_text.chars().count() + separator_chars)
        .max(1);
    let chunk_overlap_chars = chunk_overlap_chars.min(max_chunk_chars.saturating_sub(1));
    ResolvedChunkOptions {
        max_chunk_chars,
        chunk_overlap_chars,
    }
}

/// Truncates to `max_chars` with a `...` marker (mirrors `fitTextToChars`).
pub fn fit_text_to_chars(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_owned();
    }
    if max_chars <= 3 {
        return ".".repeat(max_chars);
    }
    let raw_end = max_chars - 3;
    let end_byte: usize = value.chars().take(raw_end).map(|c| c.len_utf8()).sum();
    format!("{}...", value[..end_byte].trim_end())
}

fn vector_metadata_text(metadata: Option<&EntityMetadata>, max_chars: Option<usize>) -> String {
    let Some(metadata) = metadata else {
        return String::new();
    };
    match metadata {
        EntityMetadata::Code(meta) => compact_metadata_lines(&code_metadata_lines(meta), max_chars),
        EntityMetadata::Markdown(meta) => {
            compact_metadata_lines(&markdown_metadata_lines(meta), max_chars)
        }
    }
}

fn code_metadata_lines(meta: &CodeEntityMetadata) -> Vec<Option<String>> {
    vec![
        match &meta.symbol_name {
            Some(name) => Some(format!(
                "symbol: {} {name}",
                symbol_type_name(meta.symbol_type)
            )),
            None => Some(format!("symbol: {}", symbol_type_name(meta.symbol_type))),
        },
        meta.scope.clone().map(|scope| format!("scope: {scope}")),
        meta.signature
            .as_deref()
            .map(|signature| format!("signature: {}", one_line(signature))),
        if meta.modifiers.is_empty() {
            None
        } else {
            Some(format!(
                "modifiers: {}",
                meta.modifiers
                    .iter()
                    .map(|m| modifier_name(*m))
                    .collect::<Vec<_>>()
                    .join(" ")
            ))
        },
        meta.doc
            .as_deref()
            .map(|doc| format!("doc: {}", one_line(doc))),
    ]
}

fn markdown_metadata_lines(meta: &MarkdownEntityMetadata) -> Vec<Option<String>> {
    vec![
        meta.heading
            .clone()
            .map(|heading| format!("heading: {heading}")),
        meta.level.map(|level| format!("heading_level: {level}")),
        meta.scope.clone().map(|scope| format!("scope: {scope}")),
    ]
}

fn metadata_budget(max_chars: Option<usize>) -> Option<usize> {
    max_chars.map(|max| ((max as f64) * MAX_METADATA_BUDGET_RATIO).floor() as usize)
}

fn compact_metadata_lines(lines: &[Option<String>], max_chars: Option<usize>) -> String {
    let text = lines
        .iter()
        .filter_map(|line| line.as_deref())
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    match max_chars {
        None => text,
        Some(max) => fit_text_to_chars(&text, max),
    }
}

fn one_line(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn symbol_type_name(symbol_type: CodeSymbolType) -> &'static str {
    match symbol_type {
        CodeSymbolType::Module => "module",
        CodeSymbolType::Class => "class",
        CodeSymbolType::Interface => "interface",
        CodeSymbolType::Function => "function",
        CodeSymbolType::Value => "value",
        CodeSymbolType::Alias => "alias",
    }
}

fn modifier_name(modifier: CodeEntityModifier) -> &'static str {
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
