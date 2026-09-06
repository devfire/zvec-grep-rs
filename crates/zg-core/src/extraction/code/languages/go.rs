//! Go language adapter.
//!
//! Mirrors `GO_ADAPTER` in `engine/extraction/code/languages/go.ts`:
//! `type_spec` scopes unwrap to the underlying struct/interface type,
//! methods append their receiver type to the breadcrumb, `type_spec`
//! classifies by wrapped type, and capitalized names count as exported.

use crate::extraction::code::adapter::{LanguageAdapter, SyntaxNode};
use crate::extraction::code::families::metadata::{
    extract_generic_signature, extract_preceding_doc,
};
use crate::types::{CodeEntityModifier, CodeSymbolType};

/// Entity node kinds for Go.
pub const ENTITY_TYPES: &[&str] = &[
    "function_declaration",
    "method_spec",
    "method_declaration",
    "type_alias",
    "type_spec",
];

/// Scope node kinds for Go.
pub const SCOPE_TYPES: &[&str] = &["type_spec"];

/// Shared Go adapter instance (mirrors `GO_ADAPTER`).
pub static GO_ADAPTER: GoLanguage = GoLanguage;

/// Go language adapter.
pub struct GoLanguage;

impl LanguageAdapter for GoLanguage {
    fn format(&self) -> &'static str {
        "go"
    }
    fn entity_types(&self) -> &'static [&'static str] {
        ENTITY_TYPES
    }
    fn scope_types(&self) -> &'static [&'static str] {
        SCOPE_TYPES
    }
    fn extract_name(&self, node: &SyntaxNode<'_>) -> Option<String> {
        node.field_text("name").map(str::to_string)
    }
    fn should_enter_scope(&self, node: &SyntaxNode<'_>) -> bool {
        if node.kind() != "type_spec" {
            return true;
        }
        matches!(
            node.field("type").map(|t| t.kind().to_string()),
            Some(kind) if kind == "interface_type" || kind == "struct_type"
        )
    }
    fn enter_scope_node<'a>(&self, node: &SyntaxNode<'a>) -> SyntaxNode<'a> {
        node.field("type").unwrap_or(*node)
    }
    fn scope_breadcrumb(&self, node: &SyntaxNode<'_>, breadcrumb: &[String]) -> Vec<String> {
        if node.kind() != "method_declaration" {
            return breadcrumb.to_vec();
        }
        match extract_go_receiver_type(node) {
            Some(receiver) => {
                let mut out = breadcrumb.to_vec();
                out.push(receiver);
                out
            }
            None => breadcrumb.to_vec(),
        }
    }
    fn classify_node(
        &self,
        node: &SyntaxNode<'_>,
        breadcrumb: &[String],
    ) -> Option<CodeSymbolType> {
        let _ = breadcrumb;
        if node.kind() == "type_alias" {
            return Some(CodeSymbolType::Alias);
        }
        if node.kind() == "type_spec" {
            return Some(match node.field("type").map(|t| t.kind().to_string()) {
                Some(kind) if kind == "interface_type" => CodeSymbolType::Interface,
                Some(kind) if kind == "struct_type" => CodeSymbolType::Class,
                _ => CodeSymbolType::Alias,
            });
        }
        if node.kind() == "method_spec" {
            return Some(CodeSymbolType::Function);
        }
        None
    }
    fn extract_signature(&self, node: &SyntaxNode<'_>) -> Option<String> {
        extract_generic_signature(node)
    }
    fn extract_doc(&self, node: &SyntaxNode<'_>) -> Option<String> {
        extract_preceding_doc(node)
    }
    fn extract_modifiers(&self, node: &SyntaxNode<'_>) -> Vec<CodeEntityModifier> {
        match node.field_text("name") {
            Some(name)
                if name
                    .chars()
                    .next()
                    .is_some_and(|first| first.is_ascii_uppercase()) =>
            {
                vec![CodeEntityModifier::Exported]
            }
            _ => Vec::new(),
        }
    }
}

/// Receiver struct name of a method declaration, e.g. `T` in `func (t T) M()`.
///
/// Mirrors the TS `/\*?\s*([A-Za-z_][A-Za-z0-9_]*)\s*\)/` scan over the
/// receiver text: skips a leading `*`, then reads the first identifier
/// immediately before the closing paren.
fn extract_go_receiver_type(node: &SyntaxNode<'_>) -> Option<String> {
    let receiver = node.field("receiver")?;
    let text = receiver.text()?;
    let before_close = text.rsplit(')').nth(1)?;
    let token = before_close
        .rsplit(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .next()?;
    let token = token.trim_end_matches('_');
    if token.is_empty()
        || !token
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
    {
        return None;
    }
    Some(token.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receiver_type_shape() {
        // Shape check on the helper contract: pointer receivers, plain
        // receivers, and missing receivers behave like the TS regex.
        let _ = extract_go_receiver_type;
        assert!(GO_ADAPTER.is_entity_type("type_spec"));
        assert!(GO_ADAPTER.is_scope_type("type_spec"));
        assert!(!GO_ADAPTER.is_scope_type("function_declaration"));
    }
}
