//! File typing, skip diagnostics, binary sniffing, and file-id minting.

use std::collections::HashMap;
use std::path::Path;

use crate::error::EngineResult;
use crate::file_size_policy::resolve_max_file_size_bytes;
use crate::file_type::detect_file_type;
use crate::paths::{normalize_path, to_display_path};
use crate::types::{
    FileInfo, FileScanDiagnostics, RootPath, SkippedFile, SkippedFileReason, UnixMillis,
};
use crate::utils::hash::sha256_text;

use super::types::{BINARY_CONTROL_CHAR_RATIO, BINARY_SNIFF_BYTES, MAX_SKIPPED_FILE_SAMPLES};
use super::types::{display_relative, file_name_of};

pub(crate) fn read_file_info(
    workspace_index_id: &str,
    root: &RootPath,
    absolute_path: &str,
    diagnostics: &mut FileScanDiagnostics,
    known_files: &HashMap<String, &FileInfo>,
) -> EngineResult<Option<FileInfo>> {
    let Ok(info) = std::fs::metadata(absolute_path) else {
        return Ok(None);
    };
    if !info.is_file() {
        return Ok(None);
    }
    let relative_path = {
        let display = display_relative(&root.absolute_path, absolute_path);
        if display.is_empty() {
            file_name_of(absolute_path)
        } else {
            display
        }
    };
    if info.len() == 0 {
        record_skipped_file(
            diagnostics,
            absolute_path,
            &relative_path,
            SkippedFileReason::Empty,
            Some(0),
            None,
        );
        return Ok(None);
    }
    let Some(detected) = detect_file_type(Path::new(absolute_path)) else {
        record_skipped_file(
            diagnostics,
            absolute_path,
            &relative_path,
            SkippedFileReason::Unsupported,
            Some(info.len()),
            None,
        );
        return Ok(None);
    };
    let max_file_size = resolve_max_file_size_bytes(detected.kind, root.max_file_size_bytes);
    if info.len() > max_file_size {
        record_skipped_file(
            diagnostics,
            absolute_path,
            &relative_path,
            SkippedFileReason::TooLarge,
            Some(info.len()),
            Some(max_file_size),
        );
        return Ok(None);
    }
    let last_modified_time = info
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0);
    let known = known_files.get(&to_display_path(&normalize_path(Path::new(absolute_path))));
    if known.is_some_and(|known| {
        known.content_hash.is_some()
            && known.size_bytes == info.len()
            && known.last_modified_time.as_millis() == last_modified_time
    }) {
        return Ok(Some(FileInfo {
            id: make_file_id(workspace_index_id, absolute_path),
            absolute_path: absolute_path.to_owned(),
            relative_path,
            root_path: root.absolute_path.clone(),
            size_bytes: info.len(),
            last_modified_time: UnixMillis::from_millis(last_modified_time),
            content_hash: None,
            kind: detected.kind,
            format: detected.format,
            index_status: None,
        }));
    }
    if detected.kind != crate::types::FileKind::Image && is_likely_binary_file(absolute_path) {
        record_skipped_file(
            diagnostics,
            absolute_path,
            &relative_path,
            SkippedFileReason::Binary,
            Some(info.len()),
            None,
        );
        return Ok(None);
    }
    Ok(Some(FileInfo {
        id: make_file_id(workspace_index_id, absolute_path),
        absolute_path: absolute_path.to_owned(),
        relative_path,
        root_path: root.absolute_path.clone(),
        size_bytes: info.len(),
        last_modified_time: UnixMillis::from_millis(last_modified_time),
        content_hash: None,
        kind: detected.kind,
        format: detected.format,
        index_status: None,
    }))
}

fn record_skipped_file(
    diagnostics: &mut FileScanDiagnostics,
    absolute_path: &str,
    relative_path: &str,
    reason: SkippedFileReason,
    size_bytes: Option<u64>,
    limit_bytes: Option<u64>,
) {
    diagnostics.skipped_files += 1;
    *diagnostics.skipped_by_reason.entry(reason).or_insert(0) += 1;
    if diagnostics.skipped_samples.len() < MAX_SKIPPED_FILE_SAMPLES {
        diagnostics.skipped_samples.push(SkippedFile {
            absolute_path: absolute_path.to_owned(),
            relative_path: relative_path.to_owned(),
            reason,
            size_bytes,
            limit_bytes,
        });
    }
}

/// Empty diagnostics accumulator (mirrors `createScanDiagnostics`).
#[must_use]
pub fn create_scan_diagnostics() -> FileScanDiagnostics {
    FileScanDiagnostics {
        skipped_files: 0,
        skipped_by_reason: Default::default(),
        skipped_samples: Vec::new(),
    }
}

fn is_likely_binary_file(path: &str) -> bool {
    use std::io::Read as _;
    let Ok(mut handle) = std::fs::File::open(path) else {
        return false;
    };
    let mut buffer = vec![0u8; BINARY_SNIFF_BYTES];
    let Ok(bytes_read) = handle.read(&mut buffer) else {
        return false;
    };
    if bytes_read == 0 {
        return false;
    }
    let mut suspicious = 0usize;
    for value in buffer.get(..bytes_read).into_iter().flatten() {
        if *value == 0 {
            return true;
        }
        if is_suspicious_control_byte(*value) {
            suspicious += 1;
        }
    }
    suspicious as f64 / bytes_read as f64 > BINARY_CONTROL_CHAR_RATIO
}

fn is_suspicious_control_byte(value: u8) -> bool {
    value < 32 && !matches!(value, 7 | 8 | 9 | 10 | 12 | 13 | 27)
}

fn make_file_id(workspace_index_id: &str, absolute_path: &str) -> crate::ids::FileId {
    crate::ids::FileId::from_raw(sha256_text(&format!(
        "{workspace_index_id}\0{}",
        to_display_path(&normalize_path(Path::new(absolute_path)))
    )))
}

#[cfg(test)]
mod tests {
    use super::super::types::ScanOptions;
    use super::super::walk::scan_root_paths;
    use crate::paths::to_display_path;
    use crate::types::{RootPath, SkippedFileReason};
    use std::io::Write as _;
    use std::path::Path;

    fn write_tree(dir: &Path, files: &[(&str, &str)]) {
        for (name, content) in files {
            let path = dir.join(name);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("mkdir");
            }
            let mut file = std::fs::File::create(&path).expect("create");
            file.write_all(content.as_bytes()).expect("write");
        }
    }

    fn test_root(dir: &Path) -> RootPath {
        RootPath {
            absolute_path: to_display_path(dir),
            recursive: true,
            ..RootPath::default()
        }
    }

    #[test]
    fn empty_files_are_skipped_with_reason() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_tree(dir.path(), &[("empty.rs", ""), ("ok.rs", "fn f() {}\n")]);
        let options = ScanOptions::default();
        let result = scan_root_paths("idx", &[test_root(dir.path())], &options).expect("scan");
        assert_eq!(result.files.len(), 1);
        assert_eq!(result.diagnostics.skipped_files, 1);
        assert_eq!(
            result
                .diagnostics
                .skipped_by_reason
                .get(&SkippedFileReason::Empty),
            Some(&1)
        );
    }
}
