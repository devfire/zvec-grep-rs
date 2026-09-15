//! VB.NET language adapter.
//!
//! Mirrors `languages/java.rs`: entity/scope tables plus the name-field
//! family hooks. Two hooks diverge from the java.rs mirror:
//! [`crate::extraction::code::adapter::LanguageAdapter::extract_modifiers`]
//! applies VB-only spellings (`Shared` → `Static`, `Friend` → `Internal`)
//! here so the shared `extract_common_modifiers` stays free of them —
//! `friend class Foo;` is genuine C++ and must never report `Internal`
//! outside VB; [`crate::extraction::code::adapter::LanguageAdapter::extract_doc`]
//! additionally probes the `type_declaration` wrapper parent (see below).
//!
//! Entity tables are keep-only-if-present subsets of the vb-dotnet
//! `NODE_TYPES`: every listed kind bears a `name` field except
//! `constructor_declaration`, named `New` by [`VbLanguage::extract_name`]
//! (`Sub New` has no name field; VB constructors are always `Sub New`).

use crate::extraction::code::adapter::{LanguageAdapter, SyntaxNode};
use crate::extraction::code::families::metadata::{
    extract_common_modifiers, extract_generic_signature, first_non_empty_line,
};
use crate::extraction::code::families::name_field::{
    name_field_extract_doc, name_field_extract_name, name_field_extract_signature,
};
use crate::types::{CodeEntityModifier, CodeSymbolType};

/// Entity node kinds for VB.NET (all bear a `name` field in `NODE_TYPES`
/// except `constructor_declaration`, named `New` by the step-6 override).
pub const ENTITY_TYPES: &[&str] = &[
    "class_block",
    "constructor_declaration",
    "delegate_declaration",
    "enum_block",
    "event_declaration",
    "interface_block",
    "method_declaration",
    "module_block",
    "namespace_block",
    "property_declaration",
    "structure_block",
];

/// Scope node kinds for VB.NET.
pub const SCOPE_TYPES: &[&str] = &[
    "class_block",
    "enum_block",
    "interface_block",
    "module_block",
    "namespace_block",
    "structure_block",
];

/// Shared VB.NET adapter instance.
pub static VB_ADAPTER: VbLanguage = VbLanguage;

/// VB.NET language adapter.
pub struct VbLanguage;

impl crate::extraction::code::adapter::private::Sealed for VbLanguage {}

impl LanguageAdapter for VbLanguage {
    fn format(&self) -> &'static str {
        "vb"
    }
    fn entity_types(&self) -> &'static [&'static str] {
        ENTITY_TYPES
    }
    fn scope_types(&self) -> &'static [&'static str] {
        SCOPE_TYPES
    }
    fn extract_name(&self, node: &SyntaxNode<'_>) -> Option<String> {
        if node.kind() == "constructor_declaration" {
            return Some("New".to_string());
        }
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
        if let Some(doc) = name_field_extract_doc(node) {
            return Some(doc);
        }
        // vb-dotnet wraps every type in a fieldless `type_declaration`
        // parent, so preceding comments are siblings of the wrapper, not of
        // the `*_block` entity itself. Probe the wrapper only: falling back
        // to any parent would misattribute the type's doc to undocumented
        // members nested inside it.
        node.parent()
            .filter(|parent| parent.kind() == "type_declaration")
            .and_then(|parent| name_field_extract_doc(&parent))
    }
    fn extract_modifiers(&self, node: &SyntaxNode<'_>) -> Vec<CodeEntityModifier> {
        let mut modifiers = extract_common_modifiers(node);
        // Haystack rebuilt exactly as `extract_common_modifiers` builds it;
        // the shared helper stays free of VB-only spellings (see module docs).
        let signature = extract_generic_signature(node).or_else(|| {
            node.text()
                .map(|text| first_non_empty_line(text).to_string())
        });
        let Some(haystack) = signature.as_deref() else {
            return modifiers;
        };
        for word in haystack.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')) {
            let extra = match word.to_ascii_lowercase().as_str() {
                "shared" => Some(CodeEntityModifier::Static),
                "friend" => Some(CodeEntityModifier::Internal),
                _ => None,
            };
            if let Some(modifier) = extra {
                // Local dedup: `push_unique` in `families::metadata` is
                // private to that module and unreachable here.
                if !modifiers.contains(&modifier) {
                    modifiers.push(modifier);
                }
            }
        }
        modifiers
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_vb(source: &str) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_vb_dotnet::LANGUAGE.into())
            .expect("vb-dotnet grammar loads");
        parser.parse(source, None).expect("vb source parses")
    }

    fn modifiers_of(source: &str, kind: &str) -> Vec<CodeEntityModifier> {
        let tree = parse_vb(source);
        assert!(
            !tree.root_node().has_error(),
            "fixture parses without error nodes: {source:?}"
        );
        let root = SyntaxNode::new(tree.root_node(), source.as_bytes());
        let node = root
            .find_descendant_by_kind(kind)
            .expect("fixture contains the target node");
        VB_ADAPTER.extract_modifiers(&node)
    }

    #[test]
    fn shared_sub_reports_public_and_static() {
        let source =
            "Public Class Greeter\n    Public Shared Sub SayHello()\n    End Sub\nEnd Class\n";
        assert_eq!(
            modifiers_of(source, "method_declaration"),
            vec![CodeEntityModifier::Public, CodeEntityModifier::Static]
        );
    }

    #[test]
    fn uppercase_modifiers_match() {
        let source =
            "PUBLIC Class Greeter\n    PUBLIC SHARED SUB Shout()\n    END SUB\nEND CLASS\n";
        assert_eq!(
            modifiers_of(source, "method_declaration"),
            vec![CodeEntityModifier::Public, CodeEntityModifier::Static]
        );
    }

    #[test]
    fn friend_class_reports_internal() {
        let source = "Friend Class Helper\nEnd Class\n";
        assert_eq!(
            modifiers_of(source, "class_block"),
            vec![CodeEntityModifier::Internal]
        );
    }

    #[test]
    fn shared_method_without_shared_reports_no_static() {
        let source = "Public Class Greeter\n    Public Sub SayHello()\n    End Sub\nEnd Class\n";
        assert_eq!(
            modifiers_of(source, "method_declaration"),
            vec![CodeEntityModifier::Public]
        );
    }

    #[test]
    fn constructor_extracts_new() {
        let source = "Public Class Greeter\n    Public Sub New()\n    End Sub\nEnd Class\n";
        let tree = parse_vb(source);
        assert!(
            !tree.root_node().has_error(),
            "fixture parses without error nodes: {source:?}"
        );
        let root = SyntaxNode::new(tree.root_node(), source.as_bytes());
        let ctor = root
            .find_descendant_by_kind("constructor_declaration")
            .expect("fixture contains the constructor node");
        assert_eq!(VB_ADAPTER.extract_name(&ctor), Some("New".to_string()));
        let class = root
            .find_descendant_by_kind("class_block")
            .expect("fixture contains the class node");
        assert_eq!(VB_ADAPTER.extract_name(&class), Some("Greeter".to_string()));
    }
}
