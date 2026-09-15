//! Code extractor entry point: indexing orchestration, chunk budgets, fallback.
//!
//! Port of `engine/extraction/code/extractor.ts` (`CodeExtractor`). Structured
//! formats parse with the native grammar crates (the TS port uses WASM
//! grammars); component formats (`vue`, `svelte`) extract `<script>` blocks
//! and remap their fragments into host coordinates.
//!
//! Divergences from the TypeScript original:
//! - Budgets: TS counts UTF-16 code units (`string.length`); this port counts
//!   Unicode scalar values (`chars().count()`), identical for the BMP.
//! - Offsets in [`Range`] are byte offsets; TS reports UTF-16-unit offsets.
//! - `truncateInline` is dead code in the TS original and is not ported.

use crate::code_formats::is_component_code_format;
use crate::error::{EngineError, EngineResult, codes};
use crate::extraction::code::adapter::{SyntaxNode, resolve_adapter};
use crate::extraction::code::extractor::script_blocks::extract_script_blocks;
use crate::extraction::code::extractor::walk::{code_entity_to_search_fragments, walk_code_node};
use crate::extraction::text::extract_plain_text_fragments;
use crate::extraction::{ChunkOptions, ExtractedFragment};
use crate::ids::make_entity_id;
use crate::types::{
    CodeEntityMetadata, CodeEntityModifier, CodeSymbolType, Content, Entity, EntityFragment,
    EntityMetadata, FileInfo, Range,
};

/// Formats with a native grammar available (mirrors `LANGUAGE_WASM_MAP`).
fn has_grammar(format: &str) -> bool {
    matches!(
        format,
        "c" | "cpp"
            | "csharp"
            | "go"
            | "java"
            | "javascript"
            | "jsx"
            | "python"
            | "rust"
            | "tsx"
            | "typescript"
            | "vb"
    )
}

fn language_for_format(format: &str) -> Option<tree_sitter::Language> {
    let language = match format {
        "c" => tree_sitter_c::LANGUAGE.into(),
        "cpp" => tree_sitter_cpp::LANGUAGE.into(),
        "csharp" => tree_sitter_c_sharp::LANGUAGE.into(),
        "go" => tree_sitter_go::LANGUAGE.into(),
        "java" => tree_sitter_java::LANGUAGE.into(),
        "javascript" | "jsx" => tree_sitter_javascript::LANGUAGE.into(),
        "python" => tree_sitter_python::LANGUAGE.into(),
        "rust" => tree_sitter_rust::LANGUAGE.into(),
        "typescript" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        "tsx" => tree_sitter_typescript::LANGUAGE_TSX.into(),
        "vb" => tree_sitter_vb_dotnet::LANGUAGE.into(),
        _ => return None,
    };
    Some(language)
}
/// Entry point used by [`crate::extraction`]: structural fragments plus
/// per-fragment embedding content, or `None` when this file is not code.
///
/// # Errors
///
/// Returns `EXTRACTORS.CODE_INVALID_CHUNK_SIZE` when the chunk size is zero,
/// `EXTRACTORS.CODE_INVALID_CHUNK_OVERLAP` when overlap is not smaller than the chunk
/// size, or the component script-block extraction error.
pub fn extract_for_indexing(
    file: &FileInfo,
    text: &str,
    options: &ChunkOptions,
) -> EngineResult<Option<Vec<ExtractedFragment>>> {
    if !file.kind.is_code() {
        return Ok(None);
    }
    let (max_chunk_chars, chunk_overlap_chars) = resolve_code_chunk_options(options)?;

    if is_component_code_format(file.format.as_str()) {
        let fragments = extract_script_blocks(file, text, max_chunk_chars, chunk_overlap_chars)?;
        if fragments.is_empty() {
            return Ok(Some(fallback(
                file,
                text,
                max_chunk_chars,
                chunk_overlap_chars,
            )));
        }
        return Ok(Some(fragments));
    }

    let format = file.format.as_str();
    let Some(adapter) = resolve_adapter(format) else {
        return Ok(Some(fallback(
            file,
            text,
            max_chunk_chars,
            chunk_overlap_chars,
        )));
    };
    if !has_grammar(format) {
        return Ok(Some(fallback(
            file,
            text,
            max_chunk_chars,
            chunk_overlap_chars,
        )));
    }
    let Some(language) = language_for_format(format) else {
        return Ok(Some(fallback(
            file,
            text,
            max_chunk_chars,
            chunk_overlap_chars,
        )));
    };

    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(&language).is_err() {
        return Ok(Some(fallback(
            file,
            text,
            max_chunk_chars,
            chunk_overlap_chars,
        )));
    }
    let Some(tree) = parser.parse(text.as_bytes(), None) else {
        return Ok(Some(fallback(
            file,
            text,
            max_chunk_chars,
            chunk_overlap_chars,
        )));
    };

    let bytes = text.as_bytes();
    let root = SyntaxNode::new(tree.root_node(), bytes);
    let mut collected: Vec<CodeEntity<'_>> = Vec::new();
    walk_code_node(root, adapter, &[], &mut collected);

    let mut out = Vec::new();
    let mut entity_id_index = 0usize;
    for entity in &collected {
        let raws =
            code_entity_to_search_fragments(adapter, entity, max_chunk_chars, chunk_overlap_chars);
        let major_id = if raws.first().is_some_and(|raw| raw.mark == GroupMark::Major) {
            Some(make_entity_id(&file.id, entity_id_index))
        } else {
            None
        };
        for raw in raws {
            let id = make_entity_id(&file.id, entity_id_index);
            entity_id_index += 1;
            let group = match raw.mark {
                GroupMark::Major => Some(id.clone()),
                GroupMark::Single => None,
                GroupMark::Minor => major_id.clone(),
            };
            out.push(ExtractedFragment {
                fragment: EntityFragment {
                    entity: Entity {
                        id,
                        file_id: file.id.clone(),
                        range: raw.range,
                        content: Content::Text { text: raw.text },
                        metadata: Some(EntityMetadata::Code(raw.metadata)),
                    },
                    group: group.map(|id| id.as_str().to_owned()),
                },
                embedding_source: raw.embedding_text.map(|text| Content::Text { text }),
            });
        }
    }
    if out.is_empty() {
        return Ok(Some(fallback(
            file,
            text,
            max_chunk_chars,
            chunk_overlap_chars,
        )));
    }
    Ok(Some(out))
}

fn resolve_code_chunk_options(options: &ChunkOptions) -> EngineResult<(usize, usize)> {
    let max_chunk_chars = options.max_chunk_chars();
    let chunk_overlap_chars = options.overlap_chars();
    if max_chunk_chars == 0 {
        return Err(EngineError::new(
            codes::extractor_code_invalid_chunk_size(),
            "code extractor requires a positive integer chunk size",
        )
        .with_context(format!("maxChunkChars={max_chunk_chars}")));
    }
    if chunk_overlap_chars >= max_chunk_chars {
        return Err(EngineError::new(
            codes::extractor_code_invalid_chunk_overlap(),
            "code extractor requires overlap to be smaller than chunk size",
        )
        .with_context(format!(
            "maxChunkChars={max_chunk_chars} chunkOverlapChars={chunk_overlap_chars}"
        )));
    }
    Ok((max_chunk_chars, chunk_overlap_chars))
}

fn fallback(
    file: &FileInfo,
    text: &str,
    max_chunk_chars: usize,
    chunk_overlap_chars: usize,
) -> Vec<ExtractedFragment> {
    extract_plain_text_fragments(file, text, max_chunk_chars, chunk_overlap_chars)
        .into_iter()
        .map(|fragment| ExtractedFragment {
            fragment,
            embedding_source: None,
        })
        .collect()
}

/// One collected entity: a tree node plus its resolved adapter data.
pub(crate) struct CodeEntity<'a> {
    pub(crate) node: SyntaxNode<'a>,
    pub(crate) name: Option<String>,
    pub(crate) symbol_type: CodeSymbolType,
    pub(crate) breadcrumb: Vec<String>,
    pub(crate) signature: Option<String>,
    pub(crate) doc: Option<String>,
    pub(crate) modifiers: Vec<CodeEntityModifier>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GroupMark {
    /// First fragment of a split entity (TS `group: ""`).
    Major,
    /// Whole entity in one fragment (TS: no `group` field).
    Single,
    /// Continuation chunk of a split entity (inherits the major id).
    Minor,
}

pub(crate) struct RawFragment {
    pub(crate) mark: GroupMark,
    pub(crate) range: Range,
    pub(crate) text: String,
    pub(crate) embedding_text: Option<String>,
    pub(crate) metadata: CodeEntityMetadata,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::FileId;
    use crate::types::{FileFormat, FileKind};

    fn rust_file(id: &str) -> FileInfo {
        FileInfo {
            id: FileId::from_raw(id.to_owned()),
            absolute_path: "/repo/main.rs".to_owned(),
            relative_path: "main.rs".to_owned(),
            root_path: "/repo".to_owned(),
            size_bytes: 0,
            last_modified_time: crate::types::UnixMillis::from_millis(0),
            content_hash: None,
            kind: FileKind::Code,
            format: FileFormat::parse("rust"),
            index_status: None,
        }
    }

    #[test]
    fn extracts_named_function_entity() {
        let file = rust_file("f");
        let out = extract_for_indexing(&file, "fn alpha() {\n    1\n}\n", &ChunkOptions::default())
            .expect("extract")
            .expect("code");
        assert!(!out.is_empty());
        assert!(
            out.iter()
                .all(|item| item.fragment.entity.file_id == file.id)
        );
        let names: Vec<_> = out
            .iter()
            .filter_map(|item| item.fragment.entity.code_metadata())
            .filter_map(|meta| meta.symbol_name.clone())
            .collect();
        assert!(names.iter().any(|name| name == "alpha"));
    }

    #[test]
    fn rejects_bad_chunk_options() {
        let file = rust_file("f");
        let bad = ChunkOptions {
            max_chunk_chars: Some(0),
            overlap_chars: None,
        };
        assert!(extract_for_indexing(&file, "fn f() {}", &bad).is_err());
    }
    fn code_file(id: &str, format: &str, relative_path: &str) -> FileInfo {
        FileInfo {
            id: FileId::from_raw(id.to_owned()),
            absolute_path: format!("/repo/{relative_path}"),
            relative_path: relative_path.to_owned(),
            root_path: "/repo".to_owned(),
            size_bytes: 0,
            last_modified_time: crate::types::UnixMillis::from_millis(0),
            content_hash: None,
            kind: FileKind::Code,
            format: FileFormat::parse(format),
            index_status: None,
        }
    }

    fn symbol_pairs(out: &[ExtractedFragment]) -> Vec<(Option<String>, CodeSymbolType)> {
        out.iter()
            .filter_map(|item| item.fragment.entity.code_metadata())
            .map(|meta| (meta.symbol_name.clone(), meta.symbol_type))
            .collect()
    }

    fn assert_symbol(
        pairs: &[(Option<String>, CodeSymbolType)],
        name: &str,
        symbol_type: CodeSymbolType,
    ) {
        assert!(
            pairs
                .iter()
                .any(|pair| pair == &(Some(name.to_owned()), symbol_type)),
            "expected ({name:?}, {symbol_type:?}) in {pairs:?}"
        );
    }

    fn parse_csharp(source: &str) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_c_sharp::LANGUAGE.into())
            .expect("c-sharp grammar loads");
        parser.parse(source, None).expect("c-sharp source parses")
    }

    fn parse_vb(source: &str) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_vb_dotnet::LANGUAGE.into())
            .expect("vb-dotnet grammar loads");
        parser.parse(source, None).expect("vb source parses")
    }

    fn extract_symbols(file: &FileInfo, source: &str) -> Vec<(Option<String>, CodeSymbolType)> {
        let out = extract_for_indexing(file, source, &ChunkOptions::default())
            .expect("extract")
            .expect("code");
        assert!(!out.is_empty(), "structured extraction emits fragments");
        symbol_pairs(&out)
    }

    const CSHARP_BLOCK_FIXTURE: &str = "namespace A.B\n{\n    public class Greeter\n    {\n        public Greeter()\n        {\n        }\n        public void SayHello()\n        {\n        }\n        public string Name { get; set; }\n    }\n    public struct Point\n    {\n        public int X;\n    }\n    public interface IShape\n    {\n        void Draw();\n    }\n    public enum Color\n    {\n        Red\n    }\n    public record Person\n    {\n        public string Name { get; init; }\n    }\n    public delegate void Notify(string message);\n}\n";

    #[test]
    fn csharp_block_namespace_names_and_types() {
        assert!(
            !parse_csharp(CSHARP_BLOCK_FIXTURE).root_node().has_error(),
            "c-sharp fixture parses without error nodes"
        );
        let file = code_file("cs1", "csharp", "Greeter.cs");
        let pairs = extract_symbols(&file, CSHARP_BLOCK_FIXTURE);
        assert_symbol(&pairs, "A.B", CodeSymbolType::Module);
        assert_symbol(&pairs, "Greeter", CodeSymbolType::Class);
        assert_symbol(&pairs, "Greeter", CodeSymbolType::Function);
        assert_symbol(&pairs, "SayHello", CodeSymbolType::Function);
        assert_symbol(&pairs, "Name", CodeSymbolType::Value);
        assert_symbol(&pairs, "Point", CodeSymbolType::Class);
        assert_symbol(&pairs, "IShape", CodeSymbolType::Interface);
        assert_symbol(&pairs, "Draw", CodeSymbolType::Function);
        assert_symbol(&pairs, "Color", CodeSymbolType::Class);
        assert_symbol(&pairs, "Person", CodeSymbolType::Class);
        assert_symbol(&pairs, "Notify", CodeSymbolType::Value);
    }

    #[test]
    fn csharp_file_scoped_namespace_names_and_types() {
        let source = "namespace Solo.Ns;\npublic class Solo\n{\n}\n";
        assert!(
            !parse_csharp(source).root_node().has_error(),
            "file-scoped fixture parses without error nodes"
        );
        let file = code_file("cs2", "csharp", "Solo.cs");
        let pairs = extract_symbols(&file, source);
        assert_symbol(&pairs, "Solo.Ns", CodeSymbolType::Module);
        assert_symbol(&pairs, "Solo", CodeSymbolType::Class);
    }

    #[test]
    fn csharp_dropped_kinds_stay_absent() {
        use crate::extraction::code::languages::csharp::ENTITY_TYPES;
        assert_eq!(ENTITY_TYPES.len(), 11);
        for dropped in [
            "destructor_declaration",
            "indexer_declaration",
            "operator_declaration",
            "conversion_operator_declaration",
            "local_function_statement",
            "event_declaration",
            "event_field_declaration",
        ] {
            assert!(
                !ENTITY_TYPES.contains(&dropped),
                "{dropped} must stay out of ENTITY_TYPES"
            );
        }
    }

    const VB_FIXTURE: &str = "Public Module Helpers\nEnd Module\nNamespace Acme\n    ' Greeter doc\n\n    Public Class Greeter\n        Public Sub New()\n        End Sub\n        Public Property Name As String\n        Public Shared Sub SayHello()\n        End Sub\n        Public Event Changed As EventHandler\n        Public Delegate Sub Notify(message As String)\n    End Class\n    Public Interface IShape\n    End Interface\n    Public Structure Point\n    End Structure\n    Public Enum Color\n        Red\n    End Enum\nEnd Namespace\n";

    #[test]
    fn vb_names_types_doc_and_modifiers() {
        assert!(
            !parse_vb(VB_FIXTURE).root_node().has_error(),
            "vb fixture parses without error nodes"
        );
        let file = code_file("vb1", "vb", "Greeter.vb");
        let out = extract_for_indexing(&file, VB_FIXTURE, &ChunkOptions::default())
            .expect("extract")
            .expect("code");
        assert!(!out.is_empty(), "structured extraction emits fragments");
        let pairs = symbol_pairs(&out);
        assert_symbol(&pairs, "Helpers", CodeSymbolType::Module);
        assert_symbol(&pairs, "Acme", CodeSymbolType::Module);
        assert_symbol(&pairs, "Greeter", CodeSymbolType::Class);
        assert_symbol(&pairs, "New", CodeSymbolType::Function);
        assert_symbol(&pairs, "Name", CodeSymbolType::Value);
        assert_symbol(&pairs, "SayHello", CodeSymbolType::Function);
        assert_symbol(&pairs, "Changed", CodeSymbolType::Value);
        assert_symbol(&pairs, "Notify", CodeSymbolType::Value);
        assert_symbol(&pairs, "IShape", CodeSymbolType::Interface);
        assert_symbol(&pairs, "Point", CodeSymbolType::Class);
        assert_symbol(&pairs, "Color", CodeSymbolType::Class);
        let class_meta = out
            .iter()
            .filter_map(|item| item.fragment.entity.code_metadata())
            .find(|meta| meta.symbol_name.as_deref() == Some("Greeter"))
            .expect("class fragment");
        assert_eq!(class_meta.doc.as_deref(), Some("Greeter doc"));
        let method_meta = out
            .iter()
            .filter_map(|item| item.fragment.entity.code_metadata())
            .find(|meta| meta.symbol_name.as_deref() == Some("SayHello"))
            .expect("method fragment");
        assert!(method_meta.modifiers.contains(&CodeEntityModifier::Public));
        assert!(method_meta.modifiers.contains(&CodeEntityModifier::Static));
    }

    #[test]
    fn vb_dropped_kinds_stay_absent() {
        use crate::extraction::code::languages::vb::ENTITY_TYPES;
        assert_eq!(ENTITY_TYPES.len(), 11);
        for dropped in [
            "variable_declarator",
            "dim_statement",
            "const_declaration",
            "enum_member",
            "type_declaration",
        ] {
            assert!(
                !ENTITY_TYPES.contains(&dropped),
                "{dropped} must stay out of ENTITY_TYPES"
            );
        }
    }

    #[test]
    fn empty_dotnet_files_fall_back_without_error() {
        for (id, format, path) in [("e1", "csharp", "Empty.cs"), ("e2", "vb", "Empty.vb")] {
            let file = code_file(id, format, path);
            for source in ["", "   \n  \n"] {
                let out = extract_for_indexing(&file, source, &ChunkOptions::default())
                    .expect("fallback without error");
                assert!(out.is_some());
            }
        }
    }
}
