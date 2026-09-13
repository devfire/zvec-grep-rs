//! Daemon process control: instance lock, status probe, stop, spawn.
//!
//! Mirrors `../zvec-grep/src/daemon/server-controller.ts`
//! (`DaemonInstanceLock`, `serverStatus`, `stopServer`, `startServer`).
//! Two divergences (see `docs/ts-divergence.md`):
//!
//! - Process signaling uses `/bin/kill` (`SIGTERM` then `SIGKILL`) instead
//!   of `process.kill`: `libc::kill` is `unsafe`, and the workspace
//!   forbids `unsafe` outright. Escalation is Unix-only; elsewhere a
//!   refused shutdown surfaces as [`DaemonError::ShutdownFailed`].
//! - The machine name reads `$HOSTNAME` (falling back to
//!   `/proc/sys/kernel/hostname`) instead of `os.hostname()` — same
//!   stability contract for lock comparison, no new dependency.
//!
//! Lock records tolerate unknown JSON fields, so a lock written by the TS
//! daemon still compares by pid/hostname instead of failing to parse.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::config::resolve_client_token;
use crate::errors::DaemonError;

/// Grace for a refused shutdown POST before `SIGTERM`.
pub const TERMINATION_GRACE: Duration = Duration::from_secs(2);

/// Heartbeat interval for a held instance lock.
pub const LOCK_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);

/// One daemon instance record (`instance.lock`). Serialized camelCase so
/// locks stay interoperable with the TS daemon's `instance.lock`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InstanceRecordBody {
    pid: u32,
    hostname: String,
    instance_token: String,
    started_at_ms: u64,
    updated_at_ms: u64,
    server_url: String,
    #[serde(default)]
    ready: bool,
}

/// Live instance identity used for comparisons.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonInstanceRecord {
    /// Daemon PID.
    pub pid: u32,
    /// Machine name.
    pub hostname: String,
    /// Unique instance token.
    pub instance_token: String,
    /// Health endpoint base.
    pub server_url: String,
    /// Readiness flag.
    pub ready: bool,
}

/// Liveness snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonControlStatus {
    /// A live holder owns the lock.
    pub running: bool,
    /// The holder answered `/healthz` and marked ready.
    pub ready: bool,
    /// Holder PID, when running.
    pub pid: Option<u32>,
    /// Server URL, when running.
    pub server_url: Option<String>,
}

/// Held instance lock with a 5 s heartbeat. Heartbeat stops on `release`.
pub struct DaemonInstanceLock {
    path: PathBuf,
    record: InstanceRecordBody,
    heartbeat: Option<JoinHandle<()>>,
}

impl DaemonInstanceLock {
    /// Acquires `<daemon-home>/instance.lock` exclusively (mode `0600`),
    /// taking over stale locks (dead PID or foreign hostname) after up to
    /// three attempts. A live same-host holder fails with
    /// [`DaemonError::AlreadyRunning`].
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError::AlreadyRunning`] when a live holder owns the lock, or
    /// [`DaemonError::IndexFailed`] when the lock directory or file cannot be created.
    pub async fn acquire(home: Option<&Path>, server_url: &str) -> Result<Self, DaemonError> {
        let dir = daemon_dir(home);
        tokio::fs::create_dir_all(&dir)
            .await
            .map_err(|error| DaemonError::IndexFailed {
                message: format!("failed to create daemon directory: {error}"),
            })?;
        let path = dir.join("instance.lock");
        for _ in 0..3 {
            let record = InstanceRecordBody {
                pid: std::process::id(),
                hostname: machine_name(),
                instance_token: Uuid::new_v4().to_string(),
                started_at_ms: now_ms(),
                updated_at_ms: now_ms(),
                server_url: server_url.to_owned(),
                ready: false,
            };
            match tokio::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .await
            {
                Ok(mut file) => {
                    use tokio::io::AsyncWriteExt;
                    let body = format!("{}\n", serde_json::to_string(&record).unwrap_or_default());
                    file.write_all(body.as_bytes()).await.map_err(|error| {
                        DaemonError::IndexFailed {
                            message: format!("failed to write instance lock: {error}"),
                        }
                    })?;
                    file.flush()
                        .await
                        .map_err(|error| DaemonError::IndexFailed {
                            message: format!("failed to write instance lock: {error}"),
                        })?;
                    drop(file);
                    chmod_owner_only(&path);
                    return Ok(Self {
                        path,
                        record,
                        heartbeat: None,
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => {
                    return Err(DaemonError::IndexFailed {
                        message: format!("failed to acquire instance lock: {error}"),
                    });
                }
            }
            if let Some(existing) = read_record(&path).await
                && existing.hostname == machine_name()
                && process_alive(existing.pid)
            {
                return Err(DaemonError::AlreadyRunning { pid: existing.pid });
            }
            let _ = tokio::fs::remove_file(&path).await;
        }
        Err(DaemonError::IndexFailed {
            message: "could not acquire the zvec-grep server instance lock".to_owned(),
        })
    }

    /// Marks the instance ready and starts the heartbeat.
    pub async fn mark_ready(&mut self) {
        self.record.ready = true;
        self.write().await;
        if self.heartbeat.is_none() {
            let path = self.path.clone();
            let record = self.record.clone();
            self.heartbeat = Some(tokio::spawn(async move {
                let mut record = record;
                loop {
                    tokio::time::sleep(LOCK_HEARTBEAT_INTERVAL).await;
                    record.updated_at_ms = now_ms();
                    if read_record(&path).await.is_some_and(|current| {
                        current.instance_token == record.instance_token && current.pid == record.pid
                    }) {
                        let body =
                            format!("{}\n", serde_json::to_string(&record).unwrap_or_default());
                        let _ = tokio::fs::write(&path, body).await;
                    } else {
                        return;
                    }
                }
            }));
        }
    }

    /// Current record.
    #[must_use]
    pub fn record(&self) -> DaemonInstanceRecord {
        DaemonInstanceRecord {
            pid: self.record.pid,
            hostname: self.record.hostname.clone(),
            instance_token: self.record.instance_token.clone(),
            server_url: self.record.server_url.clone(),
            ready: self.record.ready,
        }
    }

    /// Stops the heartbeat and removes the lock when it is still ours.
    pub async fn release(mut self) {
        if let Some(heartbeat) = self.heartbeat.take() {
            heartbeat.abort();
        }
        if read_record(&self.path).await.is_some_and(|current| {
            current.instance_token == self.record.instance_token && current.pid == self.record.pid
        }) {
            let _ = tokio::fs::remove_file(&self.path).await;
        }
    }

    async fn write(&mut self) {
        self.record.updated_at_ms = now_ms();
        let current = read_record(&self.path).await;
        if current.is_some_and(|current| {
            current.instance_token != self.record.instance_token || current.pid != self.record.pid
        }) {
            return;
        }
        let body = format!(
            "{}\n",
            serde_json::to_string(&self.record).unwrap_or_default()
        );
        let _ = tokio::fs::write(&self.path, body).await;
    }
}

/// Reads the liveness of the recorded instance: lock holder alive on this
/// host plus a `/healthz` probe for readiness.
pub async fn server_status(home: Option<&Path>) -> DaemonControlStatus {
    let Some(record) = read_record(&lock_path(home)).await else {
        return DaemonControlStatus {
            running: false,
            ready: false,
            pid: None,
            server_url: None,
        };
    };
    if record.hostname != machine_name() || !process_alive(record.pid) {
        return DaemonControlStatus {
            running: false,
            ready: false,
            pid: None,
            server_url: None,
        };
    }
    let ready = health_ready(&record.server_url).await && record.ready;
    DaemonControlStatus {
        running: true,
        ready,
        pid: Some(record.pid),
        server_url: Some(record.server_url),
    }
}

/// Stops the recorded daemon: authenticated `POST /control/shutdown`,
/// then process-exit wait, then `SIGTERM`/`SIGKILL` escalation on Unix.
/// Refusing to stop our own process surfaces as `ShutdownFailed` with a
/// zero status (no HTTP round trip happened).
///
/// # Errors
///
/// Returns [`DaemonError::ShutdownFailed`] when the target is the current process, the
/// shutdown request is refused, or escalation fails.
pub async fn stop_server(
    home: Option<&Path>,
    timeout: Duration,
    token_file: Option<PathBuf>,
) -> Result<DaemonControlStatus, DaemonError> {
    let path = lock_path(home);
    let Some(record) = read_record(&path).await else {
        return Ok(DaemonControlStatus {
            running: false,
            ready: false,
            pid: None,
            server_url: None,
        });
    };
    if record.hostname != machine_name() || !process_alive(record.pid) {
        return Ok(DaemonControlStatus {
            running: false,
            ready: false,
            pid: None,
            server_url: None,
        });
    }
    if record.pid == std::process::id() {
        return Err(DaemonError::ShutdownFailed { status: 0 });
    }
    let token = resolve_client_token(token_file, home.map(Path::to_path_buf).as_deref())
        .map_err(|_| DaemonError::ShutdownFailed { status: 0 })?;
    let mut accepted = false;
    if let Ok(response) = shutdown_request(&record.server_url, token.as_deref(), timeout).await {
        if response == 202 {
            accepted = true;
        } else {
            return Err(DaemonError::ShutdownFailed { status: response });
        }
    }
    if accepted && wait_for_exit(record.pid, timeout).await {
        remove_record_if(home, &record).await;
        return Ok(DaemonControlStatus {
            running: false,
            ready: false,
            pid: None,
            server_url: None,
        });
    }
    force_stop(home, &record, timeout).await
}

/// Spawns a detached `zg server run` child and waits for readiness.
/// (Phase I owns the exact CLI surface; this is the spawn/wait primitive.)
///
/// # Errors
///
/// Returns [`DaemonError::IndexFailed`] when the child cannot be spawned or readiness
/// times out.
pub async fn start_server(
    program: &str,
    args: &[String],
    home: Option<&Path>,
    timeout: Duration,
) -> Result<DaemonControlStatus, DaemonError> {
    let current = server_status(home).await;
    if current.ready {
        return Ok(current);
    }
    tokio::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|error| DaemonError::IndexFailed {
            message: format!("failed to spawn server: {error}"),
        })?;
    wait_for_status(home, timeout).await
}

/// Polls until the recorded server is running and ready.
///
/// # Errors
///
/// Returns [`DaemonError::IndexFailed`] when readiness times out.
pub async fn wait_for_status(
    home: Option<&Path>,
    timeout: Duration,
) -> Result<DaemonControlStatus, DaemonError> {
    let deadline = Instant::now() + timeout;
    loop {
        let status = server_status(home).await;
        if status.running && status.ready {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            return Err(DaemonError::IndexFailed {
                message: "timed out waiting for zvec-grep server to start".to_owned(),
            });
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn daemon_dir(home: Option<&Path>) -> PathBuf {
    home.map_or_else(crate::logger::daemon_home, Path::to_path_buf)
}

fn lock_path(home: Option<&Path>) -> PathBuf {
    daemon_dir(home).join("instance.lock")
}

/// Lenient lock read: required pid/hostname/server_url, everything else
/// defaulted, unknown fields ignored (TS-written locks stay readable).
async fn read_record(path: &Path) -> Option<DaemonInstanceRecord> {
    let body = tokio::fs::read_to_string(path).await.ok()?;
    let value: serde_json::Value = serde_json::from_str(&body).ok()?;
    Some(DaemonInstanceRecord {
        pid: value.get("pid")?.as_u64()? as u32,
        hostname: value.get("hostname")?.as_str()?.to_owned(),
        instance_token: value.get("instanceToken")?.as_str()?.to_owned(),
        server_url: value.get("serverUrl")?.as_str()?.to_owned(),
        ready: value
            .get("ready")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
    })
}

fn machine_name() -> String {
    if let Ok(name) = std::env::var("HOSTNAME")
        && !name.trim().is_empty()
    {
        return name;
    }
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .ok()
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "unknown".to_owned())
}

/// Alive check without signals: `/proc/<pid>` on Linux, conservative
/// "alive" elsewhere so a lock is never taken over blindly.
fn process_alive(pid: u32) -> bool {
    #[cfg(target_os = "linux")]
    {
        Path::new(&format!("/proc/{pid}")).exists()
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        true
    }
}

async fn health_ready(server_url: &str) -> bool {
    let url = format!("{}/healthz", server_url.trim_end_matches('/'));
    reqwest::Client::new()
        .get(url)
        .timeout(Duration::from_secs(1))
        .send()
        .await
        .is_ok_and(|response| response.status().is_success())
}

async fn shutdown_request(
    server_url: &str,
    token: Option<&str>,
    timeout: Duration,
) -> Result<u16, ()> {
    let url = format!("{}/control/shutdown", server_url.trim_end_matches('/'));
    let mut request = reqwest::Client::new()
        .post(url)
        .timeout(timeout.min(Duration::from_secs(2)));
    if let Some(token) = token {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    Ok(request.send().await.map_err(|_| ())?.status().as_u16())
}

async fn wait_for_exit(pid: u32, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if !process_alive(pid) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    !process_alive(pid)
}

async fn remove_record_if(home: Option<&Path>, stopped: &DaemonInstanceRecord) {
    let path = lock_path(home);
    if read_record(&path).await.is_some_and(|current| {
        current.pid == stopped.pid && current.instance_token == stopped.instance_token
    }) {
        let _ = tokio::fs::remove_file(&path).await;
    }
}

/// Unix-only signal escalation via `/bin/kill` (no `unsafe` anywhere).
async fn force_stop(
    home: Option<&Path>,
    record: &DaemonInstanceRecord,
    timeout: Duration,
) -> Result<DaemonControlStatus, DaemonError> {
    #[cfg(unix)]
    {
        if !process_alive(record.pid) {
            remove_record_if(home, record).await;
            return Ok(DaemonControlStatus {
                running: false,
                ready: false,
                pid: None,
                server_url: None,
            });
        }
        let grace = TERMINATION_GRACE.min(timeout.max(Duration::from_millis(100)));
        signal(record.pid, "-TERM");
        if wait_for_exit(record.pid, grace).await {
            remove_record_if(home, record).await;
            return Ok(DaemonControlStatus {
                running: false,
                ready: false,
                pid: None,
                server_url: None,
            });
        }
        signal(record.pid, "-KILL");
        if !wait_for_exit(record.pid, grace).await {
            return Err(DaemonError::ShutdownFailed { status: 0 });
        }
        remove_record_if(home, record).await;
        Ok(DaemonControlStatus {
            running: false,
            ready: false,
            pid: None,
            server_url: None,
        })
    }
    #[cfg(not(unix))]
    {
        let _ = (home, record, timeout);
        Err(DaemonError::ShutdownFailed { status: 0 })
    }
}

#[cfg(unix)]
fn signal(pid: u32, signal: &str) {
    let _ = std::process::Command::new("kill")
        .arg(signal)
        .arg(pid.to_string())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

fn chmod_owner_only(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(metadata) = std::fs::metadata(path) {
            let mut permissions = metadata.permissions();
            permissions.set_mode(0o600);
            let _ = std::fs::set_permissions(path, permissions);
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[tokio::test]
    async fn lock_round_trip_and_status() {
        let home = home();
        let dir = home.path().to_owned();
        let mut lock = DaemonInstanceLock::acquire(Some(dir.as_path()), "http://127.0.0.1:7999")
            .await
            .unwrap();
        // Second acquire sees a live holder (our own process).
        assert!(matches!(
            DaemonInstanceLock::acquire(Some(dir.as_path()), "http://127.0.0.1:7999").await,
            Err(DaemonError::AlreadyRunning { .. })
        ));
        // No server answers `/healthz`: running but not ready.
        let status = server_status(Some(dir.as_path())).await;
        assert!(status.running && !status.ready);
        lock.mark_ready().await;
        assert!(lock.record().ready);
        lock.release().await;
        let status = server_status(Some(dir.as_path())).await;
        assert!(!status.running);
    }

    #[tokio::test]
    async fn stale_locks_are_taken_over() {
        let home = home();
        let dir = home.path().join("daemon");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        // Dead PID on this host: takeover must succeed.
        let body = serde_json::json!({
            "pid": 4_000_000_000u64,
            "hostname": machine_name(),
            "instanceToken": "stale",
            "startedAt": 0,
            "updatedAt": 0,
            "serverUrl": "http://127.0.0.1:7999",
            "ready": false,
            "mcpToolset": "agent",
        });
        tokio::fs::write(dir.join("instance.lock"), format!("{body}\n"))
            .await
            .unwrap();
        let lock = DaemonInstanceLock::acquire(Some(home.path()), "http://127.0.0.1:7999")
            .await
            .unwrap();
        assert_ne!(lock.record().instance_token, "stale");
        lock.release().await;
    }

    #[tokio::test]
    async fn stop_refuses_the_current_process() {
        let home = home();
        let lock = DaemonInstanceLock::acquire(Some(home.path()), "http://127.0.0.1:9")
            .await
            .unwrap();
        let error = stop_server(Some(home.path()), Duration::from_millis(100), None)
            .await
            .unwrap_err();
        assert!(matches!(error, DaemonError::ShutdownFailed { .. }));
        lock.release().await;
    }
}
