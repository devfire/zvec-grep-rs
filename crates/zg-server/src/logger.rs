//! Request-id JSONL daemon logger, mirroring `src/daemon/logger.ts`.
//!
//! `DaemonLogger::event` appends one JSON record per call to
//! `<daemon-home>/logs/server.log` (0700 dirs, 0600 file) and mirrors the
//! event through `tracing`. Field sanitization matches TS: credential- or
//! query-shaped keys are dropped, strings truncate at 512 chars.

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;

use sha2::{Digest, Sha256};

use crate::trace::current_trace_context;
pub use crate::trace::{RequestId, request_id};

/// Scalar log field value.
#[derive(Debug, Clone, PartialEq)]
pub enum LogField {
    Text(String),
    Int(i64),
    Bool(bool),
}

impl From<&str> for LogField {
    fn from(value: &str) -> Self {
        Self::Text(value.to_owned())
    }
}

impl From<String> for LogField {
    fn from(value: String) -> Self {
        Self::Text(value)
    }
}

impl From<i32> for LogField {
    fn from(value: i32) -> Self {
        Self::Int(i64::from(value))
    }
}

impl From<i64> for LogField {
    fn from(value: i64) -> Self {
        Self::Int(value)
    }
}

impl From<u64> for LogField {
    fn from(value: u64) -> Self {
        Self::Int(value as i64)
    }
}

impl From<usize> for LogField {
    fn from(value: usize) -> Self {
        Self::Int(value as i64)
    }
}

impl From<bool> for LogField {
    fn from(value: bool) -> Self {
        Self::Bool(value)
    }
}

/// Daemon home: `$ZVEC_GREP_HOME/daemon`, else `~/.zvec-grep/daemon`
/// (mirrors TS `daemonHome` over `defaultHome`).
#[must_use]
pub fn daemon_home() -> PathBuf {
    zg_core::paths::default_home().join("daemon")
}

/// Appends one JSON record to the daemon log. Failures are swallowed after
/// a `tracing::warn` (mirrors TS `.catch(() => undefined)`).
#[derive(Debug, Clone)]
pub struct DaemonLogger {
    path: PathBuf,
}

impl DaemonLogger {
    /// Logs to `<daemon_home>/logs/server.log`.
    #[must_use]
    pub fn new() -> Self {
        Self::with_home(daemon_home())
    }

    /// Logs under an explicit daemon home (tests).
    #[must_use]
    pub fn with_home(home: PathBuf) -> Self {
        Self {
            path: home.join("logs").join("server.log"),
        }
    }

    /// Log file path.
    #[must_use]
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// Records one event with the ambient trace id when set.
    pub fn event(&self, name: &str, fields: BTreeMap<String, LogField>) {
        let mut record = serde_json::Map::new();
        record.insert(
            "timestamp".to_owned(),
            serde_json::Value::String(
                chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            ),
        );
        record.insert(
            "event".to_owned(),
            serde_json::Value::String(name.to_owned()),
        );
        if let Some(context) = current_trace_context() {
            record.insert(
                "trace_id".to_owned(),
                serde_json::Value::String(context.trace_id().to_owned()),
            );
        }
        for (key, value) in sanitize_fields(fields) {
            record.insert(
                key,
                match value {
                    LogField::Text(text) => serde_json::Value::String(text),
                    LogField::Int(number) => serde_json::Value::Number(number.into()),
                    LogField::Bool(flag) => serde_json::Value::Bool(flag),
                },
            );
        }
        let line = serde_json::Value::Object(record).to_string();
        tracing::info!(event = name, "daemon log");
        if let Err(error) = append_line(&self.path, &line) {
            tracing::warn!(path = %self.path.display(), error = %error, "daemon log append failed");
        }
    }
}

impl Default for DaemonLogger {
    fn default() -> Self {
        Self::new()
    }
}

fn append_line(path: &std::path::Path, line: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
        }
    }
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(line.as_bytes())?;
    file.write_all(b"\n")?;
    drop(file);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// Drops credential/query-shaped keys and truncates long strings,
/// mirroring TS `sanitizeFields`.
fn sanitize_fields(fields: BTreeMap<String, LogField>) -> BTreeMap<String, LogField> {
    fields
        .into_iter()
        .filter_map(|(key, value)| {
            if is_sensitive_key(&key) {
                return None;
            }
            match value {
                LogField::Text(text) if text.chars().count() > 512 => {
                    let kept: String = text.chars().take(511).collect();
                    Some((key, LogField::Text(format!("{kept}\u{2026}"))))
                }
                other @ LogField::Text(_)
                | other @ LogField::Int(_)
                | other @ LogField::Bool(_) => Some((key, other)),
            }
        })
        .collect()
}

/// Stable 16-hex identity for a workspace root (mirrors TS `rootIdentity`).
#[must_use]
pub fn root_identity(root: &str) -> String {
    sha_hex16(root)
}

/// Stable 16-hex identity for an opaque value (mirrors TS `opaqueIdentity`).
#[must_use]
pub fn opaque_identity(value: &str) -> String {
    sha_hex16(value)
}

fn sha_hex16(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    let mut hex = String::with_capacity(16);
    for byte in digest.iter().take(8) {
        hex.push_str(&format!("{byte:02x}"));
    }
    hex
}

fn is_sensitive_key(key: &str) -> bool {
    let lower = key.to_lowercase();
    lower.contains("token")
        || lower.contains("api_key")
        || lower.contains("apikey")
        || lower.contains("api-key")
        || lower.contains("api key")
        || lower.contains("authorization")
        || lower.contains("query")
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::trace::run_with_trace_context;

    fn fields() -> BTreeMap<String, LogField> {
        BTreeMap::from([
            ("root".to_owned(), LogField::from("repo")),
            ("count".to_owned(), LogField::from(3)),
            ("api_key".to_owned(), LogField::from("secret")),
            ("query".to_owned(), LogField::from("select *")),
        ])
    }

    #[test]
    fn events_append_jsonl_and_sanitize() {
        let dir = tempfile::tempdir().expect("tempdir");
        let logger = DaemonLogger::with_home(dir.path().to_path_buf());
        logger.event("index_complete", fields());
        let text = std::fs::read_to_string(logger.path()).expect("log");
        let record: serde_json::Value = serde_json::from_str(text.trim()).expect("json");
        assert_eq!(record["event"], "index_complete");
        assert_eq!(record["root"], "repo");
        assert_eq!(record["count"], 3);
        assert!(record.get("api_key").is_none());
        assert!(record.get("query").is_none());
        assert!(record["timestamp"].is_string());
    }

    #[test]
    fn events_carry_ambient_trace_id() {
        let dir = tempfile::tempdir().expect("tempdir");
        let logger = DaemonLogger::with_home(dir.path().to_path_buf());
        let context = crate::trace::trace_context_from_mcp_meta(&serde_json::json!({
            "traceparent": "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
        }))
        .expect("context");
        run_with_trace_context(Some(context), || {
            logger.event("search", BTreeMap::new());
        });
        let text = std::fs::read_to_string(logger.path()).expect("log");
        assert!(text.contains("4bf92f3577b34da6a3ce929d0e0e4736"));
    }

    #[test]
    fn identities_are_stable_16_hex() {
        assert_eq!(root_identity("/repo").len(), 16);
        assert_eq!(root_identity("/repo"), root_identity("/repo"));
        assert_ne!(root_identity("/a"), root_identity("/b"));
        assert!(!request_id().as_str().is_empty());
    }
}
