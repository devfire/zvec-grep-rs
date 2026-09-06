//! Tree-sitter code extractor: AST walk, entity fragmenting, outlines.
//!
//! Port of `engine/extraction/code/extractor.ts` (`CodeExtractor`). Structured
//! formats parse with the native grammar crates (the TS port uses WASM
//! grammars); component formats (`vue`, `svelte`) extract `<script>` blocks
//! and remap their fragments into host coordinates.
//!
//! Divergences from the TypeScript original:
//! - Budgets: TS counts UTF-16 code units (`string.length`); this port counts
//!   Unicode scalar values (`chars().count()`), identical for the BMP.
//! - Offsets in [`Range`] are byte offsets; TS reports UTF-16-unit offsets.
//! - `truncateInline` is dead code in the TS original and is not ported.

use std::collections::{HashMap, HashSet};

use super::adapter::{LanguageAdapter, SyntaxNode, resolve_adapter};
use super::families::js_ts::has_javascript_typescript_function_value;
use crate::code_formats::is_component_code_format;
use crate::error::{EngineError, EngineResult, codes};
use crate::extraction::text::extract_plain_text_fragments;
use crate::extraction::vector_content::chunk_options_for_metadata;
use crate::extraction::{ChunkOptions, ExtractedFragment};
use crate::ids::{EntityId, FileId, make_entity_id};
use crate::types::{
    CodeEntityMetadata, CodeEntityModifier, CodeSymbolType, Content, Entity, EntityFragment,
    EntityMetadata, FileInfo, Range,
};

const OUTLINE_MAX_MEMBERS: usize = 32;
const OUTLINE_MAX_CALLS: usize = 24;
const OUTLINE_MAX_LINE_CHARS: usize = 180;
const HEADER_MAX_CHARS: usize = 1200;
const HEADER_MAX_LINES: usize = 24;

/// Formats with a native grammar available (mirrors `LANGUAGE_WASM_MAP`).
fn has_grammar(format: &str) -> bool {
    matches!(
        format,
        "c" | "cpp"
            | "go"
            | "java"
            | "javascript"
            | "jsx"
            | "python"
            | "rust"
            | "tsx"
            | "typescript"
    )
}

fn language_for_format(format: &str) -> Option<tree_sitter::Language> {
    let language = match format {
        "c" => tree_sitter_c::LANGUAGE.into(),
        "cpp" => tree_sitter_cpp::LANGUAGE.into(),
        "go" => tree_sitter_go::LANGUAGE.into(),
        "java" => tree_sitter_java::LANGUAGE.into(),
        "javascript" | "jsx" => tree_sitter_javascript::LANGUAGE.into(),
        "python" => tree_sitter_python::LANGUAGE.into(),
        "rust" => tree_sitter_rust::LANGUAGE.into(),
        "typescript" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        "tsx" => tree_sitter_typescript::LANGUAGE_TSX.into(),
        _ => return None,
    };
    Some(language)
}

/// Entry point used by [`crate::extraction`]: structural fragments plus
/// per-fragment embedding content, or `None` when this file is not code.
pub fn extract_for_indexing(
    file: &FileInfo,
    text: &str,
    options: &ChunkOptions,
) -> EngineResult<Option<Vec<ExtractedFragment>>> {
    if !file.kind.is_code() {
        return Ok(None);
    }
    let (max_chunk_chars, chunk_overlap_chars) = resolve_code_chunk_options(options)?;

    if is_component_code_format(file.format.as_str()) {
        let fragments = extract_script_blocks(file, text, max_chunk_chars, chunk_overlap_chars)?;
        if fragments.is_empty() {
            return Ok(Some(fallback(
                file,
                text,
                max_chunk_chars,
                chunk_overlap_chars,
            )));
        }
        return Ok(Some(fragments));
    }

    let format = file.format.as_str();
    let Some(adapter) = resolve_adapter(format) else {
        return Ok(Some(fallback(
            file,
            text,
            max_chunk_chars,
            chunk_overlap_chars,
        )));
    };
    if !has_grammar(format) {
        return Ok(Some(fallback(
            file,
            text,
            max_chunk_chars,
            chunk_overlap_chars,
        )));
    }
    let Some(language) = language_for_format(format) else {
        return Ok(Some(fallback(
            file,
            text,
            max_chunk_chars,
            chunk_overlap_chars,
        )));
    };

    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(&language).is_err() {
        return Ok(Some(fallback(
            file,
            text,
            max_chunk_chars,
            chunk_overlap_chars,
        )));
    }
    let Some(tree) = parser.parse(text.as_bytes(), None) else {
        return Ok(Some(fallback(
            file,
            text,
            max_chunk_chars,
            chunk_overlap_chars,
        )));
    };

    let bytes = text.as_bytes();
    let root = SyntaxNode::new(tree.root_node(), bytes);
    let mut collected: Vec<CodeEntity<'_>> = Vec::new();
    walk_code_node(root, adapter, &[], &mut collected);

    let mut out = Vec::new();
    let mut entity_id_index = 0usize;
    for entity in &collected {
        let raws =
            code_entity_to_search_fragments(adapter, entity, max_chunk_chars, chunk_overlap_chars);
        let major_id = if raws.first().is_some_and(|raw| raw.mark == GroupMark::Major) {
            Some(make_entity_id(&file.id, entity_id_index))
        } else {
            None
        };
        for raw in raws {
            let id = make_entity_id(&file.id, entity_id_index);
            entity_id_index += 1;
            let group = match raw.mark {
                GroupMark::Major => Some(id.clone()),
                GroupMark::Single => None,
                GroupMark::Minor => major_id.clone(),
            };
            out.push(ExtractedFragment {
                fragment: EntityFragment {
                    entity: Entity {
                        id,
                        file_id: file.id.clone(),
                        range: raw.range,
                        content: Content::Text { text: raw.text },
                        metadata: Some(EntityMetadata::Code(raw.metadata)),
                    },
                    group: group.map(|id| id.as_str().to_owned()),
                },
                embedding_source: raw.embedding_text.map(|text| Content::Text { text }),
            });
        }
    }
    if out.is_empty() {
        return Ok(Some(fallback(
            file,
            text,
            max_chunk_chars,
            chunk_overlap_chars,
        )));
    }
    Ok(Some(out))
}

fn resolve_code_chunk_options(options: &ChunkOptions) -> EngineResult<(usize, usize)> {
    let max_chunk_chars = options.max_chunk_chars();
    let chunk_overlap_chars = options.overlap_chars();
    if max_chunk_chars == 0 {
        return Err(EngineError::new(
            codes::extractor("CODE_INVALID_CHUNK_SIZE"),
            "code extractor requires a positive integer chunk size",
        )
        .with_context(format!("maxChunkChars={max_chunk_chars}")));
    }
    if chunk_overlap_chars >= max_chunk_chars {
        return Err(EngineError::new(
            codes::extractor("CODE_INVALID_CHUNK_OVERLAP"),
            "code extractor requires overlap to be smaller than chunk size",
        )
        .with_context(format!(
            "maxChunkChars={max_chunk_chars} chunkOverlapChars={chunk_overlap_chars}"
        )));
    }
    Ok((max_chunk_chars, chunk_overlap_chars))
}

fn fallback(
    file: &FileInfo,
    text: &str,
    max_chunk_chars: usize,
    chunk_overlap_chars: usize,
) -> Vec<ExtractedFragment> {
    extract_plain_text_fragments(file, text, max_chunk_chars, chunk_overlap_chars)
        .into_iter()
        .map(|fragment| ExtractedFragment {
            fragment,
            embedding_source: None,
        })
        .collect()
}

/// One collected entity: a tree node plus its resolvedલadapter data.
struct CodeEntity<'a> {
    node: SyntaxNode<'a>,
    name: Option<String>,
    symbol_type: CodeSymbolType,
    breadcrumb: Vec<String>,
    signature: Option<String>,
    doc: Option<String>,
    modifiers: Vec<CodeEntityModifier>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GroupMark {
    /// First fragment of a split entity (TS `group: ""`).
    Major,
    /// Whole entity in one fragment (TS: no `group` field).
    Single,
    /// Continuation chunk of a split entity (inherits the major id).
    Minor,
}

struct RawFragment {
    mark: GroupMark,
    range: Range,
    text: String,
    embedding_text: Option<String>,
    metadata: CodeEntityMetadata,
}

fn walk_code_node<'a>(
    node: SyntaxNode<'a>,
    adapter: &dyn LanguageAdapter,
    breadcrumb: &[String],
    out: &mut Vec<CodeEntity<'a>>,
) {
    for child in node.children() {
        let kind = child.kind();
        let is_scope = adapter.is_scope_type(kind) && adapter.should_enter_scope(&child);
        let is_entity = adapter.is_entity_type(kind) && adapter.should_index_entity(&child);

        if is_entity {
            for entity_node in adapter.resolve_entities(&child) {
                let name = adapter.extract_name(&entity_node);
                let entity_breadcrumb = adapter.scope_breadcrumb(&entity_node, breadcrumb);
                let symbol_type = adapter
                    .classify_node(&entity_node, &entity_breadcrumb)
                    .unwrap_or_else(|| classify_code_node(&entity_node, &entity_breadcrumb));
                out.push(CodeEntity {
                    node: entity_node,
                    name,
                    symbol_type,
                    breadcrumb: entity_breadcrumb,
                    signature: adapter.extract_signature(&entity_node),
                    doc: adapter.extract_doc(&entity_node),
                    modifiers: adapter.extract_modifiers(&entity_node),
                });
            }
        }

        if is_scope {
            let name = adapter.extract_name(&child);
            let scope_node = adapter.enter_scope_node(&child);
            let mut next: Vec<String> = breadcrumb.to_vec();
            if let Some(name) = name {
                next.push(name);
            }
            walk_code_node(scope_node, adapter, &next, out);
            continue;
        }

        if !is_entity {
            walk_code_node(child, adapter, breadcrumb, out);
        }
    }
}

fn code_entity_metadata(entity: &CodeEntity<'_>) -> CodeEntityMetadata {
    CodeEntityMetadata {
        symbol_type: entity.symbol_type,
        symbol_name: entity.name.clone(),
        scope: if entity.breadcrumb.is_empty() {
            None
        } else {
            Some(entity.breadcrumb.join("::"))
        },
        node_type: Some(entity.node.kind().to_owned()),
        signature: entity.signature.clone(),
        doc: entity.doc.clone(),
        modifiers: entity.modifiers.clone(),
    }
}

fn code_entity_to_search_fragments(
    adapter: &dyn LanguageAdapter,
    entity: &CodeEntity<'_>,
    max_chars: usize,
    overlap_chars: usize,
) -> Vec<RawFragment> {
    let metadata = code_entity_metadata(entity);
    let resolved = chunk_options_for_metadata(
        max_chars,
        overlap_chars,
        Some(&EntityMetadata::Code(metadata.clone())),
    );
    let content_max_chars = resolved.max_chunk_chars;
    let node_text = entity.node.text().unwrap_or("");

    if node_text.chars().count() <= content_max_chars {
        let window = node_to_window(&entity.node);
        return vec![RawFragment {
            mark: GroupMark::Single,
            range: window.range,
            text: window.text,
            embedding_text: None,
            metadata,
        }];
    }

    let mut out = vec![RawFragment {
        mark: GroupMark::Major,
        range: node_to_window(&entity.node).range,
        text: code_entity_outline(entity, adapter, content_max_chars),
        embedding_text: None,
        metadata: metadata.clone(),
    }];
    for window in split_large_node(
        &entity.node,
        content_max_chars,
        resolved.chunk_overlap_chars,
    ) {
        out.push(RawFragment {
            mark: GroupMark::Minor,
            range: window.range,
            text: window.text,
            embedding_text: window.embedding_text,
            metadata: metadata.clone(),
        });
    }
    out
}

struct CodeWindow {
    text: String,
    embedding_text: Option<String>,
    range: Range,
}

fn node_to_window(node: &SyntaxNode<'_>) -> CodeWindow {
    CodeWindow {
        text: node.text().unwrap_or("").to_owned(),
        embedding_text: None,
        range: Range::Text {
            start_line: node.start_row() + 1,
            end_line: node.end_row() + 1,
            start_offset: node.start_byte(),
            end_offset: node.end_byte(),
        },
    }
}

fn split_large_node(
    node: &SyntaxNode<'_>,
    max_chars: usize,
    overlap_chars: usize,
) -> Vec<CodeWindow> {
    let body = node.field("body").unwrap_or(*node);
    let statements = body.named_children();
    if statements.len() <= 1 {
        let text = node.text().unwrap_or("");
        return split_text_by_lines(
            text,
            max_chars,
            node.start_row() + 1,
            node.start_byte(),
            overlap_chars,
        );
    }

    let base_start = node.start_byte();
    let source = node.text().unwrap_or("");
    let mut out = Vec::new();
    let mut group_start = 0usize;
    let mut group_chars = 0usize;

    for (index, statement) in statements.iter().enumerate() {
        let statement_text = statement.text().unwrap_or("");
        let statement_chars = statement_text.chars().count();
        if statement_chars > max_chars {
            if index > group_start {
                out.push(slice_statements(
                    source,
                    base_start,
                    &statements,
                    group_start,
                    index - 1,
                ));
            }
            out.extend(split_text_by_lines(
                statement_text,
                max_chars,
                statement.start_row() + 1,
                statement.start_byte(),
                overlap_chars,
            ));
            group_start = index + 1;
            group_chars = 0;
            continue;
        }
        let separator_chars = usize::from(index > group_start);
        if group_chars + separator_chars + statement_chars > max_chars && index > group_start {
            out.push(slice_statements(
                source,
                base_start,
                &statements,
                group_start,
                index - 1,
            ));
            let overlap_start =
                compute_overlap_start(&statements, group_start, index - 1, overlap_chars);
            let mut candidate_start = overlap_start.min(index);
            let mut candidate_chars = statement_chars;
            let mut previous = index;
            while previous > candidate_start {
                previous -= 1;
                let added = statements[previous].text().unwrap_or("").chars().count() + 1;
                if candidate_chars + added > max_chars {
                    candidate_start = previous + 1;
                    break;
                }
                candidate_chars += added;
            }
            group_start = candidate_start;
            group_chars = candidate_chars;
            continue;
        }
        group_chars += separator_chars + statement_chars;
    }

    if group_start < statements.len() {
        out.push(slice_statements(
            source,
            base_start,
            &statements,
            group_start,
            statements.len() - 1,
        ));
    }
    out
}

fn slice_statements(
    source: &str,
    base_start: usize,
    statements: &[SyntaxNode<'_>],
    start_index: usize,
    end_index: usize,
) -> CodeWindow {
    let start = statements[start_index].start_byte();
    let end = statements[end_index].end_byte();
    let text = slice_bytes(
        source,
        start.saturating_sub(base_start),
        end.saturating_sub(base_start),
    )
    .to_owned();
    let embedding_text = statements[start_index..=end_index]
        .iter()
        .map(|statement| statement.text().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n");
    CodeWindow {
        text,
        embedding_text: Some(embedding_text),
        range: Range::Text {
            start_line: statements[start_index].start_row() + 1,
            end_line: statements[end_index].end_row() + 1,
            start_offset: start,
            end_offset: end,
        },
    }
}

fn split_text_by_lines(
    text: &str,
    max_chars: usize,
    start_line: usize,
    start_offset: usize,
    overlap_chars: usize,
) -> Vec<CodeWindow> {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut out = Vec::new();
    let mut line_index = 0usize;
    let mut offset = start_offset;
    while line_index < lines.len() {
        if lines[line_index].chars().count() > max_chars {
            out.extend(split_long_line_by_chars(
                lines[line_index],
                max_chars,
                start_line + line_index,
                offset,
                overlap_chars,
            ));
            offset += lines[line_index].len() + 1;
            line_index += 1;
            continue;
        }
        let mut end_index = line_index;
        let mut used_chars = 0usize;
        while end_index < lines.len() {
            let line_length = lines[end_index].chars().count() + 1;
            if used_chars + line_length > max_chars && end_index > line_index {
                break;
            }
            used_chars += line_length;
            end_index += 1;
        }
        let chunk = lines[line_index..end_index].join("\n");
        out.push(CodeWindow {
            text: chunk.clone(),
            embedding_text: None,
            range: Range::Text {
                start_line: start_line + line_index,
                end_line: start_line + end_index - 1,
                start_offset: offset,
                end_offset: offset + chunk.len(),
            },
        });
        if end_index >= lines.len() {
            break;
        }
        let overlap_lines = compute_line_overlap(&lines, line_index, end_index, overlap_chars);
        let next_index = end_index - overlap_lines;
        offset += lines[line_index..next_index].join("\n").len();
        if next_index > line_index {
            offset += 1;
        }
        line_index = next_index;
    }
    out
}

fn split_long_line_by_chars(
    text: &str,
    max_chars: usize,
    line: usize,
    start_offset: usize,
    overlap_chars: usize,
) -> Vec<CodeWindow> {
    // Byte offset of every char boundary; `bounds[k]` starts the k-th char.
    let mut bounds = vec![0usize];
    for (index, ch) in text.char_indices() {
        bounds.push(index + ch.len_utf8());
    }
    let total = bounds.len() - 1;
    let mut out = Vec::new();
    let mut relative_start = 0usize;
    while relative_start < total {
        let raw_end = (relative_start + max_chars).min(total);
        let relative_end = raw_end;
        out.push(CodeWindow {
            text: text[bounds[relative_start]..bounds[relative_end]].to_owned(),
            embedding_text: None,
            range: Range::Text {
                start_line: line,
                end_line: line,
                start_offset: start_offset + bounds[relative_start],
                end_offset: start_offset + bounds[relative_end],
            },
        });
        if relative_end >= total {
            break;
        }
        relative_start = (relative_start + 1)
            .max(relative_end.saturating_sub(overlap_chars))
            .min(total);
    }
    out
}

fn compute_overlap_start(
    statements: &[SyntaxNode<'_>],
    group_start: usize,
    group_end: usize,
    overlap_chars: usize,
) -> usize {
    if overlap_chars == 0 {
        return group_end + 1;
    }
    let mut chars = 0usize;
    let mut index = group_end + 1;
    while index > group_start {
        index -= 1;
        chars += statements[index].text().unwrap_or("").chars().count();
        if index < group_end {
            chars += 1;
        }
        if chars >= overlap_chars {
            break;
        }
    }
    index
}

fn compute_line_overlap(
    lines: &[&str],
    start_index: usize,
    end_index: usize,
    overlap_chars: usize,
) -> usize {
    if overlap_chars == 0 {
        return 0;
    }
    let mut chars = 0usize;
    let mut count = 0usize;
    for index in (start_index..end_index).rev() {
        chars += lines[index].chars().count() + 1;
        if chars > overlap_chars {
            break;
        }
        count += 1;
    }
    count.min((end_index - start_index) / 2)
}

fn slice_bytes(text: &str, start: usize, end: usize) -> &str {
    let len = text.len();
    let mut s = start.min(len);
    let mut e = end.min(len).max(s);
    while s < len && !text.is_char_boundary(s) {
        s += 1;
    }
    while e > s && !text.is_char_boundary(e) {
        e -= 1;
    }
    text.get(s..e).unwrap_or("")
}

const STRUCTURAL_SYMBOL_TYPES: &[CodeSymbolType] = &[
    CodeSymbolType::Class,
    CodeSymbolType::Interface,
    CodeSymbolType::Module,
];

struct OutlineMember {
    symbol_type: CodeSymbolType,
    name: Option<String>,
    signature: Option<String>,
}

fn code_entity_outline(
    entity: &CodeEntity<'_>,
    adapter: &dyn LanguageAdapter,
    max_chars: usize,
) -> String {
    let node_text = entity.node.text().unwrap_or("");
    let header = extract_code_header(node_text);
    let mut lines = vec![if header.is_empty() {
        entity
            .name
            .clone()
            .unwrap_or_else(|| format!("{:?}", entity.symbol_type))
            .to_owned()
    } else {
        header
    }];
    if STRUCTURAL_SYMBOL_TYPES.contains(&entity.symbol_type) {
        let members = collect_structure_outline_members(entity, adapter);
        if !members.is_empty() {
            lines.push(String::new());
            lines.push("members:".to_owned());
            for member in &members {
                lines.push(format!("- {}", format_outline_member(member)));
            }
        }
    } else if entity.symbol_type == CodeSymbolType::Function {
        let calls = collect_function_call_names(&entity.node);
        if !calls.is_empty() {
            lines.push(String::new());
            lines.push(format!("calls: {}", calls.join(", ")));
        }
    }
    truncate_outline(lines.join("\n").trim(), max_chars)
}

fn truncate_outline(outline: &str, max_chars: usize) -> String {
    if outline.chars().count() <= max_chars {
        return outline.to_owned();
    }
    if max_chars <= 3 {
        return ".".repeat(max_chars);
    }
    let kept: String = outline.chars().take(max_chars - 3).collect();
    format!("{}...", kept.trim_end())
}

fn extract_code_header(text: &str) -> String {
    let mut lines = Vec::new();
    for line in text
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
    {
        lines.push(line);
        if line.contains('{') || lines.len() >= HEADER_MAX_LINES {
            break;
        }
    }
    let header = lines.join("\n");
    let header = header.trim();
    if header.chars().count() > HEADER_MAX_CHARS {
        let kept: String = header.chars().take(HEADER_MAX_CHARS).collect();
        format!("{}\n...", kept.trim_end())
    } else {
        header.to_owned()
    }
}

fn collect_structure_outline_members(
    entity: &CodeEntity<'_>,
    adapter: &dyn LanguageAdapter,
) -> Vec<OutlineMember> {
    let mut members: Vec<OutlineMember> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    visit_outline_member(&entity.node, entity, adapter, 0, &mut members, &mut seen);
    members
}

fn visit_outline_member(
    node: &SyntaxNode<'_>,
    entity: &CodeEntity<'_>,
    adapter: &dyn LanguageAdapter,
    depth: usize,
    members: &mut Vec<OutlineMember>,
    seen: &mut HashSet<String>,
) {
    if members.len() >= OUTLINE_MAX_MEMBERS || depth > 12 {
        return;
    }
    if !same_node(node, &entity.node)
        && adapter.is_entity_type(node.kind())
        && adapter.should_index_entity(node)
    {
        for resolved in adapter.resolve_entities(node) {
            if members.len() >= OUTLINE_MAX_MEMBERS || same_node(&resolved, &entity.node) {
                break;
            }
            let name = adapter.extract_name(&resolved);
            let symbol_type = adapter
                .classify_node(&resolved, &entity.breadcrumb)
                .unwrap_or_else(|| classify_code_node(&resolved, &entity.breadcrumb));
            let signature = adapter.extract_signature(&resolved);
            let key = format!(
                "{symbol_type:?}:{}:{}:{}",
                name.as_deref().unwrap_or(""),
                signature.as_deref().unwrap_or(""),
                resolved.start_byte()
            );
            if seen.insert(key) {
                members.push(OutlineMember {
                    symbol_type,
                    name,
                    signature,
                });
            }
        }
        return;
    }
    for child in node.named_children() {
        visit_outline_member(&child, entity, adapter, depth + 1, members, seen);
    }
}

fn format_outline_member(member: &OutlineMember) -> String {
    let name = member.name.as_deref().unwrap_or("");
    let signature = member
        .signature
        .as_deref()
        .map(|signature| truncate_outline_line(&one_line(signature)))
        .unwrap_or_default();
    if !signature.is_empty() {
        if !name.is_empty() && !signature.contains(name) {
            return format!("{:?} {name}: {signature}", member.symbol_type);
        }
        return format!("{:?} {signature}", member.symbol_type);
    }
    if !name.is_empty() {
        return format!("{:?} {name}", member.symbol_type);
    }
    format!("{:?}", member.symbol_type)
}

fn collect_function_call_names(node: &SyntaxNode<'_>) -> Vec<String> {
    let mut calls = Vec::new();
    let mut seen = HashSet::new();
    visit_call_names(node, &mut calls, &mut seen);
    calls
}

fn visit_call_names(node: &SyntaxNode<'_>, calls: &mut Vec<String>, seen: &mut HashSet<String>) {
    if calls.len() >= OUTLINE_MAX_CALLS {
        return;
    }
    if is_call_node(node.kind()) {
        if let Some(name) = extract_call_name(node) {
            if seen.insert(name.clone()) {
                calls.push(name);
            }
        }
    }
    for child in node.named_children() {
        visit_call_names(&child, calls, seen);
    }
}

fn is_call_node(kind: &str) -> bool {
    matches!(
        kind,
        "call"
            | "call_expression"
            | "function_call_expression"
            | "method_invocation"
            | "object_creation_expression"
            | "new_expression"
    )
}

fn extract_call_name(node: &SyntaxNode<'_>) -> Option<String> {
    let target = node
        .field("function")
        .or_else(|| node.field("name"))
        .or_else(|| node.field("constructor"))
        .or_else(|| node.field("type"))
        .or_else(|| node.named_children().into_iter().next())?;
    normalize_call_name(target.text().unwrap_or(""))
}

fn normalize_call_name(value: &str) -> Option<String> {
    let collapsed = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let cleaned = collapsed
        .strip_prefix("new ")
        .unwrap_or(&collapsed)
        .trim()
        .to_owned();
    // TS strips `^new\s+`; a second pass covers `new   Foo` collapse residue.
    let cleaned = cleaned
        .strip_prefix("new ")
        .unwrap_or(&cleaned)
        .trim()
        .to_owned();
    if cleaned.is_empty()
        || cleaned.chars().count() > OUTLINE_MAX_LINE_CHARS
        || cleaned.contains(['\n', '\r'])
        || !cleaned
            .chars()
            .any(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '$')
    {
        return None;
    }
    Some(cleaned)
}

fn same_node(left: &SyntaxNode<'_>, right: &SyntaxNode<'_>) -> bool {
    left.start_byte() == right.start_byte()
        && left.end_byte() == right.end_byte()
        && left.kind() == right.kind()
}

fn one_line(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate_outline_line(value: &str) -> String {
    if value.chars().count() > OUTLINE_MAX_LINE_CHARS {
        let kept: String = value.chars().take(OUTLINE_MAX_LINE_CHARS - 3).collect();
        format!("{}...", kept.trim_end())
    } else {
        value.to_owned()
    }
}

fn classify_code_node(node: &SyntaxNode<'_>, breadcrumb: &[String]) -> CodeSymbolType {
    let node_type = node.kind();
    if node_type == "decorated_definition" {
        let inner = node.named_children().into_iter().find(|child| {
            child.kind() == "function_definition" || child.kind() == "class_definition"
        });
        return inner
            .map(|child| classify_code_node(&child, breadcrumb))
            .unwrap_or(CodeSymbolType::Value);
    }
    if (node_type == "field_definition"
        || node_type == "public_field_definition"
        || node_type == "variable_declarator")
        && has_javascript_typescript_function_value(node)
    {
        return CodeSymbolType::Function;
    }
    if node_type.contains("method") || node_type.contains("constructor") {
        return CodeSymbolType::Function;
    }
    if !breadcrumb.is_empty()
        && (node_type.contains("function")
            || node_type == "declaration"
            || node_type == "function_item")
    {
        return CodeSymbolType::Function;
    }
    if node_type.contains("function") {
        return CodeSymbolType::Function;
    }
    if node_type == "declaration" || node_type == "macro_type_specifier" {
        return CodeSymbolType::Function;
    }
    if node_type.contains("class")
        || node_type.contains("struct")
        || node_type.contains("impl")
        || node_type.contains("enum")
        || node_type.contains("union")
        || node_type.contains("record")
    {
        return CodeSymbolType::Class;
    }
    if node_type.contains("interface")
        || node_type.contains("protocol")
        || node_type.contains("trait")
    {
        return CodeSymbolType::Interface;
    }
    if node_type.contains("module") || node_type.contains("namespace") || node_type == "mod_item" {
        return CodeSymbolType::Module;
    }
    if node_type.contains("alias")
        || node_type.contains("typedef")
        || node_type == "type_definition"
        || node_type == "type_item"
    {
        return CodeSymbolType::Alias;
    }
    CodeSymbolType::Value
}

struct ScriptBlock {
    text: String,
    format: String,
    start_line: usize,
    start_offset: usize,
}

fn extract_script_blocks(
    file: &FileInfo,
    text: &str,
    max_chunk_chars: usize,
    chunk_overlap_chars: usize,
) -> EngineResult<Vec<ExtractedFragment>> {
    let mut fragments = Vec::new();
    for block in find_script_blocks(text) {
        let block_file = FileInfo {
            format: crate::types::FileFormat(block.format.clone()),
            ..file.clone()
        };
        let options = ChunkOptions {
            max_chunk_chars: Some(max_chunk_chars),
            overlap_chars: Some(chunk_overlap_chars),
        };
        if let Some(block_fragments) = extract_for_indexing(&block_file, &block.text, &options)? {
            let start_index = fragments.len();
            fragments.extend(remap_script_block_fragments(
                &file.id,
                block_fragments,
                start_index,
                block.start_line,
                block.start_offset,
            ));
        }
    }
    Ok(fragments)
}

fn script_block_format(attrs: &str) -> String {
    if let Some(lang) = script_lang_attr(attrs) {
        if lang == "ts" || lang == "typescript" {
            return "typescript".to_owned();
        }
        if lang == "tsx" {
            return "tsx".to_owned();
        }
        if lang == "jsx" {
            return "jsx".to_owned();
        }
    }
    "javascript".to_owned()
}

fn script_lang_attr(attrs: &str) -> Option<String> {
    let bytes = attrs.as_bytes();
    let mut index = 0usize;
    while index + 4 <= bytes.len() {
        if match_lang_keyword(bytes, index) {
            let mut cursor = index + 4;
            while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
                cursor += 1;
            }
            if bytes.get(cursor) != Some(&b'=') {
                index += 1;
                continue;
            }
            cursor += 1;
            while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
                cursor += 1;
            }
            if cursor < bytes.len() && (bytes[cursor] == b'"' || bytes[cursor] == b'\'') {
                cursor += 1;
            }
            let start = cursor;
            while cursor < bytes.len()
                && (bytes[cursor].is_ascii_alphanumeric()
                    || bytes[cursor] == b'_'
                    || bytes[cursor] == b'-')
            {
                cursor += 1;
            }
            if cursor > start {
                return Some(attrs[start..cursor].to_lowercase());
            }
            return None;
        }
        index += 1;
    }
    None
}

/// ASCII case-insensitive `lang` keyword match with a word boundary on both
/// sides (mirrors the TS `\blang\s*=` pattern).
fn match_lang_keyword(bytes: &[u8], index: usize) -> bool {
    let word = b"lang";
    if bytes.len() < index + word.len() {
        return false;
    }
    if !bytes[index..index + word.len()].eq_ignore_ascii_case(word) {
        return false;
    }
    if index > 0 && is_attr_word_char(bytes[index - 1]) {
        return false;
    }
    !bytes
        .get(index + word.len())
        .is_some_and(|b| is_attr_word_char(*b))
}

fn is_attr_word_char(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-'
}

fn find_insensitive(haystack: &[u8], needle: &str, from: usize) -> Option<usize> {
    let needle = needle.as_bytes();
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    let mut start = from.min(haystack.len());
    while start + needle.len() <= haystack.len() {
        if haystack[start..start + needle.len()].eq_ignore_ascii_case(needle) {
            return Some(start);
        }
        start += 1;
    }
    None
}

fn find_script_blocks(text: &str) -> Vec<ScriptBlock> {
    let bytes = text.as_bytes();
    let mut blocks = Vec::new();
    let mut cursor = 0usize;
    while let Some(open) = find_insensitive(bytes, "<script", cursor) {
        let after = open + 7;
        let delimiter = bytes.get(after).copied().unwrap_or(b'>');
        if !(delimiter.is_ascii_whitespace() || delimiter == b'/' || delimiter == b'>') {
            cursor = after;
            continue;
        }
        let Some(tag_end) = bytes
            .iter()
            .skip(after)
            .position(|b| *b == b'>')
            .map(|pos| after + pos)
        else {
            break;
        };
        let attrs = &text[after..tag_end];
        let start_offset = tag_end + 1;
        let Some(close) = find_insensitive(bytes, "</script", start_offset) else {
            break;
        };
        let mut close_end = close + 8;
        while close_end < bytes.len() && bytes[close_end].is_ascii_whitespace() {
            close_end += 1;
        }
        if bytes.get(close_end) != Some(&b'>') {
            cursor = close + 2;
            continue;
        }
        close_end += 1;
        blocks.push(ScriptBlock {
            text: text[start_offset..close].to_owned(),
            format: script_block_format(attrs),
            start_line: bytes
                .iter()
                .take(start_offset)
                .filter(|b| **b == b'\n')
                .count()
                + 1,
            start_offset,
        });
        cursor = close_end;
    }
    blocks
}

fn remap_script_block_fragments(
    file_id: &FileId,
    fragments: Vec<ExtractedFragment>,
    start_index: usize,
    start_line: usize,
    start_offset: usize,
) -> Vec<ExtractedFragment> {
    let mut id_map: HashMap<String, EntityId> = HashMap::new();
    for (index, item) in fragments.iter().enumerate() {
        id_map.insert(
            item.fragment.entity.id.as_str().to_owned(),
            make_entity_id(file_id, start_index + index),
        );
    }
    fragments
        .into_iter()
        .map(|item| {
            let id = id_map
                .get(item.fragment.entity.id.as_str())
                .cloned()
                .unwrap_or_else(|| item.fragment.entity.id.clone());
            let group = item
                .fragment
                .group
                .as_deref()
                .filter(|group| !group.is_empty())
                .and_then(|group| id_map.get(group).cloned())
                .map(|group| group.as_str().to_owned());
            ExtractedFragment {
                fragment: EntityFragment {
                    entity: Entity {
                        id,
                        file_id: file_id.clone(),
                        range: remap_script_block_range(
                            &item.fragment.entity.range,
                            start_line,
                            start_offset,
                        ),
                        content: item.fragment.entity.content.clone(),
                        metadata: item.fragment.entity.metadata.clone(),
                    },
                    group,
                },
                embedding_source: item.embedding_source,
            }
        })
        .collect()
}

fn remap_script_block_range(range: &Range, start_line: usize, start_offset: usize) -> Range {
    match range {
        Range::Text {
            start_line: start,
            end_line: end,
            start_offset: start_off,
            end_offset: end_off,
        } => Range::Text {
            start_line: start_line + start - 1,
            end_line: start_line + end - 1,
            start_offset: start_offset + start_off,
            end_offset: start_offset + end_off,
        },
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{FileFormat, FileKind};

    fn rust_file(id: &str) -> FileInfo {
        FileInfo {
            id: FileId::from_raw(id.to_owned()),
            absolute_path: "/repo/main.rs".to_owned(),
            relative_path: "main.rs".to_owned(),
            root_path: "/repo".to_owned(),
            size_bytes: 0,
            last_modified_time: crate::types::UnixMillis(0),
            content_hash: None,
            kind: FileKind::Code,
            format: FileFormat("rust".to_owned()),
            index_status: None,
        }
    }

    #[test]
    fn extracts_named_function_entity() {
        let file = rust_file("f");
        let out = extract_for_indexing(&file, "fn alpha() {\n    1\n}\n", &ChunkOptions::default())
            .expect("extract")
            .expect("code");
        assert!(!out.is_empty());
        assert!(
            out.iter()
                .all(|item| item.fragment.entity.file_id == file.id)
        );
        let names: Vec<_> = out
            .iter()
            .filter_map(|item| item.fragment.entity.code_metadata())
            .filter_map(|meta| meta.symbol_name.clone())
            .collect();
        assert!(names.iter().any(|name| name == "alpha"));
    }

    #[test]
    fn rejects_bad_chunk_options() {
        let file = rust_file("f");
        let bad = ChunkOptions {
            max_chunk_chars: Some(0),
            overlap_chars: None,
        };
        assert!(extract_for_indexing(&file, "fn f() {}", &bad).is_err());
    }
}
