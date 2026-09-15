//! C# language adapter.
//!
//! Mirrors `languages/java.rs`: entity/scope tables plus the name-field
//! family hooks. Entity tables are keep-only-if-present subsets of the
//! C# 0.23.5 `NODE_TYPES`: every listed kind bears a `name` field.

use crate::extraction::code::adapter::{LanguageAdapter, SyntaxNode};
use crate::extraction::code::families::name_field::{
    name_field_extract_doc, name_field_extract_modifiers, name_field_extract_name,
    name_field_extract_signature,
};
use crate::types::{CodeEntityModifier, CodeSymbolType};

/// Entity node kinds for C#.
///
/// Accepted asymmetry: VB keeps `event_declaration` (the vb-dotnet node
/// bears a `name` field) while C# drops both event forms
/// (`event_field_declaration` has no fields; the name is unrecoverable).
/// Accepted mapping: `delegate_declaration` classifies as Value via the
/// shared heuristic fallthrough (no closer `CodeSymbolType` fit).
pub const ENTITY_TYPES: &[&str] = &[
    "class_declaration",
    "struct_declaration",
    "interface_declaration",
    "enum_declaration",
    "record_declaration",
    "method_declaration",
    "constructor_declaration",
    "property_declaration",
    "delegate_declaration",
    "namespace_declaration",
    "file_scoped_namespace_declaration",
];

/// Scope node kinds for C#.
///
/// Block-namespace files yield members scoped as `A.B::Class` (the class's
/// own scope is `A.B`); file-scoped-namespace files (the .NET 6+ default
/// template) yield members scoped as bare `Class`.
/// `file_scoped_namespace_declaration` stays an entity but is excluded from
/// scopes (verified sibling, not parent).
pub const SCOPE_TYPES: &[&str] = &[
    "namespace_declaration",
    "class_declaration",
    "struct_declaration",
    "interface_declaration",
    "enum_declaration",
    "record_declaration",
];

/// Shared C# adapter instance (mirrors `CSHARP_ADAPTER`).
pub static CSHARP_ADAPTER: CSharpLanguage = CSharpLanguage;

/// C# language adapter.
pub struct CSharpLanguage;

impl crate::extraction::code::adapter::private::Sealed for CSharpLanguage {}

impl LanguageAdapter for CSharpLanguage {
    fn format(&self) -> &'static str {
        "csharp"
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
