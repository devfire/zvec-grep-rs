//! Workspace diff: scanned-vs-stored comparison, content hashing, path normalization.

use std::collections::{HashMap, HashSet};

use crate::error::{EngineError, EngineErrorCode, EngineResult};
use crate::types::FileInfo;
use crate::utils::hash::sha256_bytes;

use super::context::{DiffResult, file_context};

pub(crate) fn normalize_for_diff(path: &str) -> String {
    crate::paths::to_display_path(&crate::paths::normalize_path(std::path::Path::new(path)))
}

pub(crate) fn compute_diff_from_files(
    scanned_files: &[FileInfo],
    existing_files: &[FileInfo],
) -> EngineResult<DiffResult> {
    let existing_by_id: HashMap<&crate::ids::FileId, &FileInfo> =
        existing_files.iter().map(|file| (&file.id, file)).collect();
    let mut seen = HashSet::new();
    let mut diff = DiffResult::default();
    for file in scanned_files {
        seen.insert(file.id.clone());
        match existing_by_id.get(&file.id) {
            None => diff.added.push(file.clone()),
            Some(existing) => {
                if existing
                    .index_status
                    .as_ref()
                    .and_then(|status| status.indexed_time)
                    .is_none()
                {
                    diff.pending.push(file.clone());
                    continue;
                }
                if existing.size_bytes == file.size_bytes
                    && existing.last_modified_time == file.last_modified_time
                    && existing.content_hash.is_some()
                {
                    diff.unchanged.push((*existing).clone());
                    continue;
                }
                let hashed = with_content_hash(file)?;
                if existing.size_bytes == hashed.size_bytes
                    && existing.content_hash == hashed.content_hash
                {
                    diff.unchanged.push((*existing).clone());
                } else {
                    diff.modified.push(hashed);
                }
            }
        }
    }
    diff.deleted = existing_by_id
        .values()
        .filter(|file| !seen.contains(&file.id))
        .map(|file| (*file).clone())
        .collect();
    Ok(diff)
}

/// Drops stale deletions under unreadable directories (#63).
///
/// A directory the scanner could not read provides no evidence that its
/// previously indexed files were removed, so they must not be deleted (and
/// later fully re-embedded) on a transient read failure. Paths are compared
/// after display normalization, so separator/trailing-slash differences
/// between scanner output and stored paths cannot defeat the protection.
pub(crate) fn protect_unreadable_deletions(
    diff: &mut DiffResult,
    unreadable_directories: &[String],
) {
    if unreadable_directories.is_empty() || diff.deleted.is_empty() {
        return;
    }
    let protected: Vec<String> = unreadable_directories
        .iter()
        .map(|dir| normalize_for_diff(dir).trim_end_matches('/').to_owned())
        .collect();
    diff.deleted.retain(|file| {
        let path = normalize_for_diff(&file.absolute_path);
        !protected
            .iter()
            .any(|dir| path == *dir || path.starts_with(&format!("{dir}/")))
    });
}

pub(crate) fn with_content_hash(file: &FileInfo) -> EngineResult<FileInfo> {
    match std::fs::read(&file.absolute_path) {
        Ok(bytes) => {
            let mut hashed = file.clone();
            hashed.content_hash = Some(sha256_bytes(&bytes));
            Ok(hashed)
        }
        Err(err) => Err(EngineError::new(
            EngineErrorCode::IndexingContentHashFailed,
            "indexing failed to compute file content hash",
        )
        .with_context(format!("{}\ndetail={err}", file_context(file)))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_diff_of_equal_sets() {
        let diff = compute_diff_from_files(&[], &[]).expect("diff");
        assert!(diff.added.is_empty() && diff.deleted.is_empty());
    }
}
