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
use zg_core::types::UnixMillis;

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
    /// The record is published atomically: a fully written temp file is
    /// hard-linked into place, so the lock path never holds an empty or
    /// partial record (#91). A lock that exists but does not parse is
    /// treated as live (a concurrent acquirer may be mid-publish on an
    /// older build) and is never removed here.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError::AlreadyRunning`] when a live holder owns the lock, or
    /// [`DaemonError::IndexFailed`] when the lock directory or file cannot be created,
    /// or when an existing lock is unreadable.
    pub async fn acquire(home: Option<&Path>, server_url: &str) -> Result<Self, DaemonError> {
        let dir = daemon_dir(home);
        tokio::fs::create_dir_all(&dir)
            .await
            .map_err(|error| DaemonError::IndexFailed {
                message: format!("failed to create daemon directory: {error}"),
            })?;
        let path = dir.join("instance.lock");
        // One identity for the whole attempt sequence: retrying with a
        // fresh token per attempt would orphan the previous token's claim.
        let record = InstanceRecordBody {
            pid: std::process::id(),
            hostname: machine_name(),
            instance_token: Uuid::new_v4().to_string(),
            // Clock-unavailable direction: metadata only — liveness is
            // PID-based, so a `0` stamp is inert.
            started_at_ms: UnixMillis::now_ms_or(0),
            updated_at_ms: UnixMillis::now_ms_or(0),
            server_url: server_url.to_owned(),
            ready: false,
        };
        let body = record_body(&record);
        for _ in 0..3 {
            match publish_new_lock(&path, &body).await {
                Ok(()) => {
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
            match read_lock_state(&path).await {
                LockState::Record(existing)
                    if existing.hostname == machine_name() && process_alive(existing.pid) =>
                {
                    return Err(DaemonError::AlreadyRunning { pid: existing.pid });
                }
                LockState::Record(existing) => {
                    // Stale: remove only if the file still names that exact
                    // instance (re-read immediately before removal, #91).
                    remove_lock_if(&path, existing.pid, &existing.instance_token).await;
                }
                LockState::Unparseable => {
                    return Err(DaemonError::IndexFailed {
                        message: "existing instance lock is unreadable; treating it as held by a live daemon and refusing takeover".to_owned(),
                    });
                }
                // Raced a release/removal: retry the exclusive publish.
                LockState::Missing => {}
            }
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
                    // Same direction: heartbeat stamp is metadata; PID decides.
                    record.updated_at_ms = UnixMillis::now_ms_or(0);
                    // Atomic republish only while the lock still names this
                    // instance; a replaced/unreadable lock ends the heartbeat
                    // instead of being clobbered (#91).
                    if !publish_owned_lock(&path, &record).await {
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
        remove_lock_if(&self.path, self.record.pid, &self.record.instance_token).await;
    }

    async fn write(&mut self) {
        // Same direction: heartbeat stamp is metadata; PID decides.
        self.record.updated_at_ms = UnixMillis::now_ms_or(0);
        let _ = publish_owned_lock(&self.path, &self.record).await;
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
/// (The CLI owns the exact surface; this is the spawn/wait primitive.)
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

/// On-disk lock state. `Unparseable` (exists but not a readable record)
/// must stay distinguishable from `Missing`: a mid-publish or corrupt
/// lock is treated as held, never as stale/removable (#91).
enum LockState {
    Missing,
    Unparseable,
    Record(DaemonInstanceRecord),
}

async fn read_lock_state(path: &Path) -> LockState {
    let body = match tokio::fs::read_to_string(path).await {
        Ok(body) => body,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return LockState::Missing,
        Err(_) => return LockState::Unparseable,
    };
    parse_record(&body).map_or(LockState::Unparseable, LockState::Record)
}

fn parse_record(body: &str) -> Option<DaemonInstanceRecord> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    Some(DaemonInstanceRecord {
        pid: u32::try_from(value.get("pid")?.as_u64()?).ok()?,
        hostname: value.get("hostname")?.as_str()?.to_owned(),
        instance_token: value.get("instanceToken")?.as_str()?.to_owned(),
        server_url: value.get("serverUrl")?.as_str()?.to_owned(),
        ready: value
            .get("ready")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
    })
}

fn record_body(record: &InstanceRecordBody) -> String {
    format!("{}\n", serde_json::to_string(record).unwrap_or_default())
}

/// Writes `body` to a fresh unique temp file next to `path` (mode `0600`
/// before it becomes visible at any lock path) and returns its path.
async fn write_temp_lock(path: &Path, body: &str) -> Result<PathBuf, std::io::Error> {
    use tokio::io::AsyncWriteExt;
    let dir = path.parent().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "lock path has no parent")
    })?;
    let temp = dir.join(format!(
        "instance.lock.tmp-{}-{}",
        std::process::id(),
        Uuid::new_v4()
    ));
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .await?;
    if let Err(error) = file.write_all(body.as_bytes()).await {
        let _ = tokio::fs::remove_file(&temp).await;
        return Err(error);
    }
    if let Err(error) = file.flush().await {
        let _ = tokio::fs::remove_file(&temp).await;
        return Err(error);
    }
    drop(file);
    chmod_owner_only(&temp);
    Ok(temp)
}

/// Exclusive atomic publish: hard-links a fully written temp file to
/// `path`, failing with `AlreadyExists` if any lock is already there.
/// The lock path therefore never exposes an empty/partial record (#91).
async fn publish_new_lock(path: &Path, body: &str) -> Result<(), std::io::Error> {
    let temp = write_temp_lock(path, body).await?;
    let result = tokio::fs::hard_link(&temp, path).await;
    let _ = tokio::fs::remove_file(&temp).await;
    result
}

/// Atomic replace: renames a fully written temp file over `path`, so
/// readers only ever see the old or the new complete record (#91).
async fn publish_replace_lock(path: &Path, body: &str) -> Result<(), std::io::Error> {
    let temp = write_temp_lock(path, body).await?;
    let result = tokio::fs::rename(&temp, path).await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(&temp).await;
    }
    result
}

/// Atomically republishes `record` iff the lock still names the same
/// instance (PID + token). Returns `false` — publishing nothing — when
/// the lock was replaced or is unreadable; a vanished lock is recreated
/// exclusively, never by clobbering a concurrent winner (#91).
async fn publish_owned_lock(path: &Path, record: &InstanceRecordBody) -> bool {
    let body = record_body(record);
    match read_lock_state(path).await {
        LockState::Record(current)
            if current.pid == record.pid && current.instance_token == record.instance_token =>
        {
            publish_replace_lock(path, &body).await.is_ok()
        }
        LockState::Missing => publish_new_lock(path, &body).await.is_ok(),
        LockState::Record(_) | LockState::Unparseable => false,
    }
}

/// Removes the lock only if it still names `(pid, instance_token)`.
/// The record is re-read immediately before the removal (#91); a file
/// replaced in the meantime (or unreadable) is left untouched.
async fn remove_lock_if(path: &Path, pid: u32, instance_token: &str) {
    if read_record(path)
        .await
        .is_some_and(|current| current.pid == pid && current.instance_token == instance_token)
    {
        let _ = tokio::fs::remove_file(path).await;
    }
}

/// Lenient lock read: required `pid/hostname/server_url`, everything else
/// defaulted, unknown fields ignored (TS-written locks stay readable).
/// Missing and unreadable locks both yield `None`; callers that remove
/// or take over the lock must use [`read_lock_state`] instead (#91).
async fn read_record(path: &Path) -> Option<DaemonInstanceRecord> {
    match read_lock_state(path).await {
        LockState::Record(record) => Some(record),
        LockState::Missing | LockState::Unparseable => None,
    }
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
    remove_lock_if(&lock_path(home), stopped.pid, &stopped.instance_token).await;
}

/// Re-reads the lock and confirms it still names the same instance
/// (PID + instance token). A missing or replaced record means the PID
/// may have been reused by an unrelated process, so it must not be
/// signalled (#67).
async fn instance_still_recorded(home: Option<&Path>, expected: &DaemonInstanceRecord) -> bool {
    read_record(&lock_path(home)).await.is_some_and(|current| {
        current.pid == expected.pid && current.instance_token == expected.instance_token
    })
}

/// Unix-only signal escalation via `/bin/kill` (no `unsafe` anywhere).
/// The instance record is re-verified immediately before each signal:
/// if it no longer names this instance, no signal is sent and the stop
/// fails instead of killing a possibly unrelated reused PID (#67).
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
        if !instance_still_recorded(home, record).await {
            return Err(DaemonError::ShutdownFailed { status: 0 });
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
        if !instance_still_recorded(home, record).await {
            return Err(DaemonError::ShutdownFailed { status: 0 });
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
        let mut lock = DaemonInstanceLock::acquire(Some(dir.as_path()), "http://127.0.0.1:18099")
            .await
            .unwrap();
        // Second acquire sees a live holder (our own process).
        assert!(matches!(
            DaemonInstanceLock::acquire(Some(dir.as_path()), "http://127.0.0.1:18099").await,
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
            "serverUrl": "http://127.0.0.1:18099",
            "ready": false,
            "mcpToolset": "agent",
        });
        tokio::fs::write(dir.join("instance.lock"), format!("{body}\n"))
            .await
            .unwrap();
        let lock = DaemonInstanceLock::acquire(Some(home.path()), "http://127.0.0.1:18099")
            .await
            .unwrap();
        assert_ne!(lock.record().instance_token, "stale");
        lock.release().await;
    }

    /// Regression (#91): a lock file that exists but does not parse yet
    /// (a concurrent acquirer caught between create and write, or a
    /// heartbeat caught mid-truncate) is a live holder, not a stale one.
    /// It must not be removed or taken over.
    #[tokio::test]
    async fn unparseable_lock_is_treated_as_live() {
        for contents in ["", "not json\n"] {
            let home = home();
            tokio::fs::write(home.path().join("instance.lock"), contents)
                .await
                .unwrap();
            let result =
                DaemonInstanceLock::acquire(Some(home.path()), "http://127.0.0.1:18099").await;
            assert!(result.is_err(), "unparseable lock must refuse acquire");
            let kept = tokio::fs::read_to_string(home.path().join("instance.lock"))
                .await
                .unwrap();
            assert_eq!(kept, contents, "unparseable lock must be left in place");
        }
    }

    /// Regression (#91): an update (heartbeat/`mark_ready` write) must not
    /// overwrite a lock that no longer parses as ours; with non-atomic
    /// writes that window clobbered the concurrent holder's record.
    #[tokio::test]
    async fn write_does_not_overwrite_unparseable_lock() {
        let home = home();
        let mut lock = DaemonInstanceLock::acquire(Some(home.path()), "http://127.0.0.1:18099")
            .await
            .unwrap();
        tokio::fs::write(home.path().join("instance.lock"), "")
            .await
            .unwrap();
        lock.write().await;
        let kept = tokio::fs::read_to_string(home.path().join("instance.lock"))
            .await
            .unwrap();
        assert_eq!(kept, "", "update must not clobber an unparseable lock");
        // The file no longer names us, so release must also leave it alone.
        lock.release().await;
        assert!(home.path().join("instance.lock").exists());
        tokio::fs::remove_file(home.path().join("instance.lock"))
            .await
            .unwrap();
    }

    /// Regression (#91): exactly one of many concurrent acquirers wins,
    /// and the published lock is complete (parseable) from the start.
    #[tokio::test]
    async fn concurrent_acquirers_have_a_single_winner() {
        let home = home();
        let dir = home.path().to_owned();
        let mut handles = Vec::new();
        for _ in 0..8 {
            let dir = dir.clone();
            handles.push(tokio::spawn(async move {
                DaemonInstanceLock::acquire(Some(dir.as_path()), "http://127.0.0.1:18099").await
            }));
        }
        let mut winners = Vec::new();
        for handle in handles {
            if let Ok(lock) = handle.await.unwrap() {
                winners.push(lock);
            }
        }
        assert_eq!(winners.len(), 1, "exactly one acquirer may hold the lock");
        assert!(
            read_record(&lock_path(Some(dir.as_path()))).await.is_some(),
            "published lock must be complete and parseable"
        );
        // Publishing must not leave temp files behind.
        let mut entries = tokio::fs::read_dir(&dir).await.unwrap();
        let mut names = Vec::new();
        while let Some(entry) = entries.next_entry().await.unwrap() {
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
        assert_eq!(names, ["instance.lock".to_owned()]);
        for lock in winners {
            lock.release().await;
        }
    }

    #[cfg(unix)]
    async fn write_lock(home: &Path, pid: u32, token: &str) {
        // `daemon_dir(home)` is `home` itself; the lock sits directly in it.
        let dir = home.to_owned();
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let body = serde_json::json!({
            "pid": pid,
            "hostname": machine_name(),
            "instanceToken": token,
            "startedAt": 0,
            "updatedAt": 0,
            "serverUrl": "http://127.0.0.1:9",
            "ready": false,
        });
        tokio::fs::write(dir.join("instance.lock"), format!("{body}\n"))
            .await
            .unwrap();
    }

    #[cfg(unix)]
    fn record_for(pid: u32, token: &str) -> DaemonInstanceRecord {
        DaemonInstanceRecord {
            pid,
            hostname: machine_name(),
            instance_token: token.to_owned(),
            server_url: "http://127.0.0.1:9".to_owned(),
            ready: false,
        }
    }

    /// Regression (#67): the lock record was replaced after it was read, so
    /// the recorded PID may now belong to an unrelated (reused-PID) process.
    /// `force_stop` must not signal it.
    #[cfg(unix)]
    #[tokio::test]
    async fn force_stop_does_not_signal_when_record_was_replaced() {
        let home = home();
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let pid = child.id();
        write_lock(home.path(), pid, "original").await;
        write_lock(home.path(), pid, "replacement").await;
        let result = force_stop(
            Some(home.path()),
            &record_for(pid, "original"),
            Duration::from_millis(100),
        )
        .await;
        let still_alive = child.try_wait().unwrap().is_none();
        let record_kept = read_record(&lock_path(Some(home.path())))
            .await
            .is_some_and(|current| current.instance_token == "replacement");
        let _ = child.kill();
        let _ = child.wait();
        assert!(result.is_err(), "mismatched record must refuse to signal");
        assert!(still_alive, "unrelated process must not be signalled");
        assert!(record_kept, "replacement record must be left in place");
    }

    /// Regression (#67), escalation arm: record replaced while waiting out
    /// the `SIGTERM` grace period must block the `SIGKILL`.
    #[cfg(unix)]
    #[tokio::test]
    async fn force_stop_does_not_escalate_when_record_changes_during_grace() {
        let home = home();
        let mut child = std::process::Command::new("sh")
            .args(["-c", "trap '' TERM; while true; do sleep 1; done"])
            .spawn()
            .unwrap();
        let pid = child.id();
        write_lock(home.path(), pid, "original").await;
        let replacer_home = home.path().to_owned();
        let replacer = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            write_lock(&replacer_home, pid, "replacement").await;
        });
        let result = force_stop(
            Some(home.path()),
            &record_for(pid, "original"),
            Duration::from_millis(300),
        )
        .await;
        replacer.await.unwrap();
        let still_alive = child.try_wait().unwrap().is_none();
        let _ = child.kill();
        let _ = child.wait();
        assert!(result.is_err(), "mismatched record must refuse to signal");
        assert!(still_alive, "SIGKILL must not hit a replaced record's PID");
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
