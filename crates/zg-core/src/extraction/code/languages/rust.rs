//! Rust language adapter.
//!
//! Mirrors `RUST_ADAPTER` in `engine/extraction/code/languages/rust.ts`:
//! `impl` blocks name their self type; everything else uses the `name`
//! field. Signature, doc, and modifiers reuse the metadata helpers.

use crate::extraction::code::adapter::{LanguageAdapter, SyntaxNode};
use crate::extraction::code::families::metadata::{
    extract_common_modifiers, extract_generic_signature, extract_preceding_doc,
};
use crate::types::{CodeEntityModifier, CodeSymbolType};

/// Entity node kinds for Rust.
pub const ENTITY_TYPES: &[&str] = &[
    "enum_item",
    "function_item",
    "function_signature_item",
    "impl_item",
    "struct_item",
    "trait_item",
    "type_item",
    "union_item",
];

/// Scope node kinds for Rust.
pub const SCOPE_TYPES: &[&str] = &["impl_item", "mod_item", "trait_item"];

/// Shared Rust adapter instance (mirrors `RUST_ADAPTER`).
pub static RUST_ADAPTER: RustLanguage = RustLanguage;

/// Rust language adapter.
pub struct RustLanguage;

impl LanguageAdapter for RustLanguage {
    fn format(&self) -> &'static str {
        "rust"
    }
    fn entity_types(&self) -> &'static [&'static str] {
        ENTITY_TYPES
    }
    fn scope_types(&self) -> &'static [&'static str] {
        SCOPE_TYPES
    }
    fn extract_name(&self, node: &SyntaxNode<'_>) -> Option<String> {
        if node.kind() == "impl_item" {
            return node.field_text("type").map(str::to_string);
        }
        node.field_text("name").map(str::to_string)
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
        extract_generic_signature(node)
    }
    fn extract_doc(&self, node: &SyntaxNode<'_>) -> Option<String> {
        extract_preceding_doc(node)
    }
    fn extract_modifiers(&self, node: &SyntaxNode<'_>) -> Vec<CodeEntityModifier> {
        extract_common_modifiers(node)
    }
}
