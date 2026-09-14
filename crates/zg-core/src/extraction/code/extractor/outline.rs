//! Outlines: structural headers, member lists, call names.

use std::collections::HashSet;

use crate::extraction::code::adapter::{LanguageAdapter, SyntaxNode};
use crate::extraction::code::extractor::entry::CodeEntity;
use crate::extraction::code::extractor::walk::classify_code_node;
use crate::types::CodeSymbolType;

const OUTLINE_MAX_MEMBERS: usize = 32;
const OUTLINE_MAX_CALLS: usize = 24;
const OUTLINE_MAX_LINE_CHARS: usize = 180;
const HEADER_MAX_CHARS: usize = 1200;
const HEADER_MAX_LINES: usize = 24;

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

pub(crate) fn code_entity_outline(
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
    if is_call_node(node.kind())
        && let Some(name) = extract_call_name(node)
        && seen.insert(name.clone())
    {
        calls.push(name);
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
