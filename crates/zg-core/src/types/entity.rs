//! Entities: indexed content units bound to source locations.

use serde::{Deserialize, Serialize};

use crate::ids::{EntityId, FileId};

/// Source location of an entity within its file.
///
/// Wire shape follows the TS contract exactly: `kind` is snake_case
/// (`page_text`) while struct fields are camelCase (`startLine`). The
/// enum-level rule covers variants; each struct variant re-declares the
/// field rule.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Range {
    File,
    #[serde(rename_all = "camelCase")]
    Text {
        start_line: usize,
        end_line: usize,
        start_offset: usize,
        end_offset: usize,
    },
    #[serde(rename_all = "camelCase")]
    Byte {
        start_offset: usize,
        end_offset: usize,
    },
    Page {
        page: usize,
    },
    #[serde(rename_all = "camelCase")]
    PageText {
        page: usize,
        start_offset: usize,
        end_offset: usize,
    },
    PageRegion {
        page: usize,
        x: f64,
        y: f64,
        width: f64,
        height: f64,
    },
}

/// Structural kind of a code symbol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CodeSymbolType {
    Module,
    Class,
    Interface,
    Function,
    Value,
    Alias,
}

/// Modifiers attached to code entities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CodeEntityModifier {
    Exported,
    Async,
    Static,
    Public,
    Private,
    Protected,
    Internal,
}

/// Tree-sitter-aware metadata for code entities.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub struct CodeEntityMetadata {
    pub symbol_type: CodeSymbolType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub modifiers: Vec<CodeEntityModifier>,
}

/// Heading-derived metadata for markdown entities.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub struct MarkdownEntityMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heading: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
}

/// Format-specific entity metadata.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum EntityMetadata {
    Code(CodeEntityMetadata),
    Markdown(MarkdownEntityMetadata),
}

/// An indexed unit of content bound to a source location.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entity {
    pub id: EntityId,
    pub file_id: FileId,
    pub range: Range,
    pub content: Content,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<EntityMetadata>,
}

impl Entity {
    /// Code metadata if this entity carries any.
    pub fn code_metadata(&self) -> Option<&CodeEntityMetadata> {
        match &self.metadata {
            Some(EntityMetadata::Code(meta)) => Some(meta),
            _ => None,
        }
    }
}

/// An [`Entity`] plus the collapse group it belongs to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntityFragment {
    #[serde(flatten)]
    pub entity: Entity,
    /// Present when several fragments form one logical entity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
}

impl EntityFragment {
    /// Public identity used for group collapse: the group id when set,
    /// otherwise the fragment's own id.
    pub fn public_id(&self) -> &str {
        self.group.as_deref().unwrap_or(self.entity.id.as_str())
    }
}

use crate::types::Content;
