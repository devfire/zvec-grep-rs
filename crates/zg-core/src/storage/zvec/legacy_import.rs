//! One-way import of TypeScript-generation file metadata.
//!
//! The TS engine keeps file metadata in a second zvec collection
//! (`files.zvec`, schema `zvec_grep_files`); this port keeps it in
//! `files.json` (see [`super::store`]). When a storage directory holds a
//! legacy collection but no `files.json`, opening imports every document
//! into [`FileRecord`]s instead of forcing a full reindex.
//!
//! Compatibility stance (Phase E, option 1 — recorded in
//! `docs/ts-divergence.md`): manifests written by this port stamp
//! [`CURRENT_INDEX_VERSION`](crate::types::CURRENT_INDEX_VERSION) (`2`),
//! which the TS generation rejects with its own
//! `WORKSPACE_INDEX.VERSION_MISMATCH`, so a Rust-written index is
//! unmistakable to TS and never silently diverged. A verified import
//! deletes `files.zvec` afterwards (explicit one-way migration); an
//! unreadable collection deletes nothing and falls back to reindexing.
//!
//! TS field contract (from `engine/storage/zvec.ts` `createFilesSchema` /
//! `fileRecordToDoc` / `docToFileRecord`):
//!
//! | zvec field | type | Rust target |
//! |---|---|---|
//! | `file_id` | indexed string | `FileInfo.id` (equals the doc pk) |
//! | `absolute_path` | indexed string | `FileInfo.absolute_path` (normalized) |
//! | `relative_path` | string | `FileInfo.relative_path` |
//! | `root_path` | string | `FileInfo.root_path` |
//! | `size_bytes` | int64 | `FileInfo.size_bytes` |
//! | `last_modified_time` | int64 | `FileInfo.last_modified_time` |
//! | `content_hash` | indexed string, nullable | `FileInfo.content_hash` |
//! | `kind` | indexed string | `FileInfo.kind` (`text`/`code`/`data`/`image`) |
//! | `format` | indexed string | `FileInfo.format` |
//! | `has_index_status` | bool | presence of `FileInfo.index_status` |
//! | `indexed_time` | int64, nullable | `FileIndexStatus.indexed_time` |
//! | `entity_count` | int32 | `FileIndexStatus.entity_count` |
//! | `token_count` | int32, nullable | `FileIndexStatus.token_count` |
//! | `truncated_fragment_count` | int32, nullable | `FileIndexStatus.truncated_fragment_count` |
//! | `error` | string, nullable | `FileIndexStatus.error` |
//! | `entity_ids_json` | string (JSON array) | `FileRecord.entity_ids` (lenient, like TS `parseStringArray`) |

use std::path::Path;
use std::time::SystemTime;

use zvec_rust::{Collection, CollectionOptions, Doc};

use super::store::FileRecord;
use crate::error::{EngineError, EngineErrorCode, EngineResult};
use crate::ids::FileId;
use crate::storage::layout::normalize_absolute_path;
use crate::types::{FileFormat, FileIndexStatus, FileInfo, FileKind, UnixMillis};

/// Result of probing the legacy `files.zvec` collection.
#[derive(Debug)]
pub(crate) enum LegacyImportOutcome {
    /// No legacy collection on disk; nothing to do.
    Absent,
    /// Documents decoded. `verified` is true only when every document in
    /// the collection decoded and the count matches the collection stats —
    /// the sole condition under which the caller may delete `files.zvec`.
    /// A partial import is still usable in memory; the caller must persist
    /// nothing so the next open retries the import.
    Imported {
        records: Vec<FileRecord>,
        verified: bool,
    },
    /// The collection cannot be opened or yields no decodable documents.
    /// The caller must delete nothing and fall back to a full reindex.
    Unreadable {
        reason: String,
    },
}

/// Reads every file-metadata document from a TS-written `files.zvec`.
///
/// Never errors: every failure mode maps to [`LegacyImportOutcome`], so
/// callers cannot turn a foreign index into a hard open failure.
pub(crate) fn try_import_legacy_files(files_zvec: &Path) -> LegacyImportOutcome {
    if !files_zvec.exists() {
        return LegacyImportOutcome::Absent;
    }
    if let Err(error) = super::initialize_zvec() {
        return LegacyImportOutcome::Unreadable { reason: error.to_string() };
    }
    let path = files_zvec.to_string_lossy().into_owned();
    let collection = match open_legacy_collection(&path) {
        Ok(collection) => collection,
        Err(reason) => return LegacyImportOutcome::Unreadable { reason },
    };
    let doc_count = match collection.stats() {
        Ok(stats) => stats.doc_count,
        Err(error) => {
            return LegacyImportOutcome::Unreadable {
                reason: format!("path={path} error={error}"),
            };
        }
    };
    let mut records = Vec::new();
    let mut failures: u64 = 0;
    let iterator = match collection.iter_with_options(None, false) {
        Ok(iterator) => iterator,
        Err(error) => {
            return LegacyImportOutcome::Unreadable {
                reason: format!("path={path} error={error}"),
            };
        }
    };
    for item in iterator {
        let doc = match item {
            Ok(doc) => doc,
            Err(error) => {
                return LegacyImportOutcome::Unreadable {
                    reason: format!("path={path} error={error}"),
                };
            }
        };
        match doc_to_file_record(&doc) {
            Ok(record) => records.push(record),
            Err(error) => {
                failures += 1;
                tracing::warn!(
                    path = %path,
                    pk = doc.get_pk().unwrap_or_default(),
                    error = %error,
                    "skipping undecodable legacy file-metadata document"
                );
            }
        }
    }
    if doc_count > 0 && records.is_empty() {
        return LegacyImportOutcome::Unreadable {
            reason: format!("path={path} docCount={doc_count} decoded=0 failures={failures}"),
        };
    }
    let decoded = match u64::try_from(records.len()) {
        Ok(decoded) => decoded,
        Err(_) => {
            return LegacyImportOutcome::Unreadable {
                reason: format!("path={path} docCount={doc_count} record count overflow"),
            };
        }
    };
    LegacyImportOutcome::Imported {
        verified: failures == 0 && decoded == doc_count,
        records,
    }
}

/// True when the legacy collection is strictly newer than the JSON store.
///
/// Drives the both-present rule: a TS reindex after a Rust run leaves a
/// newer `files.zvec` behind, and the next open re-imports rather than
/// serving stale `files.json`. Comparison uses the newest mtime under
/// `files.zvec` (a directory mtime alone misses content writes); any
/// filesystem error resolves to `false` (prefer the JSON store).
pub(crate) fn legacy_files_newer_than(files_zvec: &Path, meta_file: &Path) -> bool {
    let meta_mtime = filesystem_mtime(meta_file);
    let legacy_mtime = newest_mtime_under(files_zvec);
    match (legacy_mtime, meta_mtime) {
        (Some(legacy), Some(meta)) => legacy > meta,
        _ => false,
    }
}

/// Decodes one TS `zvec_grep_files` document into a [`FileRecord`].
///
/// Mirrors `docToFileRecord` reader-for-reader. The TS readers are total —
/// they never throw: missing strings decode to `""`, missing numbers to
/// `0`, missing booleans to `false`, and every nullable number `<= 0`
/// decodes to `null` (`readNullableNumberFieldFromFields` maps
/// non-positive values to `null`, so a stored `0` reads back as absent).
/// This decoder is total in exactly the same way; it fails only where the
/// Rust domain types cannot represent the value at all (an unknown `kind`
/// has no `FileKind` variant; a negative `size_bytes` has no `u64`).
fn doc_to_file_record(doc: &Doc) -> EngineResult<FileRecord> {
    let pk = doc.get_pk().unwrap_or_default().to_owned();
    let id = required_string(doc, &pk, "file_id")?;
    let absolute_path = normalize_absolute_path(&required_string(doc, &pk, "absolute_path")?);
    let relative_path = required_string(doc, &pk, "relative_path")?;
    let root_path = required_string(doc, &pk, "root_path")?;
    let size_bytes: u64 = required_i64(doc, &pk, "size_bytes")?
        .try_into()
        .map_err(|_| legacy_doc_error(&pk, "size_bytes is negative"))?;
    let last_modified_time = UnixMillis::from_millis(required_i64(doc, &pk, "last_modified_time")?);
    let content_hash = optional_string(doc, &pk, "content_hash")?;
    let kind = match required_string(doc, &pk, "kind")?.as_str() {
        "text" => FileKind::Text,
        "code" => FileKind::Code,
        "data" => FileKind::Data,
        "image" => FileKind::Image,
        other => {
            return Err(legacy_doc_error(&pk, &format!("unsupported kind {other:?}")));
        }
    };
    let format = FileFormat::parse(required_string(doc, &pk, "format")?);
    let index_status = if required_bool(doc, &pk, "has_index_status")? {
        Some(FileIndexStatus {
            indexed_time: optional_i64(doc, &pk, "indexed_time")?.map(UnixMillis::from_millis),
            entity_count: required_i32(doc, &pk, "entity_count")?
                .try_into()
                .map_err(|_| legacy_doc_error(&pk, "entity_count is negative"))?,
            token_count: optional_i32(doc, &pk, "token_count")?
                .map(|value| {
                    u64::try_from(value)
                        .map_err(|_| legacy_doc_error(&pk, "token_count is negative"))
                })
                .transpose()?,
            truncated_fragment_count: optional_i32(doc, &pk, "truncated_fragment_count")?
                .map(|value| {
                    usize::try_from(value)
                        .map_err(|_| legacy_doc_error(&pk, "truncated_fragment_count is negative"))
                })
                .transpose()?,
            error: optional_string(doc, &pk, "error")?,
        })
    } else {
        None
    };
    let entity_ids_json = optional_string(doc, &pk, "entity_ids_json")?.unwrap_or_default();
    Ok(FileRecord {
        info: FileInfo {
            id: FileId::from_raw(id),
            absolute_path,
            relative_path,
            root_path,
            size_bytes,
            last_modified_time,
            content_hash,
            kind,
            format,
            index_status,
        },
        entity_ids: parse_string_array(&entity_ids_json),
    })
}

/// Mirrors TS `parseStringArray`: malformed JSON and non-arrays yield `[]`,
/// non-string items are dropped.
fn parse_string_array(value: &str) -> Vec<String> {
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(value) else {
        return Vec::new();
    };
    let Some(items) = parsed.as_array() else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| item.as_str().map(str::to_owned))
        .collect()
}

fn open_legacy_collection(path: &str) -> Result<Collection, String> {
    let mut options = CollectionOptions::new().map_err(|error| error.to_string())?;
    options
        .set_read_only(true)
        .map_err(|error| error.to_string())?;
    Collection::open(path, Some(&options)).map_err(|error| {
        format!("failed to open legacy file-metadata collection path={path} error={error}")
    })
}

fn filesystem_mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|meta| meta.modified()).ok()
}

fn newest_mtime_under(dir: &Path) -> Option<SystemTime> {
    let meta = std::fs::metadata(dir).ok()?;
    let mut newest = meta.modified().ok()?;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if let Ok(mtime) = meta.modified() {
                if mtime > newest {
                    newest = mtime;
                }
            }
            if meta.is_dir() {
                stack.push(entry.path());
            }
        }
    }
    Some(newest)
}

/// TS `readStringField`: absent and null decode to `""`; only a genuine
/// field-type error fails.
fn required_string(doc: &Doc, pk: &str, field: &str) -> EngineResult<String> {
    Ok(optional_string(doc, pk, field)?.unwrap_or_default())
}

/// TS `readNullableStringFieldFromFields`: absent, null, and `""` decode to
/// `None`.
fn optional_string(doc: &Doc, pk: &str, field: &str) -> EngineResult<Option<String>> {
    if !doc.has_field(field) || doc.is_field_null(field) {
        return Ok(None);
    }
    let value = doc
        .get_string(field)
        .map_err(|error| legacy_field_error(pk, field, &error.to_string()))?;
    Ok(value.filter(|value| !value.is_empty()))
}

/// TS `readNumberFieldFromFields`: absent and null decode to `0`; only a
/// genuine field-type error fails.
fn required_i64(doc: &Doc, pk: &str, field: &str) -> EngineResult<i64> {
    if !doc.has_field(field) || doc.is_field_null(field) {
        return Ok(0);
    }
    let value = doc
        .get_i64(field)
        .map_err(|error| legacy_field_error(pk, field, &error.to_string()))?;
    Ok(value.unwrap_or(0))
}

/// TS `readNullableNumberFieldFromFields`: absent, null, and every
/// non-positive value decode to `None`.
fn optional_i64(doc: &Doc, pk: &str, field: &str) -> EngineResult<Option<i64>> {
    if !doc.has_field(field) || doc.is_field_null(field) {
        return Ok(None);
    }
    let value = doc
        .get_i64(field)
        .map_err(|error| legacy_field_error(pk, field, &error.to_string()))?;
    Ok(value.filter(|value| *value > 0))
}

/// TS `readNumberFieldFromFields` for 32-bit fields: absent and null decode
/// to `0`.
fn required_i32(doc: &Doc, pk: &str, field: &str) -> EngineResult<i32> {
    if !doc.has_field(field) || doc.is_field_null(field) {
        return Ok(0);
    }
    let value = doc
        .get_i32(field)
        .map_err(|error| legacy_field_error(pk, field, &error.to_string()))?;
    Ok(value.unwrap_or(0))
}

/// TS `readNullableNumberFieldFromFields` for 32-bit fields: absent, null,
/// and every non-positive value decode to `None`.
fn optional_i32(doc: &Doc, pk: &str, field: &str) -> EngineResult<Option<i32>> {
    if !doc.has_field(field) || doc.is_field_null(field) {
        return Ok(None);
    }
    let value = doc
        .get_i32(field)
        .map_err(|error| legacy_field_error(pk, field, &error.to_string()))?;
    Ok(value.filter(|value| *value > 0))
}

/// TS `readBooleanFieldFromFields`: absent and null decode to `false`.
fn required_bool(doc: &Doc, pk: &str, field: &str) -> EngineResult<bool> {
    if !doc.has_field(field) || doc.is_field_null(field) {
        return Ok(false);
    }
    let value = doc
        .get_bool(field)
        .map_err(|error| legacy_field_error(pk, field, &error.to_string()))?;
    Ok(value.unwrap_or(false))
}

fn legacy_doc_error(pk: &str, detail: &str) -> EngineError {
    EngineError::new(
        EngineErrorCode::from_static("STORAGE.LEGACY_IMPORT_DECODE_FAILED"),
        "legacy file-metadata document is undecodable",
    )
    .with_context(format!("fileId={pk} error={detail}"))
}

fn legacy_field_error(pk: &str, field: &str, detail: &str) -> EngineError {
    EngineError::new(
        EngineErrorCode::from_static("STORAGE.LEGACY_IMPORT_DECODE_FAILED"),
        "legacy file-metadata field read failed",
    )
    .with_context(format!("fileId={pk} field={field} error={detail}"))
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use super::*;

    fn file_doc() -> Doc {
        let mut doc = Doc::new().expect("doc");
        doc.set_pk("abc123");
        doc.add_string("file_id", "abc123").expect("field");
        doc.add_string("absolute_path", "/repo/a.txt").expect("field");
        doc.add_string("relative_path", "a.txt").expect("field");
        doc.add_string("root_path", "/repo").expect("field");
        doc.add_i64("size_bytes", 12).expect("field");
        doc.add_i64("last_modified_time", 1700000000000)
            .expect("field");
        doc.add_string("kind", "text").expect("field");
        doc.add_string("format", "text").expect("field");
        doc.add_bool("has_index_status", true).expect("field");
        doc.add_i32("entity_count", 2).expect("field");
        doc.add_string("entity_ids_json", "[\"e1\", 7, \"e2\"]")
            .expect("field");
        doc
    }

    #[test]
    fn decodes_full_legacy_document() {
        let mut doc = file_doc();
        doc.add_string("content_hash", "deadbeef").expect("field");
        doc.add_i64("indexed_time", 1700000001000).expect("field");
        doc.add_i32("token_count", 40).expect("field");
        doc.add_i32("truncated_fragment_count", 1).expect("field");
        doc.add_string("error", "boom").expect("field");
        let record = doc_to_file_record(&doc).expect("decode");
        assert_eq!(record.info.id.as_str(), "abc123");
        assert_eq!(record.info.kind, FileKind::Text);
        assert_eq!(record.info.content_hash.as_deref(), Some("deadbeef"));
        let status = record.info.index_status.expect("status");
        assert_eq!(status.indexed_time, Some(UnixMillis::from_millis(1700000001000)));
        assert_eq!(status.entity_count, 2);
        assert_eq!(status.token_count, Some(40));
        assert_eq!(status.truncated_fragment_count, Some(1));
        assert_eq!(status.error.as_deref(), Some("boom"));
        // TS `parseStringArray` drops the non-string item.
        assert_eq!(record.entity_ids, vec!["e1".to_owned(), "e2".to_owned()]);
    }

    #[test]
    fn absent_nullable_fields_become_none() {
        let record = doc_to_file_record(&file_doc()).expect("decode");
        assert_eq!(record.info.content_hash, None);
        let status = record.info.index_status.expect("status");
        assert_eq!(status.indexed_time, None);
        assert_eq!(status.token_count, None);
        assert_eq!(status.error, None);
    }

    #[test]
    fn missing_status_flag_means_unindexed() {
        let mut doc = file_doc();
        doc.add_bool("has_index_status", false).expect("field");
        let record = doc_to_file_record(&doc).expect("decode");
        assert_eq!(record.info.index_status, None);
    }

    #[test]
    fn zero_nullable_numbers_decode_as_none_like_ts() {
        // TS `readNullableNumberFieldFromFields` maps every non-positive
        // value to `null`, so a stored `0` reads back as absent.
        let mut doc = file_doc();
        doc.add_i64("indexed_time", 0).expect("field");
        doc.add_i32("token_count", 0).expect("field");
        doc.add_i32("truncated_fragment_count", 0).expect("field");
        doc.add_string("content_hash", "").expect("field");
        let record = doc_to_file_record(&doc).expect("decode");
        let status = record.info.index_status.expect("status");
        assert_eq!(status.indexed_time, None);
        assert_eq!(status.token_count, None);
        assert_eq!(status.truncated_fragment_count, None);
        assert_eq!(record.info.content_hash, None);
    }

    #[test]
    fn absent_fields_fall_back_to_ts_defaults() {
        // TS readers are total: a document carrying only its identity
        // decodes with `""` / `0` / `false` / `[]` defaults.
        let mut doc = Doc::new().expect("doc");
        doc.set_pk("bare");
        doc.add_string("file_id", "bare").expect("field");
        doc.add_string("absolute_path", "/repo/bare.txt").expect("field");
        doc.add_string("kind", "data").expect("field");
        let record = doc_to_file_record(&doc).expect("decode");
        assert_eq!(record.info.absolute_path, "/repo/bare.txt");
        assert_eq!(record.info.size_bytes, 0);
        assert_eq!(record.info.index_status, None);
        assert!(record.entity_ids.is_empty());
    }

    #[test]
    fn rejects_unknown_kind_and_negative_sizes() {
        let mut bad_kind = file_doc();
        bad_kind.add_string("kind", "pdf").expect("field");
        // `add_string` overwrites: decode must reject the unknown kind.
        assert!(doc_to_file_record(&bad_kind).is_err());
        let mut negative = file_doc();
        negative.add_i64("size_bytes", -3).expect("field");
        assert!(doc_to_file_record(&negative).is_err());
    }

    #[test]
    fn string_array_parsing_matches_ts_leniency() {
        assert!(parse_string_array("nope").is_empty());
        assert!(parse_string_array("{\"a\":1}").is_empty());
        assert_eq!(
            parse_string_array("[\"a\",null,1,\"b\"]"),
            vec!["a".to_owned(), "b".to_owned()]
        );
    }

    #[test]
    fn freshness_prefers_newer_legacy_collection() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let legacy = dir.path().join("files.zvec");
        std::fs::create_dir(&legacy).expect("mkdir");
        let meta = dir.path().join("files.json");
        std::fs::write(&meta, "{}").expect("write");
        // Pin the JSON store into the past so the just-created legacy dir
        // is strictly newer without any timing assumption.
        let old = SystemTime::UNIX_EPOCH + Duration::from_secs(1000);
        std::fs::File::options()
            .write(true)
            .open(&meta)
            .expect("open")
            .set_modified(old)
            .expect("mtime");
        assert!(legacy_files_newer_than(&legacy, &meta));
        // Missing inputs never trigger a re-import.
        assert!(!legacy_files_newer_than(&dir.path().join("absent"), &meta));
        assert!(!legacy_files_newer_than(&legacy, &dir.path().join("absent")));
    }
}
