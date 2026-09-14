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
            None => diff.added.push(with_content_hash(file)?),
            Some(existing) => {
                if existing
                    .index_status
                    .as_ref()
                    .and_then(|status| status.indexed_time)
                    .is_none()
                {
                    diff.pending.push(with_content_hash(file)?);
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

pub(crate) fn with_content_hash(file: &FileInfo) -> EngineResult<FileInfo> {
    match std::fs::read(&file.absolute_path) {
        Ok(bytes) => {
            let mut hashed = file.clone();
            hashed.content_hash = Some(sha256_bytes(&bytes));
            Ok(hashed)
        }
        Err(err) => Err(EngineError::new(
            EngineErrorCode::from_static("INDEXING.CONTENT_HASH_FAILED"),
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
