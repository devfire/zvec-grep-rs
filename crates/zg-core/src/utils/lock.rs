//! Directory-based read/write locks with stale reclamation.
//!
//! Ports `utils/lock.ts`: a write lock is an exclusive directory
//! `<path>.write` containing `lock.json`; readers register directories under
//! `<path>.readers/<pid>-<token>/`. Stale locks (age over `stale_ms`, or same
//! host with a dead PID) are reclaimed. Acquisition is fail-fast: two
//! attempts, then `LOCK.BUSY`.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::error::{EngineError, EngineResult, codes};
// Local code helpers avoid clashing with the crate-level `codes` module.
use crate::types::UnixMillis;

/// Default stale threshold: 6 hours.
pub const DEFAULT_STALE_MS: i64 = 6 * 60 * 60 * 1000;
/// Attempts before reporting `LOCK.BUSY`.
const ACQUIRE_ATTEMPTS: usize = 2;

const LOCK_FILE_NAME: &str = "lock.json";
const WRITE_SUFFIX: &str = ".write";
const READERS_SUFFIX: &str = ".readers";

/// Lock ownership record persisted inside every lock directory.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileLockInfo {
    pub token: String,
    pub pid: u32,
    pub hostname: String,
    pub started_at: UnixMillis,
    pub operation: String,
}

/// Which kind of lock to acquire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockMode {
    Read,
    Write,
}

/// Options for [`acquire_read_write_lock`].
#[derive(Debug, Clone)]
pub struct LockOptions<'a> {
    pub operation: &'a str,
    pub stale_ms: Option<i64>,
}

impl<'a> LockOptions<'a> {
    pub fn new(operation: &'a str) -> Self {
        Self {
            operation,
            stale_ms: None,
        }
    }
}

/// A held lock; released on drop (best-effort) or explicitly via [`Guard::release`].
#[derive(Debug)]
pub enum Guard {
    Write { dir: PathBuf, token: String },
    Read { dir: PathBuf, token: String },
}

impl Guard {
    /// Releases the lock; only removes the directory when the recorded token
    /// still matches ours (a reclaimed lock belongs to its new owner).
    pub fn release(self) {
        let _ = self.release_result();
    }

    fn release_result(&self) -> EngineResult<()> {
        let (dir, token) = match self {
            Self::Write { dir, token } | Self::Read { dir, token } => (dir, token),
        };
        // Only remove when lock.json still carries our token; on missing or
        // replaced owner info the lock belongs to someone else.
        if let Ok(text) = fs::read_to_string(dir.join(LOCK_FILE_NAME))
            && let Ok(info) = serde_json::from_str::<FileLockInfo>(&text)
            && info.token == *token
        {
            let _ = fs::remove_dir_all(dir);
        }
        Ok(())
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        let _ = self.release_result();
    }
}

/// Acquires a read or write lock at `lock_path`.
pub fn acquire_read_write_lock(
    lock_path: &Path,
    mode: LockMode,
    options: &LockOptions<'_>,
) -> EngineResult<Guard> {
    match mode {
        LockMode::Write => acquire_write_lock(lock_path, options),
        LockMode::Read => acquire_read_lock(lock_path, options),
    }
}

/// Fails fast when another writer holds `<path>.write`.
pub fn assert_no_write_lock(lock_path: &Path, operation: &str) -> EngineResult<()> {
    let write_dir = write_dir(lock_path);
    match try_reclaim_stale(&write_dir, DEFAULT_STALE_MS) {
        Reclaim::Absent | Reclaim::Reclaimed => Ok(()),
        Reclaim::Held(info) => Err(busy_error(&write_dir, operation, &info)),
    }
}

fn acquire_write_lock(lock_path: &Path, options: &LockOptions<'_>) -> EngineResult<Guard> {
    let stale_ms = options.stale_ms.unwrap_or(DEFAULT_STALE_MS);
    let write_dir = write_dir(lock_path);
    for _ in 0..ACQUIRE_ATTEMPTS {
        if let Some(parent) = write_dir.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let token = uuid::Uuid::new_v4().to_string();
        let info = FileLockInfo {
            token: token.clone(),
            pid: std::process::id(),
            hostname: hostname(),
            started_at: UnixMillis::now(),
            operation: options.operation.to_owned(),
        };
        match fs::create_dir(&write_dir) {
            Ok(()) => {
                if write_lock_file(&write_dir, &info).is_ok() {
                    // No active readers may remain.
                    if let Some(reader) = first_active_reader(lock_path, stale_ms) {
                        let _ = fs::remove_dir_all(&write_dir);
                        return Err(busy_error(&write_dir, options.operation, &reader));
                    }
                    return Ok(Guard::Write {
                        dir: write_dir,
                        token,
                    });
                }
                let _ = fs::remove_dir_all(&write_dir);
                continue;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                match try_reclaim_stale(&write_dir, stale_ms) {
                    Reclaim::Reclaimed | Reclaim::Absent => continue,
                    Reclaim::Held(info) => {
                        return Err(busy_error(&write_dir, options.operation, &info));
                    }
                }
            }
            Err(error) => {
                return Err(EngineError::new(
                    local_codes::lock_unavailable(),
                    format!("failed to create lock at {}", write_dir.display()),
                )
                .with_context(format!("error={error}")));
            }
        }
    }
    Err(EngineError::new(codes::lock_busy(), "Index unavailable"))
}

fn acquire_read_lock(lock_path: &Path, options: &LockOptions<'_>) -> EngineResult<Guard> {
    let stale_ms = options.stale_ms.unwrap_or(DEFAULT_STALE_MS);
    let write_dir = write_dir(lock_path);
    for _ in 0..ACQUIRE_ATTEMPTS {
        match try_reclaim_stale(&write_dir, stale_ms) {
            Reclaim::Held(info) => return Err(busy_error(&write_dir, options.operation, &info)),
            Reclaim::Absent | Reclaim::Reclaimed => {}
        }
        let token = uuid::Uuid::new_v4().to_string();
        let info = FileLockInfo {
            token: token.clone(),
            pid: std::process::id(),
            hostname: hostname(),
            started_at: UnixMillis::now(),
            operation: options.operation.to_owned(),
        };
        let readers_root = lock_path.with_extension("readers");
        let reader_dir = readers_root.join(format!("{}-{}", std::process::id(), token));
        fs::create_dir_all(&reader_dir).map_err(|error| {
            EngineError::new(
                local_codes::lock_unavailable(),
                format!("failed to create reader lock at {}", reader_dir.display()),
            )
            .with_context(format!("error={error}"))
        })?;
        if write_lock_file(&reader_dir, &info).is_err() {
            let _ = fs::remove_dir_all(&reader_dir);
            continue;
        }
        // Re-check: a writer may have appeared between our checks.
        match try_reclaim_stale(&write_dir, stale_ms) {
            Reclaim::Held(_) => {
                let _ = fs::remove_dir_all(&reader_dir);
                // Retry once; the next iteration sees the writer and reports
                // BUSY if it is still active.
                continue;
            }
            Reclaim::Absent | Reclaim::Reclaimed => {}
        }
        return Ok(Guard::Read {
            dir: reader_dir,
            token,
        });
    }
    Err(EngineError::new(codes::lock_busy(), "Index unavailable"))
}

fn first_active_reader(lock_path: &Path, stale_ms: i64) -> Option<FileLockInfo> {
    let readers_root = PathBuf::from(format!("{}{READERS_SUFFIX}", lock_path.display()));
    let entries = fs::read_dir(&readers_root).ok()?;
    let mut earliest: Option<FileLockInfo> = None;
    for entry in entries.flatten() {
        let dir = entry.path();
        match try_reclaim_stale(&dir, stale_ms) {
            Reclaim::Held(info) => {
                if earliest
                    .as_ref()
                    .is_none_or(|current| info.started_at < current.started_at)
                {
                    earliest = Some(info);
                }
            }
            Reclaim::Absent | Reclaim::Reclaimed => {}
        }
    }
    earliest
}

enum Reclaim {
    Absent,
    Reclaimed,
    Held(FileLockInfo),
}

fn try_reclaim_stale(lock_dir: &Path, stale_ms: i64) -> Reclaim {
    if !lock_dir.exists() {
        return Reclaim::Absent;
    }
    let info = match read_lock_info(lock_dir) {
        Some(info) => info,
        None => {
            // Missing/invalid lock.json: judge staleness by directory mtime.
            return match dir_age_ms(lock_dir) {
                Some(age) if age > stale_ms => {
                    let _ = fs::remove_dir_all(lock_dir);
                    Reclaim::Reclaimed
                }
                _ => Reclaim::Held(FileLockInfo {
                    token: String::new(),
                    pid: 0,
                    hostname: String::new(),
                    started_at: UnixMillis(dir_mtime_ms(lock_dir).unwrap_or_default()),
                    operation: "unknown".to_owned(),
                }),
            };
        }
    };
    if is_stale(&info, stale_ms) {
        let _ = fs::remove_dir_all(lock_dir);
        return Reclaim::Reclaimed;
    }
    Reclaim::Held(info)
}

fn is_stale(info: &FileLockInfo, stale_ms: i64) -> bool {
    let age = UnixMillis::now().0 - info.started_at.0;
    if age > stale_ms {
        return true;
    }
    info.hostname == hostname() && !process_is_alive(info.pid)
}

fn read_lock_info(lock_dir: &Path) -> Option<FileLockInfo> {
    let text = fs::read_to_string(lock_dir.join(LOCK_FILE_NAME)).ok()?;
    serde_json::from_str(&text).ok()
}

fn write_lock_file(dir: &Path, info: &FileLockInfo) -> std::io::Result<()> {
    let body = format!(
        "{}\n",
        serde_json::to_string_pretty(info).map_err(io_other)?
    );
    fs::write(dir.join(LOCK_FILE_NAME), body)
}

fn io_other(error: serde_json::Error) -> std::io::Error {
    std::io::Error::other(error.to_string())
}

fn dir_age_ms(dir: &Path) -> Option<i64> {
    let mtime = dir_mtime_ms(dir)?;
    Some(UnixMillis::now().0 - mtime)
}

fn dir_mtime_ms(dir: &Path) -> Option<i64> {
    let metadata = fs::metadata(dir).ok()?;
    let modified = metadata.modified().ok()?;
    Some(
        modified
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or_default(),
    )
}

fn write_dir(lock_path: &Path) -> PathBuf {
    PathBuf::from(format!("{}{WRITE_SUFFIX}", lock_path.display()))
}

fn busy_error(_lock_dir: &Path, operation: &str, owner: &FileLockInfo) -> EngineError {
    let context = [
        "lock unavailable".to_owned(),
        format!("lock={}", owner.operation),
        format!("operation={operation}"),
        format!("ownerOperation={}", owner.operation),
        format!("ownerPid={}", owner.pid),
        format!("ownerHost={}", owner.hostname),
    ]
    .join("\n");
    EngineError::new(codes::lock_busy(), "Index unavailable").with_context(context)
}

pub fn hostname() -> String {
    hostname_impl()
}

#[cfg(unix)]
fn hostname_impl() -> String {
    fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|h| h.trim().to_owned())
        .ok()
        .or_else(|| std::env::var("HOSTNAME").ok())
        .unwrap_or_else(|| "localhost".to_owned())
}

#[cfg(not(unix))]
fn hostname_impl() -> String {
    std::env::var("COMPUTERNAME").unwrap_or_else(|_| "localhost".to_owned())
}

/// Process-liveness probe.
///
/// On Linux this is a `/proc/<pid>` existence check (safe Rust, no signal
/// needed: EPERM-style "exists but unpermitted" processes still have a
/// `/proc` entry). Other Unix targets use `kill(pid, 0)`; that call is the
/// sole `unsafe` in the crate, isolated here with a SAFETY justification.
pub fn process_is_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    #[cfg(target_os = "linux")]
    {
        Path::new(&format!("/proc/{pid}")).exists()
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        // SAFETY: `kill` with signal 0 performs no action and only reports
        // whether the pid exists / is permitted; `pid > 0` is checked above
        // and the return value plus errno are the only effects observed.
        #[allow(unsafe_code)]
        let result = unsafe { libc::kill(pid as i32, 0) };
        result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        true
    }
}

mod local_codes {
    use crate::error::EngineErrorCode;

    pub fn lock_unavailable() -> EngineErrorCode {
        EngineErrorCode::from_static("LOCK.UNAVAILABLE")
    }
}

#[allow(unused)]
fn unused_time_guard(_: SystemTime) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_lock_is_exclusive_then_released() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lock_path = dir.path().join("locks").join("home");
        let guard =
            acquire_read_write_lock(&lock_path, LockMode::Write, &LockOptions::new("index"))
                .expect("first acquire");
        let second =
            acquire_read_write_lock(&lock_path, LockMode::Write, &LockOptions::new("index"));
        assert!(second.is_err(), "second writer must fail");
        let busy = second.err().map(|e| e.code().qualified());
        assert_eq!(busy.as_deref(), Some("ZVEC_GREP.ENGINE.LOCK.BUSY"));
        guard.release();
        let again =
            acquire_read_write_lock(&lock_path, LockMode::Write, &LockOptions::new("index"));
        assert!(again.is_ok(), "lock must be free after release");
    }

    #[test]
    fn concurrent_readers_allowed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lock_path = dir.path().join("locks").join("home");
        let first =
            acquire_read_write_lock(&lock_path, LockMode::Read, &LockOptions::new("status"))
                .expect("reader 1");
        let second =
            acquire_read_write_lock(&lock_path, LockMode::Read, &LockOptions::new("status"))
                .expect("reader 2");
        drop(first);
        drop(second);
    }

    #[test]
    fn writer_blocks_while_reader_active() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lock_path = dir.path().join("locks").join("home");
        let reader =
            acquire_read_write_lock(&lock_path, LockMode::Read, &LockOptions::new("status"))
                .expect("reader");
        let writer =
            acquire_read_write_lock(&lock_path, LockMode::Write, &LockOptions::new("index"));
        assert!(writer.is_err());
        drop(reader);
        let writer =
            acquire_read_write_lock(&lock_path, LockMode::Write, &LockOptions::new("index"));
        assert!(writer.is_ok());
    }

    #[test]
    fn stale_dead_owner_reclaimed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lock_path = dir.path().join("locks").join("home");
        let write_dir = write_dir(&lock_path);
        fs::create_dir_all(&write_dir).expect("mkdir");
        let stale_info = FileLockInfo {
            token: "stale-token".to_owned(),
            pid: 4_000_000,
            hostname: hostname(),
            started_at: UnixMillis::now(),
            operation: "index".to_owned(),
        };
        write_lock_file(&write_dir, &stale_info).expect("lock.json");
        let guard =
            acquire_read_write_lock(&lock_path, LockMode::Write, &LockOptions::new("index"));
        assert!(guard.is_ok(), "dead same-host owner must be reclaimed");
    }

    #[test]
    fn assert_no_write_lock_reports_busy() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lock_path = dir.path().join("locks").join("home");
        let guard =
            acquire_read_write_lock(&lock_path, LockMode::Write, &LockOptions::new("index"))
                .expect("writer");
        let outcome = assert_no_write_lock(&lock_path, "status");
        assert!(outcome.is_err());
        drop(guard);
        assert!(assert_no_write_lock(&lock_path, "status").is_ok());
    }
}
