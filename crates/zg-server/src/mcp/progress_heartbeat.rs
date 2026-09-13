//! Progress heartbeat: periodic MCP progress notifications while a
//! long-running tool call is in flight.
//!
//! Mirrors `../zvec-grep/src/mcp/progress-heartbeat.ts`
//! (`REMOTE_AUTHORIZATION_HEARTBEAT_MS`,
//! `LONG_RUNNING_MCP_TIMEOUT_MS`, `withProgressHeartbeat`). The guard
//! spawns a 15 s interval sender against the caller's progress token;
//! dropping the guard stops the beats. Notifications go through the
//! rmcp peer, so the guard is created per tool call from the call's
//! request context.

use std::time::Duration;

use rmcp::RoleServer;
use rmcp::model::{NumberOrString, ProgressNotificationParam, ProgressToken};
use rmcp::service::Peer;

/// Heartbeat interval while waiting on authorization or a long index.
pub const REMOTE_AUTHORIZATION_HEARTBEAT_MS: u64 = 15_000;

/// Upper-bound timeout for long-running MCP operations (mirrors TS
/// `LONG_RUNNING_MCP_TIMEOUT_MS`, `i32::MAX` millis).
pub const LONG_RUNNING_MCP_TIMEOUT: Duration = Duration::from_millis(2_147_483_647);

/// Periodic progress sender for one tool call. Created only when the
/// caller supplied a progress token; otherwise [`ProgressHeartbeat::noop`].
pub struct ProgressHeartbeat {
    task: Option<tokio::task::JoinHandle<()>>,
}

impl ProgressHeartbeat {
    /// Starts beating every [`REMOTE_AUTHORIZATION_HEARTBEAT_MS`] with
    /// `message` until dropped or [`ProgressHeartbeat::stop`]ped.
    #[must_use]
    pub fn start(peer: Peer<RoleServer>, token: ProgressToken, message: String) -> Self {
        let task = tokio::spawn(async move {
            let mut interval =
                tokio::time::interval(Duration::from_millis(REMOTE_AUTHORIZATION_HEARTBEAT_MS));
            // Monotonic progress values (mirrors the TS `progress += 1`
            // counter starting at `Date.now()`).
            let mut progress = now_ms();
            loop {
                interval.tick().await;
                progress = progress.saturating_add(1);
                let notification = ProgressNotificationParam {
                    progress_token: token.clone(),
                    progress: progress as f64,
                    total: None,
                    message: Some(message.clone()),
                };
                if peer.notify_progress(notification).await.is_err() {
                    break;
                }
            }
        });
        Self { task: Some(task) }
    }

    /// No-op heartbeat for calls without a progress token.
    #[must_use]
    pub fn noop() -> Self {
        Self { task: None }
    }

    /// Stops the beats and awaits the sender task.
    pub async fn stop(mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
            let _ = task.await;
        }
    }
}

impl Drop for ProgressHeartbeat {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

/// Reads the `progressToken` from a request `_meta` object (mirrors
/// `extra.mcpReq._meta?.progressToken`).
#[must_use]
pub fn progress_token_from_meta(meta: &rmcp::model::Meta) -> Option<ProgressToken> {
    let value = meta.0.get("progressToken")?;
    match value {
        serde_json::Value::Number(number) => number
            .as_i64()
            .map(|number| ProgressToken(NumberOrString::Number(number))),
        serde_json::Value::String(text) => {
            Some(ProgressToken(NumberOrString::String(text.as_str().into())))
        }
        serde_json::Value::Null
        | serde_json::Value::Bool(_)
        | serde_json::Value::Array(_)
        | serde_json::Value::Object(_) => None,
    }
}
