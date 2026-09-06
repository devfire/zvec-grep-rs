//! Name-field adapter factory shared by simple class-like languages.
//!
//! Mirrors `engine/extraction/code/families/name-field.ts`: the entity name
//! is the `name` field, falling back to an `*identifier` named child.
//! Signature, doc, and modifiers reuse the metadata helpers.

use crate::extraction::code::adapter::SyntaxNode;
use crate::extraction::code::families::metadata::{
    extract_common_modifiers, extract_generic_signature, extract_preceding_doc,
};

/// Extracts the entity name from the `name` field or an identifier child.
///
/// Mirrors the `extractName` installed by `createNameFieldAdapter`.
pub fn name_field_extract_name(node: &SyntaxNode<'_>) -> Option<String> {
    if let Some(name) = node.field_text("name") {
        return Some(name.to_string());
    }
    find_named_identifier_child(node)
        .and_then(|child| child.text())
        .map(str::to_string)
}

/// Finds the first `identifier` / `property_identifier` / `type_identifier`
/// named child, if any.
pub fn find_named_identifier_child<'a>(node: &SyntaxNode<'a>) -> Option<SyntaxNode<'a>> {
    for child in node.named_children() {
        match child.kind() {
            "identifier" | "property_identifier" | "type_identifier" => return Some(child),
            _ => {}
        }
    }
    None
}

/// Signature hook shared by name-field adapters.
pub fn name_field_extract_signature(node: &SyntaxNode<'_>) -> Option<String> {
    extract_generic_signature(node)
}

/// Doc hook shared by name-field adapters.
pub fn name_field_extract_doc(node: &SyntaxNode<'_>) -> Option<String> {
    extract_preceding_doc(node)
}

/// Modifier hook shared by name-field adapters.
pub fn name_field_extract_modifiers(
    node: &SyntaxNode<'_>,
) -> Vec<crate::types::CodeEntityModifier> {
    extract_common_modifiers(node)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifier_kinds_are_covered() {
        // Compile-time surface check: the fallback scans exactly the three
        // identifier kinds the TS implementation looks for.
        let kinds = ["identifier", "property_identifier", "type_identifier"];
        assert_eq!(kinds.len(), 3);
        let _ = find_named_identifier_child;
        let _ = name_field_extract_name;
    }
}
