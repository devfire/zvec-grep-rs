//! AST walk: entity collection, fragment assembly, node classification.

use crate::extraction::code::adapter::{LanguageAdapter, SyntaxNode};
use crate::extraction::code::extractor::chunking::{CodeWindow, split_large_node};
use crate::extraction::code::extractor::entry::{CodeEntity, GroupMark, RawFragment};
use crate::extraction::code::extractor::outline::code_entity_outline;
use crate::extraction::code::families::js_ts::has_javascript_typescript_function_value;
use crate::extraction::vector_content::chunk_options_for_metadata;
use crate::types::{CodeEntityMetadata, CodeSymbolType, EntityMetadata, Range};

pub(crate) fn walk_code_node<'a>(
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

pub(crate) fn code_entity_to_search_fragments(
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

pub(crate) fn node_to_window(node: &SyntaxNode<'_>) -> CodeWindow {
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

pub(crate) fn classify_code_node(node: &SyntaxNode<'_>, breadcrumb: &[String]) -> CodeSymbolType {
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
