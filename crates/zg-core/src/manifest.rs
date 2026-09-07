//! Workspace manifest.json v1 read/write/delete + structural validation.
//!
//! Ports `engine/manifest.ts`. Reads validate the raw JSON shape before
//! serde derivation so a corrupt manifest yields `MANIFEST.INVALID` instead
//! of a generic JSON error; writes go through atomic
//! [`crate::utils::json_io`] with `0700` directories / `0600` files.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::EmbeddingRuntimeConfig;
use crate::error::{EngineError, EngineResult, codes};
use crate::types::WorkspaceIndexInfo;

/// Workspace manifest file name inside a workspace home directory.
pub const WORKSPACE_MANIFEST_FILE: &str = "manifest.json";

/// Current on-disk manifest format version.
pub const CURRENT_MANIFEST_VERSION: u32 = 1;

/// Validated workspace manifest: index identity plus runtime options.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceManifest {
    #[serde(flatten)]
    pub info: WorkspaceIndexInfo,
    pub manifest_version: u32,
    pub embedding_runtime: EmbeddingRuntimeConfig,
}

impl WorkspaceManifest {
    /// Splits the manifest back into its index identity half.
    pub fn index_info(&self) -> WorkspaceIndexInfo {
        self.info.clone()
    }
}

/// Absolute path of the manifest file for a workspace home directory.
pub fn workspace_manifest_path(home: &Path) -> PathBuf {
    home.join(WORKSPACE_MANIFEST_FILE)
}

/// Reads and validates the manifest; returns `None` when no file exists.
pub fn read_workspace_manifest(home: &Path) -> EngineResult<Option<WorkspaceManifest>> {
    let path = workspace_manifest_path(home);
    let value: Value = crate::utils::json_io::read_json_file(&path, Value::Null)?;
    if value.is_null() {
        return Ok(None);
    }
    if !is_workspace_manifest(&value) {
        return Err(invalid_manifest(&path.display().to_string(), None));
    }
    serde_json::from_value(value)
        .map_err(|error| invalid_manifest(&path.display().to_string(), Some(&error.to_string())))
        .map(Some)
}

/// Persists the manifest atomically with secure file modes.
pub fn write_workspace_manifest(home: &Path, manifest: &WorkspaceManifest) -> EngineResult<()> {
    crate::utils::json_io::write_json_file(
        &workspace_manifest_path(home),
        manifest,
        crate::utils::json_io::SECURE_MODES,
    )
}

/// Deletes the manifest file; a missing file is not an error.
pub fn delete_workspace_manifest(home: &Path) -> EngineResult<()> {
    let path = workspace_manifest_path(home);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(EngineError::new(
            crate::error::EngineErrorCode::from_static("MANIFEST.DELETE_FAILED"),
            format!("failed to delete {}", path.display()),
        )
        .with_context(format!("error={error}"))),
    }
}

/// Projects the manifest back to its [`WorkspaceIndexInfo`] half, mirroring
/// `workspaceIndexInfoFromManifest`.
pub fn workspace_index_info_from_manifest(manifest: &WorkspaceManifest) -> WorkspaceIndexInfo {
    manifest.info.clone()
}

fn invalid_manifest(path: &str, cause: Option<&str>) -> EngineError {
    let mut context = format!("path={path}");
    if let Some(cause) = cause {
        context.push_str(&format!("\nerror={cause}"));
    }
    EngineError::new(
        codes::manifest_invalid(),
        "Workspace index manifest is invalid",
    )
    .with_context(context)
}

fn is_workspace_manifest(value: &Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    if object.get("manifestVersion").and_then(Value::as_u64)
        != Some(u64::from(CURRENT_MANIFEST_VERSION))
    {
        return false;
    }
    if !is_non_empty_string(object.get("id"))
        || !is_non_empty_string(object.get("name"))
        || !is_non_empty_string(object.get("path"))
    {
        return false;
    }
    let root_paths_valid = object
        .get("rootPaths")
        .and_then(Value::as_array)
        .is_some_and(|roots| !roots.is_empty() && roots.iter().all(is_root_path));
    if !root_paths_valid {
        return false;
    }
    if !matches!(
        object.get("indexPolicy").and_then(Value::as_str),
        Some("enabled" | "disabled")
    ) {
        return false;
    }
    let embedding_valid = object
        .get("embedding")
        .is_some_and(|embedding| embedding.is_null() || is_embedding_schema(embedding));
    if !embedding_valid {
        return false;
    }
    let index_version_valid = object
        .get("indexVersion")
        .is_some_and(|version| version.is_null() || version.is_i64() || version.is_u64());
    if !index_version_valid {
        return false;
    }
    if !object.get("createdTime").is_some_and(Value::is_number)
        || !object.get("updatedTime").is_some_and(Value::is_number)
    {
        return false;
    }
    object
        .get("embeddingRuntime")
        .is_some_and(is_embedding_runtime)
}

fn is_root_path(value: &Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    if !is_non_empty_string(object.get("absolutePath")) {
        return false;
    }
    if !object.get("recursive").is_some_and(Value::is_boolean) {
        return false;
    }
    for field in [
        "include",
        "exclude",
        "globs",
        "insensitiveGlobs",
        "fileTypes",
        "excludedFileTypes",
        "ignoreFiles",
    ] {
        if !is_optional_string_array(object.get(field)) {
            return false;
        }
    }
    for field in ["hidden", "noIgnore", "follow"] {
        if !is_optional_boolean(object.get(field)) {
            return false;
        }
    }
    for field in ["maxDepth", "maxFileSizeBytes"] {
        if !is_optional_non_negative_integer(object.get(field)) {
            return false;
        }
    }
    true
}

fn is_embedding_schema(value: &Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    if !is_non_empty_string(object.get("provider")) || !is_non_empty_string(object.get("model")) {
        return false;
    }
    let dimension_valid = object
        .get("dimension")
        .and_then(Value::as_u64)
        .is_some_and(|dimension| dimension > 0);
    if !dimension_valid {
        return false;
    }
    matches!(
        object.get("metric").and_then(Value::as_str),
        Some("cosine" | "dot" | "euclidean")
    )
}

fn is_embedding_runtime(value: &Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    for field in ["apiKey", "endpoint"] {
        let present = object.get(field);
        if !present.is_none_or(|item| item.is_null() || item.is_string()) {
            return false;
        }
    }
    object.get("device").is_none_or(|device| {
        device.is_null()
            || matches!(
                device.as_str(),
                Some("auto" | "cpu" | "metal" | "vulkan" | "cuda")
            )
    })
}

fn is_optional_string_array(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => true,
        Some(Value::Array(items)) => items.iter().all(Value::is_string),
        Some(_) => false,
    }
}

fn is_optional_boolean(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => true,
        Some(Value::Bool(_)) => true,
        Some(_) => false,
    }
}

fn is_optional_non_negative_integer(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => true,
        Some(item) => item.as_u64().is_some(),
    }
}

fn is_non_empty_string(value: Option<&Value>) -> bool {
    value
        .and_then(Value::as_str)
        .is_some_and(|text| !text.is_empty())
}
