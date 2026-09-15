//! Shared signature/doc/modifier helpers for code adapters.
//!
//! Mirrors `engine/extraction/code/families/metadata.ts`: best-effort,
//! language-agnostic readers over [`SyntaxNode`]
//! used by every language adapter.

use crate::extraction::code::adapter::SyntaxNode;
use crate::types::CodeEntityModifier;

/// Node kinds treated as doc comments by [`extract_preceding_doc`].
pub const COMMENT_TYPES: &[&str] = &[
    "comment",
    "line_comment",
    "block_comment",
    "documentation_comment",
];

/// Named body node kinds stripped off the end of a signature.
const BODY_TYPES: &[&str] = &[
    "statement_block",
    "compound_statement",
    "block",
    "class_body",
    "declaration_list",
    "field_declaration_list",
];

/// Builds a one-line signature from the node's header.
///
/// Mirrors `extractGenericSignature`: prefers the text before the `body`
/// field (or a known body node kind), otherwise the first non-empty line.
/// Whitespace collapses to single spaces and a trailing `{`/`;` is removed.
pub fn extract_generic_signature(node: &SyntaxNode<'_>) -> Option<String> {
    let text = node.text()?;
    let body = node.field("body").or_else(|| {
        BODY_TYPES
            .iter()
            .find_map(|kind| node.named_child_of_kind(kind))
    });

    let header = match body {
        Some(body) if body.start_byte() > node.start_byte() => {
            let end = body.start_byte() - node.start_byte();
            text.get(..end).map(str::trim_end).unwrap_or("").to_string()
        }
        _ => first_non_empty_line(text).to_string(),
    };

    let mut normalized = String::with_capacity(header.len());
    for word in header.split_whitespace() {
        if !normalized.is_empty() {
            normalized.push(' ');
        }
        normalized.push_str(word);
    }
    let trimmed = normalized.trim_end_matches(['{', ';', ' ']).trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Collects contiguous preceding sibling comments as the node's doc text.
///
/// Mirrors `extractPrecedingDoc`: walks `previousNamedSibling` while the
/// sibling is a comment, cleans each with [`clean_comment_text`], and joins
/// them in source order. Diverges by skipping named `blank_line` siblings
/// (emitted by the vb-dotnet grammar after every comment); no other bundled
/// grammar emits that kind, so other languages are unaffected.
#[must_use]
pub fn extract_preceding_doc(node: &SyntaxNode<'_>) -> Option<String> {
    let mut comments = Vec::new();
    let mut sibling = node.prev_named_sibling();
    while let Some(current) = sibling {
        if current.kind() == "blank_line" {
            sibling = current.prev_named_sibling();
            continue;
        }
        if !COMMENT_TYPES.contains(&current.kind()) {
            break;
        }
        if let Some(text) = current.text() {
            comments.push(clean_comment_text(text));
        }
        sibling = current.prev_named_sibling();
    }
    if comments.is_empty() {
        return None;
    }
    let mut doc = String::new();
    for comment in comments.iter().rev() {
        if !doc.is_empty() {
            doc.push('\n');
        }
        doc.push_str(comment);
    }
    let doc = doc.trim();
    if doc.is_empty() {
        None
    } else {
        Some(doc.to_string())
    }
}

/// Reads `exported`-style modifiers from ancestry plus visibility keywords.
///
/// Mirrors `extractCommonModifiers`: adds `exported` inside an
/// `export_statement`, then scans the generic signature for
/// `public|private|protected|internal|static|async|pub` (`pub` normalizes to
/// `public`). Diverges by matching keywords case-insensitively (VB
/// `Public`/`Shared` capitalisation); no new keyword arms are added here.
#[must_use]
pub fn extract_common_modifiers(node: &SyntaxNode<'_>) -> Vec<CodeEntityModifier> {
    let mut modifiers: Vec<CodeEntityModifier> = Vec::new();
    let signature = extract_generic_signature(node)
        .or_else(|| node.text().map(|t| first_non_empty_line(t).to_string()));
    let haystack = match signature.as_deref() {
        Some(s) => s,
        None => return modifiers,
    };

    if is_inside_node_type(node, "export_statement") {
        push_unique(&mut modifiers, CodeEntityModifier::Exported);
    }
    for word in haystack.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')) {
        let lowered = word.to_ascii_lowercase();
        let modifier = match lowered.as_str() {
            "public" => Some(CodeEntityModifier::Public),
            "private" => Some(CodeEntityModifier::Private),
            "protected" => Some(CodeEntityModifier::Protected),
            "internal" => Some(CodeEntityModifier::Internal),
            "static" => Some(CodeEntityModifier::Static),
            "async" => Some(CodeEntityModifier::Async),
            "pub" => Some(CodeEntityModifier::Public),
            _ => None,
        };
        if let Some(modifier) = modifier {
            push_unique(&mut modifiers, modifier);
        }
    }
    modifiers
}

/// True when any ancestor of `node` has kind `kind`.
#[must_use]
pub fn is_inside_node_type(node: &SyntaxNode<'_>, kind: &str) -> bool {
    closest_ancestor(node, kind).is_some()
}

/// Nearest ancestor of `node` with kind `kind`, if any.
#[must_use]
pub fn closest_ancestor<'a>(node: &SyntaxNode<'a>, kind: &str) -> Option<SyntaxNode<'a>> {
    let mut parent = node.parent();
    while let Some(current) = parent {
        if current.kind() == kind {
            return Some(current);
        }
        parent = current.parent();
    }
    None
}

/// First non-blank line of `text`, trimmed. Empty when `text` is blank.
#[must_use]
pub fn first_non_empty_line(text: &str) -> &str {
    // `str::lines` splits on `\n` and strips a trailing `\r`, matching the
    // TS `/\r?\n/` split without extra allocation.
    for line in text.lines() {
        let trimmed = line.trim();
        if !trimmed.is_empty() {
            return trimmed;
        }
    }
    ""
}

/// Strips comment markers (`//`, `#`, `/* */`, leading `*`) from doc text.
#[must_use]
pub fn clean_comment_text(text: &str) -> String {
    let mut stripped = text.trim();
    if stripped.starts_with("/**") || stripped.starts_with("/*") {
        stripped = stripped[2..].trim_start();
        if stripped.starts_with('*') {
            stripped = stripped[1..].trim_start();
        }
    }
    if let Some(without_close) = stripped.strip_suffix("*/") {
        stripped = without_close;
    }
    let mut out = String::with_capacity(stripped.len());
    for line in stripped.split('\n') {
        let is_blank = |c: char| c == ' ' || c == '\t' || c == '\r';
        let is_gap = |c: char| c == ' ' || c == '\t';
        let mut rest = line.trim_start_matches(is_blank);
        if let Some(after) = rest.strip_prefix("//") {
            rest = after.strip_prefix('/').unwrap_or(after);
            rest = rest.strip_prefix(is_gap).unwrap_or(rest);
        } else if let Some(after) = rest.strip_prefix('#') {
            rest = after.strip_prefix(is_gap).unwrap_or(after);
        } else if let Some(after) = rest.strip_prefix('*')
            && (after.starts_with(' ') || after.starts_with('\t') || after.is_empty())
        {
            rest = after.strip_prefix(is_gap).unwrap_or(after);
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(rest.trim_end());
    }
    out.trim().to_string()
}

fn push_unique(modifiers: &mut Vec<CodeEntityModifier>, modifier: CodeEntityModifier) {
    if !modifiers.contains(&modifier) {
        modifiers.push(modifier);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_line_skips_blanks() {
        assert_eq!(first_non_empty_line("\n  \nfoo  \nbar"), "foo");
        assert_eq!(first_non_empty_line("   "), "");
    }

    #[test]
    fn cleans_doc_styles() {
        assert_eq!(clean_comment_text("// hello"), "hello");
        assert_eq!(clean_comment_text("/// hello"), "hello");
        assert_eq!(clean_comment_text("# hello"), "hello");
        assert_eq!(clean_comment_text("/** hello */"), "hello");
        assert_eq!(clean_comment_text("/*\n * a\n * b\n */"), "a\nb");
    }
}
