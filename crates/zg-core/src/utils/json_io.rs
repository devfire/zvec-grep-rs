//! Atomic JSON persistence: tmp-file + rename writes, ENOENT-tolerant reads.

use std::fs;
use std::io::Write;
use std::path::Path;

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::error::EngineResult;

/// File modes for atomic writes (unix).
#[derive(Debug, Clone, Copy)]
pub struct WriteModes {
    pub directory_mode: Option<u32>,
    pub file_mode: Option<u32>,
}

/// Modes used for global config and manifests: 0700 dirs, 0600 files.
pub const SECURE_MODES: WriteModes = WriteModes {
    directory_mode: Some(0o700),
    file_mode: Some(0o600),
};

/// No explicit modes; inherit umask.
pub const DEFAULT_MODES: WriteModes = WriteModes {
    directory_mode: None,
    file_mode: None,
};

/// Reads and parses a JSON file; returns `fallback` when it does not exist.
/// Parse and IO errors propagate.
///
/// # Errors
///
/// Returns [`EngineError`](crate::error::EngineError) with `JSON.READ_FAILED` when the file cannot be read or fails to parse.
pub fn read_json_file<T: DeserializeOwned>(path: &Path, fallback: T) -> EngineResult<T> {
    match fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).map_err(|error| {
            crate::error::EngineError::new(
                crate::error::EngineErrorCode::JsonReadFailed,
                format!("failed to parse {}", path.display()),
            )
            .with_context(format!("error={error}"))
            .with_source(error)
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(fallback),
        Err(error) => Err(crate::error::EngineError::new(
            crate::error::EngineErrorCode::JsonReadFailed,
            format!("failed to read {}", path.display()),
        )
        .with_context(format!("error={error}"))
        .with_source(error)),
    }
}

/// Writes `value` as pretty JSON atomically: write `<path>.<pid>.<uuid>.tmp`
/// (optionally chmod), then rename over the target. Parent dirs are created.
///
/// # Errors
///
/// Returns [`EngineError`](crate::error::EngineError) with `JSON.WRITE_FAILED` when the value cannot be serialized or the atomic write fails.
pub fn write_json_file<T: Serialize>(
    path: &Path,
    value: &T,
    modes: WriteModes,
) -> EngineResult<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(|error| {
        crate::error::EngineError::new(
            crate::error::EngineErrorCode::JsonWriteFailed,
            format!("failed to create {}", parent.display()),
        )
        .with_context(format!("error={error}"))
        .with_source(error)
    })?;
    #[cfg(unix)]
    if let Some(mode) = modes.directory_mode {
        apply_mode(parent, mode);
    }

    let body = serde_json::to_string_pretty(value).map_err(|error| {
        crate::error::EngineError::new(
            crate::error::EngineErrorCode::JsonWriteFailed,
            "failed to serialize JSON",
        )
        .with_context(format!("error={error}"))
        .with_source(error)
    })?;

    let tmp = path.with_extension(format!(
        "{}.{}.tmp",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    let write_result = (|| -> std::io::Result<()> {
        let mut file = fs::File::create(&tmp)?;
        #[cfg(unix)]
        if let Some(mode) = modes.file_mode {
            apply_mode(&tmp, mode);
        }
        file.write_all(body.as_bytes())?;
        file.write_all(b"\n")?;
        drop(file);
        fs::rename(&tmp, path)
    })();

    if let Err(error) = write_result {
        let _ = fs::remove_file(&tmp);
        return Err(crate::error::EngineError::new(
            crate::error::EngineErrorCode::JsonWriteFailed,
            format!("failed to write {}", path.display()),
        )
        .with_context(format!("error={error}"))
        .with_source(error));
    }
    Ok(())
}

#[cfg(unix)]
fn apply_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(mode));
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn round_trip_and_fallback() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config").join("state.json");
        assert_eq!(
            read_json_file::<serde_json::Value>(&path, json!({"ok": true})).expect("read"),
            json!({"ok": true})
        );
        write_json_file(&path, &json!({"n": 7}), SECURE_MODES).expect("write");
        assert_eq!(
            read_json_file::<serde_json::Value>(&path, json!(null)).expect("re-read"),
            json!({"n": 7})
        );
        let text = fs::read_to_string(&path).expect("text");
        assert!(text.ends_with("}\n"));
        assert!(text.contains("\"n\": 7"));
    }

    #[test]
    fn parse_error_propagates() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("bad.json");
        fs::write(&path, "not json").expect("write");
        let error = read_json_file::<serde_json::Value>(&path, json!(null));
        assert!(error.is_err());
    }
}
