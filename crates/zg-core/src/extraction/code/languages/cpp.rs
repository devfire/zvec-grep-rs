//! C++ language adapter.
//!
//! Mirrors `CPP_ADAPTER` in `engine/extraction/code/languages/cpp.ts`:
//! the C-family hooks plus class/namespace entity and scope kinds.

use crate::extraction::code::adapter::{LanguageAdapter, SyntaxNode};
use crate::extraction::code::families::c_family::{
    c_family_scope_breadcrumb, classify_c_family_node, extract_c_family_name,
    should_index_c_family_entity,
};
use crate::extraction::code::families::metadata::{
    extract_common_modifiers, extract_generic_signature, extract_preceding_doc,
};
use crate::types::{CodeEntityModifier, CodeSymbolType};

/// Entity node kinds for C++.
pub const ENTITY_TYPES: &[&str] = &[
    "alias_declaration",
    "declaration",
    "field_declaration",
    "function_definition",
    "macro_type_specifier",
    "class_specifier",
    "struct_specifier",
    "union_specifier",
    "enum_specifier",
];

/// Scope node kinds for C++.
pub const SCOPE_TYPES: &[&str] = &[
    "namespace_definition",
    "class_specifier",
    "struct_specifier",
    "union_specifier",
];

/// Shared C++ adapter instance (mirrors `CPP_ADAPTER`).
pub static CPP_ADAPTER: CppLanguage = CppLanguage;

/// C++ language adapter.
pub struct CppLanguage;

impl crate::extraction::code::adapter::private::Sealed for CppLanguage {}

impl LanguageAdapter for CppLanguage {
    fn format(&self) -> &'static str {
        "cpp"
    }
    fn entity_types(&self) -> &'static [&'static str] {
        ENTITY_TYPES
    }
    fn scope_types(&self) -> &'static [&'static str] {
        SCOPE_TYPES
    }
    fn extract_name(&self, node: &SyntaxNode<'_>) -> Option<String> {
        extract_c_family_name(node)
    }
    fn should_index_entity(&self, node: &SyntaxNode<'_>) -> bool {
        should_index_c_family_entity(node)
    }
    fn scope_breadcrumb(&self, node: &SyntaxNode<'_>, breadcrumb: &[String]) -> Vec<String> {
        c_family_scope_breadcrumb(node, breadcrumb)
    }
    fn classify_node(
        &self,
        node: &SyntaxNode<'_>,
        breadcrumb: &[String],
    ) -> Option<CodeSymbolType> {
        let _ = breadcrumb;
        classify_c_family_node(node)
    }
    fn extract_signature(&self, node: &SyntaxNode<'_>) -> Option<String> {
        extract_generic_signature(node)
    }
    fn extract_doc(&self, node: &SyntaxNode<'_>) -> Option<String> {
        extract_preceding_doc(node)
    }
    fn extract_modifiers(&self, node: &SyntaxNode<'_>) -> Vec<CodeEntityModifier> {
        extract_common_modifiers(node)
    }
}
