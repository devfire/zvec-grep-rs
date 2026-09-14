//! TypeScript language adapter.
//!
//! Mirrors `TYPESCRIPT_ADAPTER` in
//! `engine/extraction/code/languages/typescript.ts`: the js-ts family hooks
//! with the wider TSX/abstract/interface/type-alias entity and scope tables.

use crate::extraction::code::adapter::{LanguageAdapter, SyntaxNode};
use crate::extraction::code::families::js_ts::{
    classify_javascript_typescript_node, extract_javascript_typescript_doc,
    extract_javascript_typescript_modifiers, extract_javascript_typescript_name,
    extract_javascript_typescript_signature, javascript_typescript_scope_breadcrumb,
    resolve_javascript_typescript_entities, should_index_javascript_typescript_entity,
};
use crate::types::{CodeEntityModifier, CodeSymbolType};

/// Entity node kinds for TypeScript.
pub const ENTITY_TYPES: &[&str] = &[
    "abstract_class_declaration",
    "abstract_method_signature",
    "class_declaration",
    "enum_declaration",
    "field_definition",
    "function_declaration",
    "generator_function_declaration",
    "interface_declaration",
    "method_signature",
    "method_definition",
    "pair",
    "public_field_definition",
    "type_alias_declaration",
    "variable_declarator",
];

/// Scope node kinds for TypeScript.
pub const SCOPE_TYPES: &[&str] = &[
    "abstract_class_declaration",
    "class_declaration",
    "internal_module",
    "interface_declaration",
    "module_declaration",
    "namespace_declaration",
];

/// Shared TypeScript adapter instance (mirrors `TYPESCRIPT_ADAPTER`;
/// also serves `tsx` via [`resolve_adapter`](crate::extraction::code::adapter::resolve_adapter)).
pub static TYPESCRIPT_ADAPTER: TypescriptLanguage = TypescriptLanguage;

/// TypeScript language adapter.
pub struct TypescriptLanguage;

impl crate::extraction::code::adapter::private::Sealed for TypescriptLanguage {}

impl LanguageAdapter for TypescriptLanguage {
    fn format(&self) -> &'static str {
        "typescript"
    }
    fn entity_types(&self) -> &'static [&'static str] {
        ENTITY_TYPES
    }
    fn scope_types(&self) -> &'static [&'static str] {
        SCOPE_TYPES
    }
    fn extract_name(&self, node: &SyntaxNode<'_>) -> Option<String> {
        extract_javascript_typescript_name(node)
    }
    fn should_index_entity(&self, node: &SyntaxNode<'_>) -> bool {
        should_index_javascript_typescript_entity(node)
    }
    fn resolve_entities<'a>(&self, node: &SyntaxNode<'a>) -> Vec<SyntaxNode<'a>> {
        resolve_javascript_typescript_entities(node)
    }
    fn scope_breadcrumb(&self, node: &SyntaxNode<'_>, breadcrumb: &[String]) -> Vec<String> {
        javascript_typescript_scope_breadcrumb(node, breadcrumb)
    }
    fn classify_node(
        &self,
        node: &SyntaxNode<'_>,
        breadcrumb: &[String],
    ) -> Option<CodeSymbolType> {
        let _ = breadcrumb;
        classify_javascript_typescript_node(node)
    }
    fn extract_signature(&self, node: &SyntaxNode<'_>) -> Option<String> {
        extract_javascript_typescript_signature(node)
    }
    fn extract_doc(&self, node: &SyntaxNode<'_>) -> Option<String> {
        extract_javascript_typescript_doc(node)
    }
    fn extract_modifiers(&self, node: &SyntaxNode<'_>) -> Vec<CodeEntityModifier> {
        extract_javascript_typescript_modifiers(node)
    }
}
