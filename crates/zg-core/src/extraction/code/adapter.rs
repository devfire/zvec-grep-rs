//! Language-adapter surface for tree-sitter code extraction.
//!
//! Mirrors `engine/extraction/code/adapter.ts`: every language exposes entity
//! and scope node-kind tables plus hooks for name, signature, doc, modifiers,
//! and scope breadcrumbs. The future extractor (`code/extractor.ts` port)
//! walks the tree with [`SyntaxNode`] and consults the adapter returned by
//! [`resolve_adapter`].
//!
//! [`SyntaxNode`] is a thin zero-cost view over `tree_sitter::Node` carrying
//! the source bytes, so `text()` needs no extra lookup. It is `Copy`; hooks
//! that "return a node" just copy the view.

use crate::types::{CodeEntityModifier, CodeSymbolType};

/// A tree-sitter node bound to the source it was parsed from.
///
/// Carries the raw source bytes alongside the node so text access is a plain
/// slice lookup. All views derived from one parse share the same lifetime.
#[derive(Clone, Copy)]
pub struct SyntaxNode<'a> {
    inner: tree_sitter::Node<'a>,
    source: &'a [u8],
}

impl<'a> SyntaxNode<'a> {
    /// Binds a raw tree-sitter node to its source bytes.
    #[must_use]
    pub fn new(inner: tree_sitter::Node<'a>, source: &'a [u8]) -> Self {
        Self { inner, source }
    }

    /// Raw node kind, e.g. `"function_definition"`.
    #[must_use]
    pub fn kind(&self) -> &str {
        self.inner.kind()
    }

    /// Source slice covered by this node, or `None` on invalid UTF-8.
    #[must_use]
    pub fn text(&self) -> Option<&'a str> {
        self.inner.utf8_text(self.source).ok()
    }

    /// Byte offset where this node starts.
    #[must_use]
    pub fn start_byte(&self) -> usize {
        self.inner.start_byte()
    }
    /// Byte offset where this node ends.
    #[must_use]
    pub fn end_byte(&self) -> usize {
        self.inner.end_byte()
    }

    /// Zero-based start row.
    #[must_use]
    pub fn start_row(&self) -> usize {
        self.inner.start_position().row
    }

    /// Zero-based end row.
    #[must_use]
    pub fn end_row(&self) -> usize {
        self.inner.end_position().row
    }

    /// Number of children, including anonymous tokens.
    #[must_use]
    pub fn child_count(&self) -> usize {
        self.inner.child_count()
    }

    /// The `index`-th child (named or anonymous), if any.
    #[must_use]
    pub fn child(&self, index: usize) -> Option<SyntaxNode<'a>> {
        self.inner.child(index).map(|inner| SyntaxNode {
            inner,
            source: self.source,
        })
    }

    /// All children in source order, including anonymous tokens.
    #[must_use]
    pub fn children(&self) -> Vec<SyntaxNode<'a>> {
        let mut out = Vec::with_capacity(self.child_count());
        for index in 0..self.child_count() {
            if let Some(child) = self.child(index) {
                out.push(child);
            }
        }
        out
    }

    /// True for named (non-anonymous-token) nodes.
    #[must_use]
    pub fn is_named(&self) -> bool {
        self.inner.is_named()
    }

    /// Child bound to `field`, if present.
    #[must_use]
    pub fn field(&self, field: &str) -> Option<SyntaxNode<'a>> {
        self.inner
            .child_by_field_name(field)
            .map(|inner| SyntaxNode {
                inner,
                source: self.source,
            })
    }

    /// Text of the child bound to `field`, if present and valid UTF-8.
    #[must_use]
    pub fn field_text(&self, field: &str) -> Option<&'a str> {
        self.field(field)?.text()
    }

    /// Number of named children.
    #[must_use]
    pub fn named_child_count(&self) -> usize {
        self.inner.named_child_count()
    }

    /// The `index`-th named child, if any.
    #[must_use]
    pub fn named_child(&self, index: usize) -> Option<SyntaxNode<'a>> {
        self.inner.named_child(index).map(|inner| SyntaxNode {
            inner,
            source: self.source,
        })
    }

    /// All named children, in source order.
    #[must_use]
    pub fn named_children(&self) -> Vec<SyntaxNode<'a>> {
        let mut out = Vec::with_capacity(self.named_child_count());
        for index in 0..self.named_child_count() {
            if let Some(child) = self.named_child(index) {
                out.push(child);
            }
        }
        out
    }

    /// First named child of exactly `kind`, if any.
    #[must_use]
    pub fn named_child_of_kind(&self, kind: &str) -> Option<SyntaxNode<'a>> {
        for index in 0..self.named_child_count() {
            if let Some(child) = self.named_child(index)
                && child.kind() == kind
            {
                return Some(child);
            }
        }
        None
    }

    /// Parent node, if any.
    #[must_use]
    pub fn parent(&self) -> Option<SyntaxNode<'a>> {
        self.inner.parent().map(|inner| SyntaxNode {
            inner,
            source: self.source,
        })
    }

    /// Previous named sibling, if any.
    #[must_use]
    pub fn prev_named_sibling(&self) -> Option<SyntaxNode<'a>> {
        self.inner.prev_named_sibling().map(|inner| SyntaxNode {
            inner,
            source: self.source,
        })
    }

    /// Depth-first search for the first descendant (or self) of `kind`.
    #[must_use]
    pub fn find_descendant_by_kind(&self, kind: &str) -> Option<SyntaxNode<'a>> {
        if self.kind() == kind {
            return Some(*self);
        }
        for index in 0..self.named_child_count() {
            if let Some(child) = self.named_child(index)
                && let Some(found) = child.find_descendant_by_kind(kind)
            {
                return Some(found);
            }
        }
        None
    }
}

impl std::fmt::Debug for SyntaxNode<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SyntaxNode")
            .field("kind", &self.kind())
            .field("start_byte", &self.start_byte())
            .finish()
    }
}

/// Per-language extraction behavior.
///
/// Mirrors the `LanguageAdapter` type in `adapter.ts`. Optional TS hooks
/// become trait methods with defaults so languages only override what they
/// need. All returned strings are owned: several hooks transform the raw
/// slice (qualifier stripping, quote trimming, signature normalization).
pub trait LanguageAdapter: Send + Sync {
    /// Structured format key, e.g. `"rust"`.
    fn format(&self) -> &'static str;

    /// Node kinds that yield indexable entities.
    fn entity_types(&self) -> &'static [&'static str];

    /// Node kinds that open a named scope for breadcrumbs.
    fn scope_types(&self) -> &'static [&'static str];

    /// True when `kind` is an entity node for this language.
    fn is_entity_type(&self, kind: &str) -> bool {
        self.entity_types().contains(&kind)
    }

    /// True when `kind` opens a scope for this language.
    fn is_scope_type(&self, kind: &str) -> bool {
        self.scope_types().contains(&kind)
    }

    /// Entity name for `node`, if one can be determined.
    fn extract_name(&self, node: &SyntaxNode<'_>) -> Option<String>;

    /// Whether an entity-kind node should actually be indexed.
    /// (TS: `shouldIndexEntity`; defaults to indexing everything.)
    fn should_index_entity(&self, node: &SyntaxNode<'_>) -> bool {
        let _ = node;
        true
    }

    /// Whether a scope-kind node should be entered during the walk.
    /// (TS: `shouldEnterScope`; defaults to entering everything.)
    fn should_enter_scope(&self, node: &SyntaxNode<'_>) -> bool {
        let _ = node;
        true
    }

    /// Expands one entity node into the nodes to index (default: itself).
    /// (TS: `resolveEntities`; js-ts fans `const x = {...}` into members.)
    fn resolve_entities<'a>(&self, node: &SyntaxNode<'a>) -> Vec<SyntaxNode<'a>> {
        vec![*node]
    }

    /// Node whose name/signature represent the scope (default: itself).
    /// (TS: `enterScopeNode`; go/python unwrap wrapper nodes.)
    fn enter_scope_node<'a>(&self, node: &SyntaxNode<'a>) -> SyntaxNode<'a> {
        *node
    }

    /// Resolves one scope node before naming it (default: itself).
    /// (TS: `resolveEntity`; kept for parity, currently identity everywhere.)
    fn resolve_entity<'a>(&self, node: &SyntaxNode<'a>) -> SyntaxNode<'a> {
        *node
    }

    /// Extends the scope breadcrumb for `node` (default: unchanged).
    fn scope_breadcrumb(&self, node: &SyntaxNode<'_>, breadcrumb: &[String]) -> Vec<String> {
        let _ = node;
        breadcrumb.to_vec()
    }

    /// Symbol type for nodes the generic walk cannot classify on its own.
    fn classify_node(
        &self,
        node: &SyntaxNode<'_>,
        breadcrumb: &[String],
    ) -> Option<CodeSymbolType> {
        let _ = node;
        let _ = breadcrumb;
        None
    }

    /// One-line signature for `node`, if one can be built.
    fn extract_signature(&self, node: &SyntaxNode<'_>) -> Option<String> {
        let _ = node;
        None
    }

    /// Preceding doc comment for `node`, if any.
    fn extract_doc(&self, node: &SyntaxNode<'_>) -> Option<String> {
        let _ = node;
        None
    }

    /// Modifiers (`exported`, `static`, …) for `node`.
    fn extract_modifiers(&self, node: &SyntaxNode<'_>) -> Vec<CodeEntityModifier> {
        let _ = node;
        Vec::new()
    }
}

/// Resolves the adapter for a structured code `format`.
///
/// Mirrors the `ADAPTERS` table in `adapter.ts`: `jsx` shares the javascript
/// adapter, `tsx` shares the typescript adapter. Returns `None` for formats
/// without structural extraction.
#[must_use]
pub fn resolve_adapter(format: &str) -> Option<&'static dyn LanguageAdapter> {
    match format {
        "c" => Some(&super::languages::c_lang::C_ADAPTER),
        "cpp" => Some(&super::languages::cpp::CPP_ADAPTER),
        "go" => Some(&super::languages::go::GO_ADAPTER),
        "java" => Some(&super::languages::java::JAVA_ADAPTER),
        "javascript" | "jsx" => Some(&super::languages::javascript::JAVASCRIPT_ADAPTER),
        "python" => Some(&super::languages::python::PYTHON_ADAPTER),
        "rust" => Some(&super::languages::rust::RUST_ADAPTER),
        "typescript" | "tsx" => Some(&super::languages::typescript::TYPESCRIPT_ADAPTER),
        _ => None,
    }
}

/// Finds the identifier leaf inside declarator wrappers.
///
/// Mirrors `findIdentifierLeaf` in `tree-sitter/nodes.ts`: descends through
/// `*_declarator` wrappers (up to 16 levels) and returns the first
/// `*identifier`, `destructor_name`, or `operator_name` node.
#[must_use]
pub fn find_identifier_leaf<'a>(node: &SyntaxNode<'a>) -> Option<SyntaxNode<'a>> {
    let mut current = *node;
    for _ in 0..16 {
        let kind = current.kind();
        if kind == "identifier" || kind.ends_with("_identifier") {
            return Some(current);
        }
        if kind == "destructor_name" || kind == "operator_name" {
            return Some(current);
        }
        match kind {
            "array_declarator"
            | "function_declarator"
            | "init_declarator"
            | "parenthesized_declarator"
            | "pointer_declarator"
            | "reference_declarator" => {
                current = current.field("declarator")?;
            }
            _ => return None,
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Probe;
    impl LanguageAdapter for Probe {
        fn format(&self) -> &'static str {
            "probe"
        }
        fn entity_types(&self) -> &'static [&'static str] {
            &["function_definition"]
        }
        fn scope_types(&self) -> &'static [&'static str] {
            &[]
        }
        fn extract_name(&self, node: &SyntaxNode<'_>) -> Option<String> {
            let _ = node;
            None
        }
    }

    #[test]
    fn defaults_index_enter_and_passthrough() {
        let adapter = Probe;
        assert!(adapter.is_entity_type("function_definition"));
        assert!(!adapter.is_entity_type("class_definition"));
        assert!(!adapter.is_scope_type("function_definition"));
        assert!(resolve_adapter("ruby").is_none());
        let resolved = resolve_adapter("tsx");
        assert!(resolved.is_some());
    }

    #[test]
    fn jsx_shares_javascript_tsx_shares_typescript() {
        let js = resolve_adapter("javascript").map(|a| a.format());
        let jsx = resolve_adapter("jsx").map(|a| a.format());
        let ts = resolve_adapter("typescript").map(|a| a.format());
        let tsx = resolve_adapter("tsx").map(|a| a.format());
        assert_eq!(js, jsx);
        assert_eq!(ts, tsx);
    }
}
