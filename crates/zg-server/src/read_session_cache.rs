//! Idle-TTL read-session cache: the sole owner of cached read sessions.
//!
//! Mirrors `../zvec-grep/src/daemon/workspace-read-session-cache.ts`.
//! The phase-B facade exposes explicit `open_read_session` /
//! `close_read_session` with no timers; this cache is where idle eviction
//! lives (it is the only layer with a runtime).
//!
//! Concurrency contract: the state mutex is only ever held for brief
//! observe/publish steps, never across `open()` or the search itself.
//! Cold opens are single-flighted on a dedicated open mutex and awaited
//! outside the state lock, so a slow open never parks warm readers,
//! `snapshot`, or `close`. The resident handle is checked out of the slot
//! for the duration of one search and restored afterwards; a rival that
//! finds the slot empty parks on a `Notify` (off-lock) until the restore
//! lands. `close()` only flips the closed generation under the brief lock:
//! it never waits for a running search (the search owner drops the
//! checked-out handle on restore) or a slow open (the opener
//! discards-and-closes its stale handle when it observes the closed
//! generation after `open()` completes).
//! Idle eviction is a single abortable sleeper per cache entry, aborted and
//! replaced on new read activity instead of accumulating one task per read.

use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;
use tokio::sync::Notify;
use zg_core::error::EngineError;
use zg_core::types::UnixMillis;

/// Idle TTL before a quiet session closes; mirrors the TS 60 s default.
pub const DEFAULT_READ_SESSION_IDLE_TTL: Duration = Duration::from_secs(60);

/// Sealed: only the daemon's own session handles implement this, so adding
/// methods is not a breaking change. (`pub(crate)`: the implementor in
/// `backend::actor` is a sibling module, not a child.)
pub(crate) mod private {
    pub trait Sealed {}
}

/// A read handle the cache can own. `Send` is required: the cache holds it
/// across awaits inside a `tokio::sync::Mutex` and moves it between the
/// cache slot and the checking-out task. Handles are deliberately *not*
/// shared between tasks (`Sync` is not required): one search owns the
/// checked-out handle at a time, so `operation` runs with no lock held.
#[async_trait::async_trait]
pub trait ClosableHandle: private::Sealed + Send {
    /// Releases the handle (closes storage, returns leases).
    async fn close(self);
}

/// Failure to run a cached read.
#[derive(Debug)]
pub enum SessionError {
    /// The cache is closed.
    Closed,
    /// Opening the session failed; the error is preserved verbatim.
    Open(EngineError),
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Closed => write!(f, "workspace read session cache is closed"),
            Self::Open(error) => write!(f, "failed to open workspace read session: {error}"),
        }
    }
}

impl std::error::Error for SessionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Closed => None,
            Self::Open(error) => Some(error),
        }
    }
}

/// How to open a fresh handle on a cold cache. Failures are already
/// classified: a closed pool (or store) reports [`SessionError::Closed`]
/// instead of forcing a synthetic engine code.
pub type OpenSession<T> =
    Arc<dyn Fn() -> BoxFuture<'static, Result<T, SessionError>> + Send + Sync>;

/// Cache snapshot for status reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionCacheSnapshot {
    /// True while a handle is resident.
    pub open: bool,
    /// Readers currently inside `with_read`.
    pub active_readers: usize,
}

struct State<T> {
    /// Resident handle. Empty while cold, closed, evicted — or while
    /// checked out by exactly one running search (`active_readers > 0`
    /// with an empty slot means "checked out", not "cold").
    handle: Option<T>,
    active_readers: usize,
    last_read_ms: u64,
    idle_seq: u64,
    closed: bool,
    /// The single pending idle-TTL sleeper, if any. Aborted and replaced
    /// on new read activity so sleepers never accumulate per read.
    idle_task: Option<tokio::task::JoinHandle<()>>,
}

struct Shared<T> {
    state: tokio::sync::Mutex<State<T>>,
    /// Single-flights cold opens. Held across `open().await` but never
    /// across the state lock or a search; `close` never takes it, so a
    /// slow open cannot park warm readers or `close`.
    open_serial: tokio::sync::Mutex<()>,
    /// Signalled on every restore and on `close` so tasks parked while the
    /// handle is checked out re-observe without polling or spinning.
    restored: Notify,
}
pub struct WorkspaceReadSessionCache<T> {
    shared: Arc<Shared<T>>,
    open: OpenSession<T>,
    idle_ttl: Duration,
}

impl<T> Clone for WorkspaceReadSessionCache<T> {
    fn clone(&self) -> Self {
        Self {
            shared: Arc::clone(&self.shared),
            open: Arc::clone(&self.open),
            idle_ttl: self.idle_ttl,
        }
    }
}

impl<T: ClosableHandle + 'static> WorkspaceReadSessionCache<T> {
    /// Empty cache that opens handles with `open` and evicts them after
    /// `idle_ttl` of quiet time (`None` selects the 60 s default; zero
    /// closes eagerly once readers drain).
    pub fn new(open: OpenSession<T>, idle_ttl: Option<Duration>) -> Self {
        Self {
            shared: Arc::new(Shared {
                state: tokio::sync::Mutex::new(State {
                    handle: None,
                    active_readers: 0,
                    // Clock-unavailable direction: write-only diagnostic
                    // stamp; idle close is `idle_seq`-guarded, so the `0`
                    // fallback is inert.
                    last_read_ms: UnixMillis::now_ms_or(0),
                    idle_seq: 0,
                    closed: false,
                    idle_task: None,
                }),
                open_serial: tokio::sync::Mutex::new(()),
                restored: Notify::new(),
            }),
            open,
            idle_ttl: idle_ttl.unwrap_or(DEFAULT_READ_SESSION_IDLE_TTL),
        }
    }

    /// Runs `operation` against the cached handle, opening it first when
    /// cold. The state mutex is held only for brief observe/publish steps:
    /// the cold open runs outside it (single-flighted on the open mutex),
    /// and the operation runs on a handle checked out of the slot with no
    /// lock held, so it must never re-enter the cache. Searches serialize
    /// per session (handles are `Send` but not `Sync`); opens, closes, and
    /// snapshots never park behind a running search.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::Closed`] when the cache is closed, or
    /// [`SessionError::Open`] when opening the handle fails.
    pub async fn with_read<R>(&self, operation: impl FnOnce(&T) -> R) -> Result<R, SessionError> {
        let handle: T = loop {
            // Registered before observing so a restore/`close` that lands
            // between the observe and the park still wakes us (no lost
            // wakeup); the future does nothing until first polled.
            let mut parked = Box::pin(self.shared.restored.notified());
            let _ = parked.as_mut().enable();
            enum Next<T> {
                Ready(T),
                Open,
                Wait,
            }
            let next = {
                let mut state = self.shared.state.lock().await;
                if state.closed {
                    return Err(SessionError::Closed);
                }
                if let Some(handle) = state.handle.take() {
                    state.active_readers += 1;
                    state.idle_seq += 1;
                    if let Some(task) = state.idle_task.take() {
                        task.abort();
                    }
                    Next::Ready(handle)
                } else if state.active_readers > 0 {
                    // Checked out by another search: park off-lock below.
                    Next::Wait
                } else {
                    Next::Open
                }
            };
            match next {
                Next::Ready(handle) => break handle,
                Next::Open => {
                    if let Some(handle) = self.open_checked_out().await? {
                        break handle;
                    }
                    // A rival published (or a close landed, which returns
                    // above as `Err`): re-observe instead of opening.
                }
                Next::Wait => {
                    parked.await;
                }
            }
        };
        self.run_checked_out(handle, operation).await
    }

    /// Single-flight cold open returning the handle already checked out.
    /// The state lock is released across `open().await`; `Ok(None)` means
    /// the world changed while queueing for the open turn (re-observe).
    async fn open_checked_out(&self) -> Result<Option<T>, SessionError> {
        let _permit = self.shared.open_serial.lock().await;
        {
            let state = self.shared.state.lock().await;
            if state.closed {
                return Err(SessionError::Closed);
            }
            if state.handle.is_some() || state.active_readers > 0 {
                return Ok(None);
            }
        }
        // Cold: open with the state lock released (only the open turn is
        // held) so warm readers, snapshots, and `close` never park here.
        let open = Arc::clone(&self.open);
        let fresh = open().await?;
        let mut state = self.shared.state.lock().await;
        if state.closed {
            // `close` won the race while we opened: discard-and-close the
            // stale handle instead of publishing it.
            drop(state);
            drop(_permit);
            fresh.close().await;
            return Err(SessionError::Closed);
        }
        state.active_readers += 1;
        state.idle_seq += 1;
        if let Some(task) = state.idle_task.take() {
            task.abort();
        }
        // The slot stays empty while checked out; the restore publishes.
        // (A rival publisher is impossible under the open turn, and a
        // restore needs a checkout, which needs a resident handle.)
        Ok(Some(fresh))
    }

    /// Runs `operation` with no lock held, then restores (or drops, when
    /// closed) the checked-out handle.
    async fn run_checked_out<R>(
        &self,
        handle: T,
        operation: impl FnOnce(&T) -> R,
    ) -> Result<R, SessionError> {
        let output = operation(&handle);
        let mut state = self.shared.state.lock().await;
        state.active_readers -= 1;
        // Same direction as construction: diagnostic only, seq-guarded.
        state.last_read_ms = UnixMillis::now_ms_or(0);
        let drained = state.active_readers == 0;
        let seq = state.idle_seq;
        if state.closed {
            // The cache died mid-search; `close()` already returned without
            // waiting for us. Drop the handle instead of restoring it.
            drop(state);
            handle.close().await;
            return Ok(output);
        }
        // Restore for the next checkout and wake one parked rival. A
        // displaced resident is impossible (the slot is empty while checked
        // out); if one ever appears, close it rather than leak it.
        let displaced = state.handle.replace(handle);
        drop(state);
        self.shared.restored.notify_one();
        if let Some(stale) = displaced {
            stale.close().await;
        }
        if drained {
            self.schedule_idle_close(seq).await;
        }
        Ok(output)
    }

    /// Current cache state.
    pub async fn snapshot(&self) -> SessionCacheSnapshot {
        let state = self.shared.state.lock().await;
        SessionCacheSnapshot {
            open: state.handle.is_some(),
            active_readers: state.active_readers,
        }
    }

    /// Closes the cache without parking behind a running search or a slow
    /// open: flips the closed generation under a brief lock, aborts the
    /// idle sleeper, and closes the resident handle if one is home. A
    /// checked-out handle is dropped by its search on restore; an
    /// in-flight open discards-and-closes its stale handle once it
    /// observes the closed generation. Parked rivals are woken so none
    /// sleeps past the close.
    pub async fn close(&self) {
        let taken = {
            let mut state = self.shared.state.lock().await;
            if state.closed {
                return;
            }
            state.closed = true;
            state.idle_seq += 1;
            if let Some(task) = state.idle_task.take() {
                task.abort();
            }
            state.handle.take()
        };
        self.shared.restored.notify_waiters();
        if let Some(handle) = taken {
            handle.close().await;
        }
    }

    async fn schedule_idle_close(&self, seq: u64) {
        if self.idle_ttl.is_zero() {
            let mut state = self.shared.state.lock().await;
            if state.closed || state.active_readers != 0 {
                return;
            }
            if let Some(handle) = state.handle.take() {
                drop(state);
                handle.close().await;
            }
            return;
        }
        // Abort-and-replace under a single lock acquisition: at most one
        // sleeper is ever pending per entry, so reads cannot leak O(reads)
        // tasks no matter how many searches run.
        let mut state = self.shared.state.lock().await;
        if state.closed
            || state.active_readers != 0
            || state.handle.is_none()
            || state.idle_seq != seq
        {
            return;
        }
        if let Some(previous) = state.idle_task.take() {
            previous.abort();
        }
        let shared = Arc::clone(&self.shared);
        let ttl = self.idle_ttl;
        state.idle_task = Some(tokio::spawn(async move {
            tokio::time::sleep(ttl).await;
            let taken = {
                let mut state = shared.state.lock().await;
                if state.closed || state.active_readers != 0 || state.idle_seq != seq {
                    return;
                }
                // Still the current generation, so this sleeper is the
                // installed one: clear the slot before evicting.
                state.idle_task = None;
                state.handle.take()
            };
            if let Some(handle) = taken {
                handle.close().await;
            }
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct TestHandle {
        id: usize,
        closes: Arc<AtomicUsize>,
    }

    impl private::Sealed for TestHandle {}
    #[async_trait::async_trait]
    impl ClosableHandle for TestHandle {
        async fn close(self) {
            self.closes.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn cache(
        ttl: Duration,
        closes: &Arc<AtomicUsize>,
        opens: &Arc<AtomicUsize>,
    ) -> WorkspaceReadSessionCache<TestHandle> {
        let closes_clone = closes.clone();
        let opens_clone = opens.clone();
        WorkspaceReadSessionCache::new(
            Arc::new(move || {
                let closes = closes_clone.clone();
                let opens = opens_clone.clone();
                Box::pin(async move {
                    let id = opens.fetch_add(1, Ordering::SeqCst);
                    Ok(TestHandle { id, closes })
                }) as BoxFuture<'static, Result<TestHandle, SessionError>>
            }),
            Some(ttl),
        )
    }

    #[tokio::test]
    async fn reuses_the_open_handle() {
        let closes = Arc::new(AtomicUsize::new(0));
        let opens = Arc::new(AtomicUsize::new(0));
        let cache = cache(Duration::from_secs(3600), &closes, &opens);
        let first = cache.with_read(|handle| handle.id).await.unwrap();
        let second = cache.with_read(|handle| handle.id).await.unwrap();
        assert_eq!(first, second);
        assert_eq!(opens.load(Ordering::SeqCst), 1);
        assert_eq!(
            cache.snapshot().await,
            SessionCacheSnapshot {
                open: true,
                active_readers: 0
            }
        );
        cache.close().await;
    }

    #[tokio::test]
    async fn idle_ttl_closes_a_quiet_session() {
        let closes = Arc::new(AtomicUsize::new(0));
        let opens = Arc::new(AtomicUsize::new(0));
        let cache = cache(Duration::from_millis(20), &closes, &opens);
        cache.with_read(|_| {}).await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(closes.load(Ordering::SeqCst), 1);
        assert!(!cache.snapshot().await.open);
        // Next read reopens transparently.
        cache.with_read(|_| {}).await.unwrap();
        assert_eq!(opens.load(Ordering::SeqCst), 2);
        cache.close().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn close_does_not_park_behind_a_running_search() {
        use std::sync::atomic::AtomicBool;
        let closes = Arc::new(AtomicUsize::new(0));
        let opens = Arc::new(AtomicUsize::new(0));
        let cache = cache(Duration::from_secs(3600), &closes, &opens);
        let entered = Arc::new(Notify::new());
        let entered_clone = entered.clone();
        let proceed = Arc::new(AtomicBool::new(false));
        let proceed_clone = proceed.clone();
        let cache_clone = cache.clone();
        // The search spins (without touching the cache) until the main
        // task lets it finish. If `close` parked behind the search, it
        // would never return and the test would hang.
        let reader = tokio::spawn(async move {
            cache_clone
                .with_read(|_| {
                    entered_clone.notify_one();
                    while !proceed_clone.load(Ordering::SeqCst) {
                        std::hint::spin_loop();
                    }
                    7
                })
                .await
                .unwrap()
        });
        entered.notified().await;
        cache.close().await;
        // `close` returned while the search is still running: the
        // checked-out handle is dropped by the search on restore.
        assert!(!reader.is_finished());
        proceed.store(true, Ordering::SeqCst);
        assert_eq!(reader.await.unwrap(), 7);
        assert_eq!(closes.load(Ordering::SeqCst), 1);
        assert!(matches!(
            cache.with_read(|_| {}).await,
            Err(SessionError::Closed)
        ));
    }

    #[tokio::test]
    async fn concurrent_cold_opens_single_flight() {
        let closes = Arc::new(AtomicUsize::new(0));
        let opens = Arc::new(AtomicUsize::new(0));
        let closes_clone = closes.clone();
        let opens_clone = opens.clone();
        let cache = WorkspaceReadSessionCache::new(
            Arc::new(move || {
                let closes = closes_clone.clone();
                let opens = opens_clone.clone();
                Box::pin(async move {
                    // Slow open: rivals must share the single flight instead
                    // of each opening their own handle.
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    let id = opens.fetch_add(1, Ordering::SeqCst);
                    Ok(TestHandle { id, closes })
                }) as BoxFuture<'static, Result<TestHandle, SessionError>>
            }),
            Some(Duration::from_secs(3600)),
        );
        let mut readers = Vec::new();
        for _ in 0..8 {
            let cache_clone = cache.clone();
            readers.push(tokio::spawn(async move {
                cache_clone.with_read(|handle| handle.id).await.unwrap()
            }));
        }
        let mut ids = Vec::new();
        for reader in readers {
            ids.push(reader.await.unwrap());
        }
        assert!(
            ids.first()
                .is_some_and(|first| ids.iter().all(|id| id == first))
        );
        assert_eq!(opens.load(Ordering::SeqCst), 1);
        cache.close().await;
        assert_eq!(closes.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn rapid_reads_leave_a_single_idle_sleeper() {
        let closes = Arc::new(AtomicUsize::new(0));
        let opens = Arc::new(AtomicUsize::new(0));
        let cache = cache(Duration::from_millis(30), &closes, &opens);
        for _ in 0..50 {
            cache.with_read(|_| {}).await.unwrap();
        }
        // New activity aborts and replaces the pending sleeper, so the
        // session is never evicted mid-burst and exactly one close fires.
        assert_eq!(opens.load(Ordering::SeqCst), 1);
        assert_eq!(closes.load(Ordering::SeqCst), 0);
        assert!(cache.snapshot().await.open);
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(closes.load(Ordering::SeqCst), 1);
        assert!(!cache.snapshot().await.open);
        cache.close().await;
    }

    #[tokio::test]
    async fn close_during_open_discards_the_stale_handle() {
        let closes = Arc::new(AtomicUsize::new(0));
        let opens = Arc::new(AtomicUsize::new(0));
        let opening = Arc::new(Notify::new());
        let opening_clone = opening.clone();
        let closes_clone = closes.clone();
        let opens_clone = opens.clone();
        let cache = WorkspaceReadSessionCache::new(
            Arc::new(move || {
                let closes = closes_clone.clone();
                let opens = opens_clone.clone();
                let opening = opening_clone.clone();
                Box::pin(async move {
                    opening.notify_one();
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    let id = opens.fetch_add(1, Ordering::SeqCst);
                    Ok(TestHandle { id, closes })
                }) as BoxFuture<'static, Result<TestHandle, SessionError>>
            }),
            Some(Duration::from_secs(3600)),
        );
        let cache_clone = cache.clone();
        let reader = tokio::spawn(async move { cache_clone.with_read(|_| {}).await });
        // Wait until the open is in flight, then close wins the race: it
        // returns without parking behind the slow open.
        opening.notified().await;
        cache.close().await;
        assert!(matches!(reader.await.unwrap(), Err(SessionError::Closed)));
        assert_eq!(opens.load(Ordering::SeqCst), 1);
        // The stale handle is discarded-and-closed, never published.
        assert_eq!(closes.load(Ordering::SeqCst), 1);
        assert!(!cache.snapshot().await.open);
    }
}
