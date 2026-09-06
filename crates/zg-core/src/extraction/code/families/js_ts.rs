//! JavaScript/TypeScript shared extraction.
//!
//! Mirrors `engine/extraction/code/families/js-ts.ts`: function-valued
//! fields, pairs, and variable declarators classify as functions only when
//! they hold a function value; `const x = { m() {} }` fans out into member
//! entities under the variable's breadcrumb.

use crate::extraction::code::adapter::SyntaxNode;
use crate::extraction::code::families::metadata::{
    closest_ancestor, extract_common_modifiers, extract_generic_signature, extract_preceding_doc,
};
use crate::extraction::code::families::name_field::find_named_identifier_child;
use crate::types::CodeSymbolType;

/// Declarations indexed only when they hold a function value.
pub const JS_TS_FUNCTION_VALUE_DECLARATION_TYPES: &[&str] = &[
    "field_definition",
    "public_field_definition",
    "variable_declarator",
];

/// Value node kinds that count as function values.
pub const JS_TS_FUNCTION_VALUE_TYPES: &[&str] = &["arrow_function", "function_expression"];

/// Whether a JS/TS entity node should be indexed.
///
/// Mirrors `shouldIndexJavascriptTypescriptEntity`: object-literal methods
/// are skipped (their `pair` parent carries them), `pair` nodes index only
/// for exported function-valued objects, and value declarations index only
/// with a function value.
pub fn should_index_javascript_typescript_entity(node: &SyntaxNode<'_>) -> bool {
    if node.kind() == "method_definition" && is_object_member(node) {
        return false;
    }
    if node.kind() == "pair" {
        return has_function_value(node) && exported_object_variable_name(node).is_some();
    }
    if !JS_TS_FUNCTION_VALUE_DECLARATION_TYPES.contains(&node.kind()) {
        return true;
    }
    if node.kind() == "variable_declarator" && !exported_object_function_entities(node).is_empty() {
        return true;
    }
    has_function_value(node)
}

/// True when `node` (or a wrapped call/arguments node) holds a function value.
///
/// Re-exported for the extractor, mirroring `hasJavascriptTypescriptFunctionValue`.
pub fn has_javascript_typescript_function_value(node: &SyntaxNode<'_>) -> bool {
    has_function_value(node)
}

/// Expands `const x = { … }` into its function-valued members.
///
/// Mirrors `resolveJavascriptTypescriptEntities`: only exported object
/// declarators fan out; everything else resolves to itself.
pub fn resolve_javascript_typescript_entities<'a>(node: &SyntaxNode<'a>) -> Vec<SyntaxNode<'a>> {
    if node.kind() != "variable_declarator" {
        return vec![*node];
    }
    let members = exported_object_function_entities(node);
    if members.is_empty() {
        vec![*node]
    } else {
        members
    }
}

/// Entity name: `pair` keys (unquoted), else the `name` field or identifier.
pub fn extract_javascript_typescript_name(node: &SyntaxNode<'_>) -> Option<String> {
    if node.kind() == "pair" {
        return node.field_text("key").map(|key| {
            let is_quote = |c: char| c == '\'' || c == '"' || c == '`';
            key.strip_prefix(is_quote)
                .and_then(|rest| rest.strip_suffix(is_quote).or(Some(rest)))
                .unwrap_or(key)
                .to_string()
        });
    }
    if let Some(name) = node.field_text("name") {
        return Some(name.to_string());
    }
    find_named_identifier_child(node)
        .and_then(|child| child.text())
        .map(str::to_string)
}

/// Appends the exported object name to the breadcrumb for member entities.
pub fn javascript_typescript_scope_breadcrumb(
    node: &SyntaxNode<'_>,
    breadcrumb: &[String],
) -> Vec<String> {
    match exported_object_variable_name(node) {
        Some(name) => {
            let mut out = breadcrumb.to_vec();
            out.push(name);
            out
        }
        None => breadcrumb.to_vec(),
    }
}

/// Signature hook: `pair` nodes render as `key: <value signature>`.
pub fn extract_javascript_typescript_signature(node: &SyntaxNode<'_>) -> Option<String> {
    if node.kind() == "pair" {
        let key = extract_javascript_typescript_name(node)?;
        let value = node.field("value")?;
        let value_signature = extract_generic_signature(&value)?;
        return Some(format!("{key}: {value_signature}"));
    }
    extract_generic_signature(node)
}

/// Classifies function-valued declarations/pairs as functions.
pub fn classify_javascript_typescript_node(node: &SyntaxNode<'_>) -> Option<CodeSymbolType> {
    if node.kind() == "pair" || JS_TS_FUNCTION_VALUE_DECLARATION_TYPES.contains(&node.kind()) {
        return if has_function_value(node) {
            Some(CodeSymbolType::Function)
        } else {
            None
        };
    }
    None
}

/// Doc hook shared by the JS/TS adapters.
pub fn extract_javascript_typescript_doc(node: &SyntaxNode<'_>) -> Option<String> {
    extract_preceding_doc(node)
}

/// Modifier hook shared by the JS/TS adapters.
pub fn extract_javascript_typescript_modifiers(
    node: &SyntaxNode<'_>,
) -> Vec<crate::types::CodeEntityModifier> {
    extract_common_modifiers(node)
}

fn has_function_value(node: &SyntaxNode<'_>) -> bool {
    let value = node.field("value").or_else(|| {
        node.named_children()
            .into_iter()
            .find(|child| JS_TS_FUNCTION_VALUE_TYPES.contains(&child.kind()))
    });
    match value {
        Some(value) => contains_function_value(&value),
        None => false,
    }
}

fn contains_function_value(node: &SyntaxNode<'_>) -> bool {
    if JS_TS_FUNCTION_VALUE_TYPES.contains(&node.kind()) {
        return true;
    }
    if node.kind() != "call_expression" && node.kind() != "arguments" {
        return false;
    }
    node.named_children().iter().any(contains_function_value)
}

/// Function-valued members of an exported `const x = { … }` declarator.
fn exported_object_function_entities<'a>(node: &SyntaxNode<'a>) -> Vec<SyntaxNode<'a>> {
    if !is_exported_variable_declarator(node) {
        return Vec::new();
    }
    let value = match node.field("value") {
        Some(value) if value.kind() == "object" || value.kind() == "object_expression" => value,
        _ => return Vec::new(),
    };
    value
        .named_children()
        .into_iter()
        .filter(|child| {
            (child.kind() == "pair" && has_function_value(child))
                || child.kind() == "method_definition"
        })
        .collect()
}

/// Variable name when `node` sits inside an exported object declarator.
fn exported_object_variable_name(node: &SyntaxNode<'_>) -> Option<String> {
    let object =
        closest_ancestor(node, "object").or_else(|| closest_ancestor(node, "object_expression"))?;
    let variable = object.parent()?;
    if variable.kind() != "variable_declarator" || !is_exported_variable_declarator(&variable) {
        return None;
    }
    extract_javascript_typescript_name(&variable)
}

fn is_exported_variable_declarator(node: &SyntaxNode<'_>) -> bool {
    if node.kind() != "variable_declarator" {
        return false;
    }
    closest_ancestor(node, "export_statement").is_some()
}

fn is_object_member(node: &SyntaxNode<'_>) -> bool {
    matches!(
        node.parent().map(|parent| parent.kind().to_string()),
        Some(kind) if kind == "object" || kind == "object_expression"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_contents() {
        assert!(JS_TS_FUNCTION_VALUE_DECLARATION_TYPES.contains(&"variable_declarator"));
        assert!(JS_TS_FUNCTION_VALUE_TYPES.contains(&"arrow_function"));
        // Surface check: the extractor's function-value probe is re-exported.
        let _ = has_javascript_typescript_function_value;
    }
}
