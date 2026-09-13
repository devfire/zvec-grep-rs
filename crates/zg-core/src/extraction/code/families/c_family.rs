//! C-family shared extraction (C and C++).
//!
//! Mirrors `engine/extraction/code/families/c-family.ts`: function
//! declarators nested inside declarations, `::`-qualified names, and
//! `typedef struct { … } Name` bodies that classify as classes.

use crate::extraction::code::adapter::{SyntaxNode, find_identifier_leaf};
use crate::types::CodeSymbolType;

/// Nodes whose entity name comes from the declarator's function name.
pub const C_FAMILY_FUNCTION_TYPES: &[&str] = &[
    "declaration",
    "field_declaration",
    "function_definition",
    "macro_type_specifier",
];

/// Declaration nodes indexed only when they declare a function.
pub const C_FAMILY_FUNCTION_DECLARATION_TYPES: &[&str] = &["declaration", "field_declaration"];

/// Whether a C-family entity node should be indexed.
///
/// Mirrors `shouldIndexEntity`: non-declaration nodes always index (function
/// macros only when a call-like name is found); declarations index only when
/// they contain a `function_declarator`.
pub fn should_index_c_family_entity(node: &SyntaxNode<'_>) -> bool {
    if !C_FAMILY_FUNCTION_DECLARATION_TYPES.contains(&node.kind()) {
        if node.kind() == "macro_type_specifier" {
            return node.text().and_then(extract_c_function_name).is_some();
        }
        return true;
    }
    node.find_descendant_by_kind("function_declarator")
        .is_some()
}

/// Entity name for a C-family node.
///
/// Function-like nodes resolve through the declarator (last `::` part kept);
/// other nodes use the `name` field's identifier leaf, with `type_definition`
/// falling back to the declarator's identifier.
pub fn extract_c_family_name(node: &SyntaxNode<'_>) -> Option<String> {
    if C_FAMILY_FUNCTION_TYPES.contains(&node.kind()) {
        let name = extract_raw_c_function_name(node)?;
        return Some(last_qualified_part(&name));
    }
    if let Some(name) = node.field("name") {
        let leaf = find_identifier_leaf(&name).unwrap_or(name);
        return leaf.text().map(str::to_string);
    }
    if node.kind() == "type_definition" {
        return node
            .field("declarator")
            .and_then(|declarator| find_identifier_leaf(&declarator))
            .and_then(|leaf| leaf.text())
            .map(str::to_string);
    }
    None
}

/// Symbol type for the nodes the generic walk cannot classify.
#[must_use]
pub fn classify_c_family_node(node: &SyntaxNode<'_>) -> Option<CodeSymbolType> {
    if node.kind() == "type_definition" {
        return Some(if typedef_wraps_class_like_body(node) {
            CodeSymbolType::Class
        } else {
            CodeSymbolType::Alias
        });
    }
    if node.kind() == "alias_declaration" {
        return Some(CodeSymbolType::Alias);
    }
    if node.kind() == "field_declaration"
        && node
            .find_descendant_by_kind("function_declarator")
            .is_some()
    {
        return Some(CodeSymbolType::Function);
    }
    None
}

/// Extends the breadcrumb with `A::B` qualifier parts of C++ definitions.
///
/// Mirrors `cFamilyScopeBreadcrumb`: when the raw name carries qualifiers,
/// appends them unless the first part duplicates the breadcrumb tail.
pub fn c_family_scope_breadcrumb(node: &SyntaxNode<'_>, breadcrumb: &[String]) -> Vec<String> {
    if !C_FAMILY_FUNCTION_TYPES.contains(&node.kind()) {
        return breadcrumb.to_vec();
    }
    let qualifier: Vec<String> = match extract_raw_c_function_name(node) {
        Some(name) => qualifier_parts(&name)
            .into_iter()
            .map(str::to_string)
            .collect(),
        None => Vec::new(),
    };
    if qualifier.is_empty() {
        return breadcrumb.to_vec();
    }
    let mut out = breadcrumb.to_vec();
    let mut parts = qualifier.as_slice();
    if let (Some(tail), Some((first, rest))) = (out.last(), parts.split_first())
        && tail == first
    {
        parts = rest;
    }
    out.extend(parts.iter().cloned());
    out
}

/// Raw (possibly `::`-qualified) function name from the declarator.
fn extract_raw_c_function_name(node: &SyntaxNode<'_>) -> Option<String> {
    let declarator = node
        .field("declarator")
        .or_else(|| node.find_descendant_by_kind("function_declarator"));
    let name = declarator
        .as_ref()
        .and_then(find_identifier_leaf)
        .and_then(|leaf| leaf.text())
        .map(str::to_string);
    match name {
        Some(name) if is_simple_c_identifier(&name) => Some(name),
        _ => {
            let text = declarator
                .as_ref()
                .and_then(|declarator| declarator.text())
                .or_else(|| node.text())?;
            extract_c_function_name(text)
        }
    }
}

/// Qualifier parts of `A::B::name` (everything but the last part).
fn qualifier_parts(name: &str) -> Vec<&str> {
    let parts: Vec<&str> = name.split("::").filter(|part| !part.is_empty()).collect();
    if parts.len() > 1 {
        parts.split_at(parts.len() - 1).0.to_vec()
    } else {
        Vec::new()
    }
}

/// Last `::` part of a qualified name.
fn last_qualified_part(name: &str) -> String {
    name.rsplit("::")
        .find(|part| !part.is_empty())
        .unwrap_or(name)
        .to_string()
}

/// True when a `type_definition` wraps a struct/union/enum body.
fn typedef_wraps_class_like_body(node: &SyntaxNode<'_>) -> bool {
    ["struct_specifier", "union_specifier", "enum_specifier"]
        .iter()
        .filter_map(|kind| node.find_descendant_by_kind(kind))
        .any(|child| child.field("body").is_some())
}

/// `~?Ident(::~?Ident)*` — mirrors the TS identifier gate.
fn is_simple_c_identifier(value: &str) -> bool {
    if value.is_empty() {
        return false;
    }
    value.split("::").all(|part| {
        let part = part.strip_prefix('~').unwrap_or(part);
        let mut chars = part.chars();
        match chars.next() {
            Some(first) if first.is_ascii_alphabetic() || first == '_' => {}
            _ => return false,
        }
        chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

/// Last `name(` call-like occurrence in `text`, if any.
///
/// Mirrors the TS `/([~A-Za-z_][~A-Za-z0-9_:]*)\s*\(/g` scan, taking the final
/// match (call-site parens sort after the declarator's own).
fn extract_c_function_name(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut last: Option<String> = None;
    let mut index = 0;
    while index < bytes.len() {
        let Some(&byte) = bytes.get(index) else {
            break;
        };
        if byte == b'~' || byte.is_ascii_alphabetic() || byte == b'_' {
            let start = index;
            index += 1;
            while bytes.get(index).is_some_and(|b| {
                *b == b'~' || b.is_ascii_alphanumeric() || *b == b'_' || *b == b':'
            }) {
                index += 1;
            }
            let mut end = index;
            while bytes.get(end).is_some_and(|b| b.is_ascii_whitespace()) {
                end += 1;
            }
            if bytes.get(end) == Some(&b'(') {
                last = text.get(start..index).map(str::to_string);
            }
        } else {
            index += 1;
        }
    }
    last
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qualified_name_helpers() {
        assert_eq!(last_qualified_part("A::B::foo"), "foo");
        assert_eq!(last_qualified_part("foo"), "foo");
        assert_eq!(qualifier_parts("A::B::foo"), vec!["A", "B"]);
        assert!(qualifier_parts("foo").is_empty());
    }

    #[test]
    fn identifier_gate() {
        assert!(is_simple_c_identifier("foo"));
        assert!(is_simple_c_identifier("~Foo"));
        assert!(is_simple_c_identifier("A::B::~C"));
        assert!(!is_simple_c_identifier(""));
        assert!(!is_simple_c_identifier("9lives"));
        assert!(!is_simple_c_identifier("foo bar"));
    }

    #[test]
    fn function_name_takes_last_match() {
        assert_eq!(
            extract_c_function_name("void f(int x) { g(1); }").as_deref(),
            Some("g")
        );
        assert_eq!(
            extract_c_function_name("int ns::foo(int a)").as_deref(),
            Some("ns::foo")
        );
        assert_eq!(extract_c_function_name("int x = 1;"), None);
    }
}
