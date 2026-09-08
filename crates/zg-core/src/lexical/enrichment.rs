//! Structural container enrichment of lexical matches (100-file limit).
//!
//! Port of `engine/service/structure-enrichment.ts`
//! (`enrichLexicalItemsWithStructure`): for each distinct file behind a
//! lexical match, re-extract its structural fragments (code symbols, markdown
//! sections) and attach the smallest fragment containing the match as the
//! item's `container`, filling `metadata` when the item has none.
//!
//! The whole pass is best-effort: unreadable, oversized, non-structural, or
//! fragment-free files yield `None` and leave their items untouched, exactly
//! like the TS `try/catch → null` chain.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::EngineResult;
use crate::types::EntityFragment;

/// Default cap on distinct match files to parse, mirroring
/// `RG_STRUCTURE_ENRICH_FILE_LIMIT`.
pub const STRUCTURE_ENRICH_FILE_LIMIT: usize = 100;

/// Namespace mixed into the synthetic file id, mirroring
/// `STRUCTURE_ENRICH_FILE_ID_NAMESPACE`.
const STRUCTURE_FILE_ID_NAMESPACE: &str = "__rg_structure__";

/// Diagnostics for one enrichment pass, mirroring
/// `ZvecGrepStructureEnrichmentDiagnostics`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StructureEnrichmentDiagnostics {
    pub source: String,
    pub file_limit: usize,
    pub matched_files: usize,
    pub parsed_files: usize,
    pub enriched_files: usize,
    pub enriched_items: usize,
    pub skipped_files: usize,
    pub truncated: bool,
}

/// Output of [`enrich_lexical_items_with_structure`].
#[derive(Debug, Clone)]
pub struct StructureEnrichmentResult {
    pub items: Vec<crate::service::types::ContextItem>,
    pub diagnostics: StructureEnrichmentDiagnostics,
}

/// Attaches the smallest containing structural fragment to every lexical
/// match in `items`.
///
/// `root` is the workspace root the items were searched under, `file_limit`
/// caps how many distinct files are parsed (pass
/// [`STRUCTURE_ENRICH_FILE_LIMIT`] for TS parity), and
/// `max_file_size_bytes` overrides the per-kind size cap for the re-read
/// files. Only [`crate::service::types::ContextItemKind::RgMatch`] items are considered; everything
/// else passes through untouched.
pub fn enrich_lexical_items_with_structure(
    root: &Path,
    items: &[crate::service::types::ContextItem],
    file_limit: usize,
    max_file_size_bytes: Option<u64>,
) -> EngineResult<StructureEnrichmentResult> {
    let matched_files = unique_lexical_file_paths(items);
    let selected: HashSet<&str> = matched_files
        .iter()
        .take(file_limit)
        .map(String::as_str)
        .collect();

    let mut fragments_by_file: HashMap<&str, Option<Vec<EntityFragment>>> = HashMap::new();
    let mut parsed_files = 0usize;
    for path in selected {
        let fragments = parse_structural_fragments(root, Path::new(path), max_file_size_bytes);
        if fragments.is_some() {
            parsed_files += 1;
        }
        fragments_by_file.insert(path, fragments);
    }

    let mut enriched_items = 0usize;
    let mut enriched_files: HashSet<String> = HashSet::new();
    let mut enriched = Vec::with_capacity(items.len());
    for item in items {
        enriched.push(enrich_lexical_item(
            item,
            &fragments_by_file,
            &mut enriched_items,
            &mut enriched_files,
        ));
    }
    let matched_count = matched_files.len();
    Ok(StructureEnrichmentResult {
        items: enriched,
        diagnostics: StructureEnrichmentDiagnostics {
            source: "structural_extraction".to_owned(),
            file_limit,
            matched_files: matched_count,
            parsed_files,
            enriched_files: enriched_files.len(),
            enriched_items,
            skipped_files: matched_count.saturating_sub(parsed_files),
            truncated: matched_count > file_limit,
        },
    })
}

fn enrich_lexical_item(
    item: &crate::service::types::ContextItem,
    fragments_by_file: &HashMap<&str, Option<Vec<EntityFragment>>>,
    enriched_items: &mut usize,
    enriched_files: &mut HashSet<String>,
) -> crate::service::types::ContextItem {
    use crate::service::types::{ContextItemKind, StructuralContainer};

    if item.kind != ContextItemKind::RgMatch {
        return item.clone();
    }
    let key = normalize_key(&item.file.absolute_path);
    let Some(Some(fragments)) = fragments_by_file.get(key.as_str()) else {
        return item.clone();
    };
    if fragments.is_empty() {
        return item.clone();
    }
    let Some(match_range) = lexical_match_range(item) else {
        return item.clone();
    };
    let Some(container) = smallest_containing_fragment(fragments, match_range) else {
        return item.clone();
    };

    *enriched_items += 1;
    enriched_files.insert(key);
    let entity_id = container
        .group
        .clone()
        .map(crate::ids::EntityId::from_raw)
        .unwrap_or_else(|| container.entity.id.clone());
    let mut enriched = item.clone();
    // `container.metadata ?? item.metadata`: the fragment wins when present.
    if container.entity.metadata.is_some() {
        enriched.metadata = container.entity.metadata.clone();
    }
    enriched.container = Some(StructuralContainer {
        entity_id,
        range: container.entity.range.clone(),
        metadata: container.entity.metadata.clone(),
    });
    enriched
}
/// Best-effort structural parse of one matched file; `None` when the file
/// cannot or should not contribute fragments.
fn parse_structural_fragments(
    root: &Path,
    absolute_path: &Path,
    max_file_size_bytes: Option<u64>,
) -> Option<Vec<EntityFragment>> {
    let file = file_info_for_structure(root, absolute_path, max_file_size_bytes)?;
    let bytes = std::fs::read(absolute_path).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    let source = crate::extraction::Source::Text {
        file: &file,
        text: text.as_ref(),
    };
    let fragments =
        crate::extraction::extract(&source, &crate::extraction::ChunkOptions::default()).ok()?;
    let structural: Vec<EntityFragment> = fragments
        .into_iter()
        .filter(is_structural_fragment)
        .collect();
    if structural.is_empty() {
        return None;
    }
    Some(structural)
}
/// Only code and markdown fragments can contain a match (mirrors
/// `isStructuralFragment`).
fn is_structural_fragment(fragment: &EntityFragment) -> bool {
    matches!(
        fragment.entity.metadata,
        Some(crate::types::EntityMetadata::Code(_))
            | Some(crate::types::EntityMetadata::Markdown(_))
    )
}

/// Builds the synthetic [`FileInfo`] for re-extraction, mirroring
/// `fileInfoForStructure`: regular non-empty files only, enrichable types
/// only (code, or markdown text), within the resolved size cap.
fn file_info_for_structure(
    root: &Path,
    absolute_path: &Path,
    max_file_size_bytes: Option<u64>,
) -> Option<crate::types::FileInfo> {
    let metadata = std::fs::metadata(absolute_path).ok()?;
    if !metadata.is_file() || metadata.len() == 0 {
        return None;
    }
    let detected = crate::file_type::detect_file_type(absolute_path)?;
    if !is_structurally_enrichable(&detected) {
        return None;
    }
    if metadata.len()
        > crate::file_size_policy::resolve_max_file_size_bytes(detected.kind, max_file_size_bytes)
    {
        return None;
    }
    let absolute = crate::paths::normalize_path(absolute_path);
    let relative =
        crate::paths::to_display_path(absolute.strip_prefix(root).unwrap_or(absolute.as_path()));
    let mtime_millis = metadata
        .modified()
        .ok()
        .and_then(|time| {
            time.duration_since(std::time::UNIX_EPOCH)
                .ok()
                .map(|d| d.as_millis() as i64)
        })
        .unwrap_or(0);
    Some(crate::types::FileInfo {
        id: crate::ids::FileId::from_raw(make_structure_file_id(&absolute)),
        absolute_path: crate::paths::to_display_path(&absolute),
        relative_path: if relative.is_empty() {
            ".".to_owned()
        } else {
            relative
        },
        root_path: crate::paths::to_display_path(root),
        size_bytes: metadata.len(),
        last_modified_time: crate::types::UnixMillis::from_millis(mtime_millis),
        content_hash: None,
        kind: detected.kind,
        format: detected.format,
        index_status: None,
    })
}

/// Code files plus markdown texts can carry a container (mirrors
/// `isStructurallyEnrichableFile`).
fn is_structurally_enrichable(file: &crate::file_type::FileType) -> bool {
    use crate::types::FileKind;

    file.kind == FileKind::Code
        || (file.kind == FileKind::Text && file.format.as_str() == "markdown")
}

/// The match's own range, preferring the pre-expansion excerpt range
/// (mirrors `lexicalMatchRange`).
fn lexical_match_range(item: &crate::service::types::ContextItem) -> Option<(usize, usize)> {
    let range = item.excerpt_range.as_ref().unwrap_or(&item.range);
    match range {
        crate::types::Range::Text {
            start_line,
            end_line,
            ..
        } => Some((*start_line, *end_line)),
        _ => None,
    }
}

/// Smallest fragment whose line span covers the match, mirroring
/// `smallestContainingFragment` (containment is line-based; columns are
/// ignored, exactly like the TS `textRangeContains`).
fn smallest_containing_fragment<'a>(
    fragments: &'a [EntityFragment],
    match_range: (usize, usize),
) -> Option<&'a EntityFragment> {
    let (start, end) = match_range;
    let mut best: Option<&'a EntityFragment> = None;
    for fragment in fragments {
        if !text_range_contains(&fragment.entity.range, start, end) {
            continue;
        }
        let replace = match best {
            None => true,
            Some(current) => compare_fragment_container(fragment, current) < 0,
        };
        if replace {
            best = Some(fragment);
        }
    }
    best
}

fn text_range_contains(range: &crate::types::Range, start: usize, end: usize) -> bool {
    match range {
        crate::types::Range::Text {
            start_line,
            end_line,
            ..
        } => *start_line <= start && *end_line >= end,
        _ => false,
    }
}

/// Orders candidate containers: smaller line span first, then higher
/// specificity, then id (mirrors `compareFragmentContainer`, including the
/// reversed specificity subtraction).
fn compare_fragment_container(left: &EntityFragment, right: &EntityFragment) -> i64 {
    let span = |fragment: &EntityFragment| match fragment.entity.range {
        crate::types::Range::Text {
            start_line,
            end_line,
            ..
        } => end_line as i64 - start_line as i64,
        _ => i64::MAX,
    };
    span(left).saturating_sub(span(right).clamp(i64::MIN + 1, i64::MAX - 1))
        + i64::from(
            (fragment_specificity_score(right) - fragment_specificity_score(left)).clamp(-1, 1),
        )
        .saturating_mul(0)
        + specificity_then_id(left, right)
}

/// Distinct normalized file paths behind lexical matches, preserving
/// first-seen order (mirrors the TS distinct-file collection).
fn unique_lexical_file_paths(items: &[crate::service::types::ContextItem]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut paths = Vec::new();
    for item in items {
        if item.kind != crate::service::types::ContextItemKind::RgMatch {
            continue;
        }
        let key = normalize_key(&item.file.absolute_path);
        if seen.insert(key.clone()) {
            paths.push(key);
        }
    }
    paths
}

/// Specificity delta plus id tiebreak, kept as one helper so the primary
/// line-span ordering above stays readable.
fn specificity_then_id(left: &EntityFragment, right: &EntityFragment) -> i64 {
    let order = (fragment_specificity_score(right) - fragment_specificity_score(left))
        .cmp(&0)
        .then_with(|| left.entity.id.as_str().cmp(right.entity.id.as_str()));
    match order {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    }
}

/// Code symbols with names outrank anonymous ones; markdown sections with
/// headings outrank plain chunks (mirrors `fragmentSpecificityScore`).
fn fragment_specificity_score(fragment: &EntityFragment) -> i32 {
    match &fragment.entity.metadata {
        Some(crate::types::EntityMetadata::Code(meta)) => i32::from(meta.symbol_name.is_none()) + 1,
        Some(crate::types::EntityMetadata::Markdown(meta)) => i32::from(meta.heading.is_some()),
        None => 0,
    }
}

/// Synthetic file id for re-extraction, mirroring `makeStructureFileId`
/// (sha256 of `namespace + NUL + normalized path`).
fn make_structure_file_id(absolute: &Path) -> String {
    crate::utils::hash::sha256_text(&format!(
        "{STRUCTURE_FILE_ID_NAMESPACE}\0{}",
        crate::paths::to_display_path(absolute)
    ))
}

/// Normalizes an absolute path string for match-file keying (mirrors
/// `normalizePath` on the TS side: separators plus slash collapsing).
fn normalize_key(absolute_path: &str) -> String {
    let mnoho = absolute_path.replace('\\', "/");
    let mut collapsed = String::with_capacity(mnoho.len());
    let mut previous_slash = false;
    for ch in mnoho.chars() {
        if ch == '/' {
            if previous_slash {
                continue;
            }
            previous_slash = true;
        } else {
            previous_slash = false;
        }
        collapsed.push(ch);
    }
    collapsed
}

/// Absolute display of a path for file-key purposes.
fn _absolute_display(path: &Path) -> PathBuf {
    crate::paths::normalize_path(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::types::{ContentStatus, ContextFile, ContextItemKind};
    use std::io::Write as _;

    fn lexical_item(absolute_path: &str, line: usize) -> crate::service::types::ContextItem {
        crate::service::types::ContextItem {
            kind: ContextItemKind::RgMatch,
            rank: 1,
            file: ContextFile {
                absolute_path: absolute_path.to_owned(),
                relative_path: "main.rs".to_owned(),
                root_path: String::new(),
            },
            range: crate::types::Range::Text {
                start_line: line,
                end_line: line,
                start_offset: 0,
                end_offset: 3,
            },
            excerpt_range: None,
            content: crate::types::Content::Text {
                text: "foo".to_owned(),
            },
            content_role: None,
            outline: None,
            status: ContentStatus::Fresh,
            score: None,
            matched_by: Some("lexical".to_owned()),
            metadata: None,
            entity_id: None,
            trace: None,
            query_groups: Vec::new(),
            container: None,
            selection_reason: None,
        }
    }

    #[test]
    fn enriches_rust_match_with_containing_symbol() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("main.rs");
        let mut file = std::fs::File::create(&path).expect("create");
        file.write_all(b"fn alpha() {\n    let needle = 1;\n}\n")
            .expect("write");

        let absolute = crate::paths::to_display_path(&crate::paths::normalize_path(&path));
        let items = vec![lexical_item(&absolute, 2)];
        let result = enrich_lexical_items_with_structure(
            dir.path(),
            &items,
            STRUCTURE_ENRICH_FILE_LIMIT,
            None,
        )
        .expect("enrich");
        assert_eq!(result.items.len(), 1);
        assert_eq!(result.diagnostics.matched_files, 1);
        assert_eq!(result.diagnostics.parsed_files, 1);
        assert_eq!(result.diagnostics.enriched_items, 1);
        assert!(!result.diagnostics.truncated);
        let container = result.items[0].container.as_ref().expect("container");
        assert!(!container.entity_id.as_str().is_empty());
    }

    #[test]
    fn non_code_files_pass_through_untouched() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("notes.csv");
        let mut file = std::fs::File::create(&path).expect("create");
        file.write_all(b"a,b\nneedle,2\n").expect("write");

        let absolute = crate::paths::to_display_path(&crate::paths::normalize_path(&path));
        let items = vec![lexical_item(&absolute, 2)];
        let result = enrich_lexical_items_with_structure(
            dir.path(),
            &items,
            STRUCTURE_ENRICH_FILE_LIMIT,
            None,
        )
        .expect("enrich");
        assert!(result.items[0].container.is_none());
        assert_eq!(result.diagnostics.enriched_items, 0);
        assert_eq!(result.diagnostics.skipped_files, 1);
    }

    #[test]
    fn file_limit_truncates() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut items = Vec::new();
        for index in 0..3 {
            let name = format!("f{index}.rs");
            let path = dir.path().join(&name);
            let mut file = std::fs::File::create(&path).expect("create");
            file.write_all(b"fn f() {\n    let needle = 1;\n}\n")
                .expect("write");
            let absolute = crate::paths::to_display_path(&crate::paths::normalize_path(&path));
            items.push(lexical_item(&absolute, 2));
        }
        let result =
            enrich_lexical_items_with_structure(dir.path(), &items, 2, None).expect("enrich");
        assert!(result.diagnostics.truncated);
        assert_eq!(result.diagnostics.matched_files, 3);
        assert_eq!(result.diagnostics.file_limit, 2);
    }
}
