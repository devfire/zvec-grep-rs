//! Size/SHA-256 verification and crash-safe publication for model artifacts.
//!
//! A cached artifact is only usable when it is a non-empty regular file
//! matching every expectation that is known for it. Expectations may be
//! partially unknown (`None`): no hash or size is ever invented here, but
//! whatever metadata a catalog entry does carry is always enforced, both
//! for a fresh download (verified on the temp file before rename) and for
//! a pre-existing cache entry (invalid entries are removed so the next
//! caller re-downloads instead of trusting garbage forever).

use std::fs;
use std::io::Read;
use std::path::Path;

use sha2::{Digest, Sha256};

use super::error::ModelError;
use crate::error::{EngineError, EngineResult};
use crate::utils::hash::to_hex;

/// Expected size and/or SHA-256 for one downloadable artifact.
///
/// `None` means "not known in-repo"; it is never treated as a match for a
/// fabricated value. Catalog entries carry `ArtifactExpectation<'static>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ArtifactExpectation<'a> {
    /// Exact byte length, when known.
    pub size: Option<u64>,
    /// Lowercase hex SHA-256, when known.
    pub sha256: Option<&'a str>,
}

impl<'a> ArtifactExpectation<'a> {
    /// No size or hash is known for this artifact.
    pub const UNVERIFIED: Self = Self {
        size: None,
        sha256: None,
    };

    /// Builds an expectation from optional size / SHA-256 parts.
    #[must_use]
    pub const fn new(size: Option<u64>, sha256: Option<&'a str>) -> Self {
        Self { size, sha256 }
    }

    /// True when at least one checkable expectation is present.
    #[must_use]
    pub const fn has_checks(&self) -> bool {
        self.size.is_some() || self.sha256.is_some()
    }
}

fn failed(label: &str, detail: String) -> EngineError {
    EngineError::from(ModelError::DownloadFailed {
        context: format!("artifact={label} detail={detail}"),
    })
}

/// Streams `path` and returns its lowercase hex SHA-256.
fn sha256_file(path: &Path, label: &str) -> EngineResult<String> {
    let mut file = fs::File::open(path)
        .map_err(|err| failed(label, format!("open for sha256: {err}")))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|err| failed(label, format!("read for sha256: {err}")))?;
        if read == 0 {
            break;
        }
        let Some(chunk) = buffer.get(..read) else {
            return Err(failed(label, "read for sha256: short buffer".to_owned()));
        };
        hasher.update(chunk);
    }
    Ok(to_hex(hasher.finalize().as_ref()))
}

/// Verifies `path` against `expectation`: it must be a non-empty regular
/// file, match the expected size when one is known, and match the expected
/// SHA-256 when one is known.
///
/// # Errors
///
/// Returns [`ModelError::DownloadFailed`] when the file is missing, empty,
/// not a regular file, unreadable, or mismatches a known expectation
/// (including a malformed expected hash).
pub fn verify_artifact_file(
    path: &Path,
    expectation: ArtifactExpectation<'_>,
    label: &str,
) -> EngineResult<()> {
    let metadata = fs::metadata(path)
        .map_err(|err| failed(label, format!("stat cached artifact: {err}")))?;
    if !metadata.is_file() {
        return Err(failed(label, "cached artifact is not a file".to_owned()));
    }
    if metadata.len() == 0 {
        return Err(failed(label, "cached artifact is empty".to_owned()));
    }
    if let Some(expected_size) = expectation.size
        && metadata.len() != expected_size
    {
        return Err(failed(
            label,
            format!(
                "size mismatch expected={expected_size} actual={}",
                metadata.len()
            ),
        ));
    }
    if let Some(expected_sha256) = expectation.sha256 {
        let normalized = expected_sha256.trim().to_ascii_lowercase();
        if normalized.len() != 64
            || !normalized.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(failed(
                label,
                "expected sha256 is not 64 hex characters".to_owned(),
            ));
        }
        let actual = sha256_file(path, label)?;
        if actual != normalized {
            return Err(failed(
                label,
                format!("sha256 mismatch expected={normalized} actual={actual}"),
            ));
        }
    }
    Ok(())
}

/// Revalidates a cache entry. Returns `Ok(true)` when a valid file is kept,
/// `Ok(false)` when no file exists or an invalid file was removed (so the
/// caller should download), and an error only when removal itself fails or
/// the entry cannot be inspected for a reason other than absence.
///
/// # Errors
///
/// Returns [`ModelError::DownloadFailed`] when an invalid entry exists but
/// cannot be removed, so garbage is never silently kept by a failed cleanup.
pub fn discard_invalid_cached_file(
    path: &Path,
    expectation: ArtifactExpectation<'_>,
    label: &str,
) -> EngineResult<bool> {
    match fs::metadata(path) {
        Ok(_) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(failed(label, format!("stat cached artifact: {err}"))),
    }
    if verify_artifact_file(path, expectation, label).is_ok() {
        return Ok(true);
    }
    match fs::remove_file(path) {
        Ok(()) => Ok(false),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(failed(
            label,
            format!("remove invalid cached artifact: {err}"),
        )),
    }
}

/// Fsyncs the parent directory of `path` so a rename into it survives a
/// crash, mirroring `utils::json_io::sync_parent_dir`.
///
/// # Errors
///
/// Returns [`ModelError::DownloadFailed`] when the directory cannot be
/// opened or synced.
pub fn sync_parent_dir(path: &Path, label: &str) -> EngineResult<()> {
    let dir: &Path = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let handle = fs::File::open(dir)
        .map_err(|err| failed(label, format!("open parent dir for fsync: {err}")))?;
    handle
        .sync_all()
        .map_err(|err| failed(label, format!("fsync parent dir: {err}")))
}

/// Verifies a completed temp download, fsyncs it, atomically renames it to
/// `destination`, and fsyncs the parent directory. On any failure the temp
/// file is removed and nothing is published at `destination` by this call.
///
/// # Errors
///
/// Returns [`ModelError::DownloadFailed`] when verification, fsync, or the
/// rename fails.
pub fn publish_verified_temp(
    temp_path: &Path,
    destination: &Path,
    expectation: ArtifactExpectation<'_>,
    label: &str,
) -> EngineResult<()> {
    let result = publish_inner(temp_path, destination, expectation, label);
    if result.is_err() {
        // Cleanup mirrors download.rs: a leftover temp file must not be
        // mistaken for a cache entry; failure here cannot improve the
        // already-reported verification/rename error.
        let _ = fs::remove_file(temp_path);
    }
    result
}

fn publish_inner(
    temp_path: &Path,
    destination: &Path,
    expectation: ArtifactExpectation<'_>,
    label: &str,
) -> EngineResult<()> {
    verify_artifact_file(temp_path, expectation, label)?;
    if fs::symlink_metadata(temp_path).is_ok_and(|meta| meta.file_type().is_symlink()) {
        return Err(failed(label, "temp file was replaced by a symlink".to_owned()));
    }
    let file = fs::File::open(temp_path)
        .map_err(|err| failed(label, format!("open temp file for fsync: {err}")))?;
    file.sync_all()
        .map_err(|err| failed(label, format!("fsync temp file: {err}")))?;
    drop(file);
    fs::rename(temp_path, destination)
        .map_err(|err| failed(label, format!("rename into cache: {err}")))?;
    sync_parent_dir(destination, label)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::hash::sha256_bytes;

    fn expectation_for(bytes: &[u8]) -> (u64, String) {
        let size = u64::try_from(bytes.len()).unwrap_or(0);
        (size, sha256_bytes(bytes))
    }

    #[test]
    fn valid_file_with_computed_hash_is_accepted_and_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("model.bin");
        let bytes: &[u8] = b"GGUF-fixture-bytes-for-verification";
        fs::write(&path, bytes).unwrap();
        let (size, hash) = expectation_for(bytes);
        let expectation = ArtifactExpectation::new(Some(size), Some(hash.as_str()));
        verify_artifact_file(&path, expectation, "model.bin").unwrap();
        assert!(discard_invalid_cached_file(&path, expectation, "model.bin").unwrap());
        assert!(path.exists());
    }

    #[test]
    fn corrupt_and_truncated_cache_entries_are_rejected_and_removed() {
        let dir = tempfile::tempdir().unwrap();
        let original: &[u8] = b"GGUF-fixture-bytes-for-verification";
        let (size, hash) = expectation_for(original);
        let expectation = ArtifactExpectation::new(Some(size), Some(hash.as_str()));

        // Same length, different bytes: sha256 must reject it.
        let corrupt = dir.path().join("corrupt.bin");
        fs::write(&corrupt, b"XXXX-fixture-bytes-for-verification").unwrap();
        assert!(verify_artifact_file(&corrupt, expectation, "corrupt.bin").is_err());
        assert!(!discard_invalid_cached_file(&corrupt, expectation, "corrupt.bin").unwrap());
        assert!(!corrupt.exists());

        // Truncated: size (and hash) must reject it.
        let truncated = dir.path().join("truncated.bin");
        fs::write(&truncated, b"GGUF-fix").unwrap();
        assert!(!discard_invalid_cached_file(&truncated, expectation, "truncated.bin").unwrap());
        assert!(!truncated.exists());

        // Empty garbage is rejected even with no known size/hash.
        let empty = dir.path().join("empty.bin");
        fs::write(&empty, b"").unwrap();
        assert!(
            !discard_invalid_cached_file(&empty, ArtifactExpectation::UNVERIFIED, "empty.bin")
                .unwrap()
        );
        assert!(!empty.exists());
    }

    #[test]
    fn failed_verification_leaves_no_file_at_final_path() {
        let dir = tempfile::tempdir().unwrap();
        let temp_path = dir.path().join(".model.bin.part-test.tmp");
        let destination = dir.path().join("model.bin");
        let original: &[u8] = b"expected-bytes";
        let (_size, hash) = expectation_for(original);
        // Temp holds different bytes than the expected hash.
        fs::write(&temp_path, b"tampered-bytes!").unwrap();
        let expectation = ArtifactExpectation::new(None, Some(hash.as_str()));
        publish_verified_temp(&temp_path, &destination, expectation, "model.bin").unwrap_err();
        assert!(!destination.exists());
        assert!(!temp_path.exists());
    }

    #[test]
    fn publish_moves_valid_temp_to_destination() {
        let dir = tempfile::tempdir().unwrap();
        let temp_path = dir.path().join(".model.bin.part-test.tmp");
        let destination = dir.path().join("model.bin");
        let bytes: &[u8] = b"published-bytes";
        fs::write(&temp_path, bytes).unwrap();
        let (size, hash) = expectation_for(bytes);
        let expectation = ArtifactExpectation::new(Some(size), Some(hash.as_str()));
        publish_verified_temp(&temp_path, &destination, expectation, "model.bin").unwrap();
        assert_eq!(fs::read(&destination).unwrap(), bytes);
        assert!(!temp_path.exists());
    }
}
