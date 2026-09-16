//! Storage-filter resolution for validated search plans.

use crate::ids::FileId;
use crate::storage::{StorageSearchFilter, WorkspaceIndexStorage};
use crate::types::{FileInfo, ResolvedSearchPlan, SearchPlan};
use crate::utils::file_selection::{FileTypesMatcher, OrderedGlobs};
use crate::utils::glob::{
    CompiledGlob, has_path_glob, is_absolute_path_pattern, normalize_path_for_match,
    normalize_path_pattern,
};

/// Storage filter plus whether the file-id dimension resolved to an empty
/// set. The TS filter distinguishes "absent `fileIds`" from "present but
/// empty" (match nothing); the Rust filter uses plain vectors, so the flag
/// carries that bit (mirrors `filterMatchesNoFiles`).
pub(crate) struct ResolvedPlanFilter {
    pub(crate) filter: Option<StorageSearchFilter>,
    pub(crate) matches_no_files: bool,
}

pub(crate) fn search_plan_to_storage_filter(
    plan: &ResolvedSearchPlan,
    storage: &dyn WorkspaceIndexStorage,
    file_type_matcher: &FileTypesMatcher,
) -> ResolvedPlanFilter {
    let symbol_types = if plan.plan.symbol_types.is_empty() {
        None
    } else {
        Some(plan.plan.symbol_types.clone())
    };
    // Fast path: with no file-id dimension constraints there is nothing to
    // enumerate (mirrors the `None` from `resolve_filtered_file_ids` without
    // cloning metadata). Symbol-only plans still filter on `symbol_types`.
    let file_ids = if !file_id_constrained(plan) && file_type_matcher.is_empty() {
        None
    } else {
        resolve_filtered_file_ids(plan, &storage.list_file_refs(), file_type_matcher)
    };
    match (file_ids, symbol_types) {
        (None, None) => ResolvedPlanFilter {
            filter: None,
            matches_no_files: false,
        },
        (file_ids, symbol_types) => {
            let file_ids = file_ids.unwrap_or_default();
            // A resolved-but-empty file set matches nothing, even when symbol
            // dimensions are present (mirrors `filterMatchesNoFiles`).
            let matches_no_files = file_ids.is_empty() && file_id_constrained(plan);
            ResolvedPlanFilter {
                filter: Some(StorageSearchFilter {
                    file_ids,
                    group_ids: Vec::new(),
                    symbol_names: Vec::new(),
                    symbol_types: symbol_types.unwrap_or_default(),
                }),
                matches_no_files,
            }
        }
    }
}

/// True when the plan constrains the file-id dimension at all, so an empty
/// resolution means "match nothing" rather than "unfiltered".
fn file_id_constrained(plan: &ResolvedSearchPlan) -> bool {
    !plan.plan.include_paths.is_empty()
        || !plan.plan.exclude_paths.is_empty()
        || plan.plan.modified_after.is_some()
        || plan.plan.modified_before.is_some()
        || !plan.plan.globs.is_empty()
        || !plan.plan.insensitive_globs.is_empty()
        || !plan.plan.file_types.is_empty()
        || !plan.plan.excluded_file_types.is_empty()
}

fn resolve_filtered_file_ids(
    plan: &ResolvedSearchPlan,
    files: &[&FileInfo],
    file_type_matcher: &FileTypesMatcher,
) -> Option<Vec<FileId>> {
    let include_matchers: Vec<CompiledPathFilter> = plan
        .plan
        .include_paths
        .iter()
        .map(|pattern| CompiledPathFilter::new(pattern))
        .collect();
    let exclude_matchers: Vec<CompiledPathFilter> = plan
        .plan
        .exclude_paths
        .iter()
        .map(|pattern| CompiledPathFilter::new(pattern))
        .collect();
    let has_modified = plan.plan.modified_after.is_some() || plan.plan.modified_before.is_some();
    let has_shared = !plan.plan.globs.is_empty()
        || !plan.plan.insensitive_globs.is_empty()
        || !file_type_matcher.is_empty();
    if include_matchers.is_empty() && exclude_matchers.is_empty() && !has_modified && !has_shared {
        return None;
    }
    let globs = OrderedGlobs::new(&plan.plan.globs, &plan.plan.insensitive_globs);
    Some(
        files
            .iter()
            .filter(|file| {
                let file: &FileInfo = file;
                let included = include_matchers.is_empty()
                    || include_matchers.iter().any(|matcher| matcher.matches(file));
                let excluded = exclude_matchers.iter().any(|matcher| matcher.matches(file));
                included
                    && !excluded
                    && globs.matches(&file.relative_path)
                    && file_type_matcher.matches(std::path::Path::new(&file.relative_path))
                    && matches_modified_time_filter(file, &plan.plan)
            })
            .map(|file| file.id.clone())
            .collect(),
    )
}

fn matches_modified_time_filter(file: &FileInfo, plan: &SearchPlan) -> bool {
    if plan
        .modified_after
        .is_some_and(|after| file.last_modified_time.as_millis() < after.as_millis())
    {
        return false;
    }
    if plan
        .modified_before
        .is_some_and(|before| file.last_modified_time.as_millis() > before.as_millis())
    {
        return false;
    }
    true
}

/// Include/exclude path pattern compiled once per query: wildcard patterns
/// reuse the shared [`CompiledGlob`]; literal patterns keep exact/path-prefix
/// semantics without per-file regex work. Empty patterns match nothing, as do
/// patterns whose regex fails to compile.
struct CompiledPathFilter {
    absolute: bool,
    kind: CompiledPathKind,
}

enum CompiledPathKind {
    /// Empty pattern: matches nothing.
    Never,
    /// Literal pattern: exact match or anything underneath `prefix` (`None`
    /// for trailing-slash patterns, mirroring `path_pattern_matches`).
    Literal {
        expected: String,
        prefix: Option<String>,
    },
    /// Glob pattern: precompiled matcher (invalid patterns match nothing).
    Glob { matcher: CompiledGlob },
}

impl CompiledPathFilter {
    fn new(pattern: &str) -> Self {
        let absolute = is_absolute_path_pattern(pattern);
        let normalized = normalize_path_pattern(pattern);
        let kind = if normalized.is_empty() {
            CompiledPathKind::Never
        } else if has_path_glob(&normalized) {
            CompiledPathKind::Glob {
                matcher: CompiledGlob::new(pattern, false),
            }
        } else {
            let prefix = if normalized.ends_with('/') {
                None
            } else {
                Some(format!("{normalized}/"))
            };
            CompiledPathKind::Literal {
                expected: normalized,
                prefix,
            }
        };
        Self { absolute, kind }
    }

    fn matches(&self, file: &FileInfo) -> bool {
        match &self.kind {
            CompiledPathKind::Never => false,
            CompiledPathKind::Literal { expected, prefix } => {
                let path = if self.absolute {
                    normalize_path_for_match(&file.absolute_path)
                } else {
                    normalize_path_for_match(&file.relative_path)
                };
                path == *expected
                    || prefix
                        .as_deref()
                        .is_some_and(|prefix| path.starts_with(prefix))
            }
            CompiledPathKind::Glob { matcher } => {
                let path = if self.absolute {
                    file.absolute_path.as_str()
                } else {
                    file.relative_path.as_str()
                };
                matcher.matches(path)
            }
        }
    }
}
