//! Python language adapter.
//!
//! Mirrors `PYTHON_ADAPTER` in `engine/extraction/code/languages/python.ts`:
//! `decorated_definition` wrappers resolve to the inner definition for
//! naming and signatures, open scopes only around classes, and add `async`
//! / `static` modifiers from the raw text.

use crate::extraction::code::adapter::{LanguageAdapter, SyntaxNode};
use crate::extraction::code::families::metadata::{
    extract_common_modifiers, extract_generic_signature, extract_preceding_doc,
};
use crate::types::{CodeEntityModifier, CodeSymbolType};

/// Entity node kinds for Python.
pub const ENTITY_TYPES: &[&str] = &[
    "class_definition",
    "decorated_definition",
    "function_definition",
];

/// Scope node kinds for Python.
pub const SCOPE_TYPES: &[&str] = &["class_definition", "decorated_definition"];

/// Shared Python adapter instance (mirrors `PYTHON_ADAPTER`).
pub static PYTHON_ADAPTER: PythonLanguage = PythonLanguage;

/// Python language adapter.
pub struct PythonLanguage;

impl crate::extraction::code::adapter::private::Sealed for PythonLanguage {}

impl LanguageAdapter for PythonLanguage {
    fn format(&self) -> &'static str {
        "python"
    }
    fn entity_types(&self) -> &'static [&'static str] {
        ENTITY_TYPES
    }
    fn scope_types(&self) -> &'static [&'static str] {
        SCOPE_TYPES
    }
    fn extract_name(&self, node: &SyntaxNode<'_>) -> Option<String> {
        if node.kind() == "decorated_definition" {
            let inner = inner_python_definition(node)?;
            return inner.field_text("name").map(str::to_string);
        }
        node.field_text("name").map(str::to_string)
    }
    fn should_enter_scope(&self, node: &SyntaxNode<'_>) -> bool {
        if node.kind() != "decorated_definition" {
            return true;
        }
        node.named_children()
            .iter()
            .any(|child| child.kind() == "class_definition")
    }
    fn enter_scope_node<'a>(&self, node: &SyntaxNode<'a>) -> SyntaxNode<'a> {
        if node.kind() != "decorated_definition" {
            return *node;
        }
        inner_python_definition(node)
            .filter(|inner| inner.kind() == "class_definition")
            .unwrap_or(*node)
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
        extract_generic_signature(&inner_python_definition(node).unwrap_or(*node))
    }
    fn extract_doc(&self, node: &SyntaxNode<'_>) -> Option<String> {
        extract_preceding_doc(node)
    }
    fn extract_modifiers(&self, node: &SyntaxNode<'_>) -> Vec<CodeEntityModifier> {
        let mut modifiers = extract_common_modifiers(node);
        let text = match node.text() {
            Some(text) => text,
            None => return modifiers,
        };
        if has_leading_async_def(text) && !modifiers.contains(&CodeEntityModifier::Async) {
            modifiers.push(CodeEntityModifier::Async);
        }
        if has_staticmethod_decorator(text) && !modifiers.contains(&CodeEntityModifier::Static) {
            modifiers.push(CodeEntityModifier::Static);
        }
        modifiers
    }
}

/// Inner definition of a `decorated_definition`, if any.
fn inner_python_definition<'a>(node: &SyntaxNode<'a>) -> Option<SyntaxNode<'a>> {
    if node.kind() != "decorated_definition" {
        return None;
    }
    node.named_children()
        .into_iter()
        .find(|child| child.kind() == "function_definition" || child.kind() == "class_definition")
}

/// True for a line like `async def f(` with only whitespace before `async`.
///
/// Mirrors the TS `/^\s*async\s+def\b/m` test.
fn has_leading_async_def(text: &str) -> bool {
    text.split('\n').any(|line| {
        let rest = line.trim_start_matches(|c: char| c.is_whitespace() && c != '\n');
        let after_async = match rest.strip_prefix("async") {
            Some(rest) => rest,
            None => return false,
        };
        if !after_async.starts_with(char::is_whitespace) {
            return false;
        }
        let after_space = after_async.trim_start_matches(char::is_whitespace);
        match after_space.strip_prefix("def") {
            Some(rest) => {
                rest.is_empty()
                    || !rest.starts_with(|c: char| c == '_' || c.is_ascii_alphanumeric())
            }
            None => false,
        }
    })
}

/// True for a `@staticmethod` decorator line.
///
/// Mirrors the TS `/^\s*@staticmethod\b/m` test.
fn has_staticmethod_decorator(text: &str) -> bool {
    text.split('\n').any(|line| {
        let rest = line.trim_start_matches(|c: char| c.is_whitespace() && c != '\n');
        match rest.strip_prefix("@staticmethod") {
            Some(rest) => {
                rest.is_empty()
                    || !rest.starts_with(|c: char| c == '_' || c.is_ascii_alphanumeric())
            }
            None => false,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decorator_line_probes() {
        assert!(has_leading_async_def("  async  def f():\n    pass"));
        assert!(!has_leading_async_def("x = 1\nasyncio.run()"));
        assert!(!has_leading_async_def("def f():"));
        assert!(has_staticmethod_decorator(
            "    @staticmethod\n    def f():"
        ));
        assert!(!has_staticmethod_decorator("@staticmethods\ndef f():"));
    }
}
