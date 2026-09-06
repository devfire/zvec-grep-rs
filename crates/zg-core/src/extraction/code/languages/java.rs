//! Java language adapter.
//!
//! Mirrors `JAVA_ADAPTER` in `engine/extraction/code/languages/java.ts`,
//! built on the name-field family hooks.

use crate::extraction::code::adapter::{LanguageAdapter, SyntaxNode};
use crate::extraction::code::families::name_field::{
    name_field_extract_doc, name_field_extract_modifiers, name_field_extract_name,
    name_field_extract_signature,
};
use crate::types::{CodeEntityModifier, CodeSymbolType};

/// Entity node kinds for Java.
pub const ENTITY_TYPES: &[&str] = &[
    "annotation_type_declaration",
    "class_declaration",
    "constructor_declaration",
    "enum_declaration",
    "interface_declaration",
    "method_declaration",
    "record_declaration",
];

/// Scope node kinds for Java.
pub const SCOPE_TYPES: &[&str] = &[
    "annotation_type_declaration",
    "class_declaration",
    "enum_declaration",
    "interface_declaration",
    "record_declaration",
];

/// Shared Java adapter instance (mirrors `JAVA_ADAPTER`).
pub static JAVA_ADAPTER: JavaLanguage = JavaLanguage;

/// Java language adapter.
pub struct JavaLanguage;

impl LanguageAdapter for JavaLanguage {
    fn format(&self) -> &'static str {
        "java"
    }
    fn entity_types(&self) -> &'static [&'static str] {
        ENTITY_TYPES
    }
    fn scope_types(&self) -> &'static [&'static str] {
        SCOPE_TYPES
    }
    fn extract_name(&self, node: &SyntaxNode<'_>) -> Option<String> {
        name_field_extract_name(node)
    }
    fn classify_node(
        &self,
        node: &SyntaxNode<'_>,
        breadcrumb: &[String],
    ) -> Option<CodeSymbolType> {
        let _ = node;
        let _ = breadcrumb;
        None
    }
    fn extract_signature(&self, node: &SyntaxNode<'_>) -> Option<String> {
        name_field_extract_signature(node)
    }
    fn extract_doc(&self, node: &SyntaxNode<'_>) -> Option<String> {
        name_field_extract_doc(node)
    }
    fn extract_modifiers(&self, node: &SyntaxNode<'_>) -> Vec<CodeEntityModifier> {
        name_field_extract_modifiers(node)
    }
}
