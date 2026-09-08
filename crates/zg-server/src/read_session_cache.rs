//! Idle-TTL read-session cache: the sole owner of cached read sessions.
//!
//! Mirrors `../zvec-grep/src/daemon/workspace-read-session-cache.ts`.
//! The phase-B facade exposes explicit `open_read_session` /
//! `close_read_session` with no timers; this cache is where idle eviction
//! lives (it is the only layer with a runtime). Default serialization of
//! operations matches TS (`serializeOperations` defaults to true): `with_read`
//! holds one mutex across open + operation, so opens are single-flight and
//! reads never interleave on one session.

use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;
use zg_core::error::EngineError;

/// Idle TTL before a quiet session closes; mirrors the TS 60 s default.
pub const DEFAULT_READ_SESSION_IDLE_TTL: Duration = Duration::from_secs(60);

/// A read handle the cache can own. `Send` is required: the cache holds it
/// across awaits inside a `tokio::sync::Mutex`.
#[async_trait::async_trait]
pub trait ClosableHandle: Send {
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

impl std::error::Error for SessionError {}

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
    handle: Option<T>,
    active_readers: usize,
    last_read_ms: u64,
    idle_seq: u64,
    closed: bool,
}

struct Shared<T> {
    state: tokio::sync::Mutex<State<T>>,
}

/// Idle-TTL cache over one read handle. `Clone` shares one cache.
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
                    last_read_ms: now_ms(),
                    idle_seq: 0,
                    closed: false,
                }),
            }),
            open,
            idle_ttl: idle_ttl.unwrap_or(DEFAULT_READ_SESSION_IDLE_TTL),
        }
    }

    /// Runs `operation` against the cached handle, opening it first when
    /// cold. Operations serialize (mirrors TS default); the operation
    /// itself is synchronous and runs while the cache mutex is held, so it
    /// must be short and must never re-enter the cache.
    pub async fn with_read<R>(&self, operation: impl FnOnce(&T) -> R) -> Result<R, SessionError> {
        let mut state = self.shared.state.lock().await;
        if state.closed {
            return Err(SessionError::Closed);
        }
        if state.handle.is_none() {
            let open = Arc::clone(&self.open);
            // Awaited while holding the mutex: opens are single-flight and
            // a concurrent `close` waits for us instead of racing us.
            let handle = open().await?;
            state.handle = Some(handle);
        }
        state.active_readers += 1;
        state.idle_seq += 1;
        let Some(handle) = state.handle.as_ref() else {
            unreachable!("handle is present: opened above or already cached");
        };
        let output = operation(handle);
        state.active_readers -= 1;
        state.last_read_ms = now_ms();
        let seq = state.idle_seq;
        let reads_drained = state.active_readers == 0;
        drop(state);
        if reads_drained {
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

    /// Closes the cache: in-flight reads (which hold the mutex) finish
    /// first, then the resident handle closes.
    pub async fn close(&self) {
        let mut state = self.shared.state.lock().await;
        state.closed = true;
        state.idle_seq += 1;
        if let Some(handle) = state.handle.take() {
            drop(state);
            handle.close().await;
        }
    }

    async fn schedule_idle_close(&self, seq: u64) {
        if self.idle_ttl.is_zero() {
            let mut state = self.shared.state.lock().await;
            if state.active_readers == 0 {
                if let Some(handle) = state.handle.take() {
                    drop(state);
                    handle.close().await;
                }
            }
            return;
        }
        let shared = Arc::clone(&self.shared);
        let ttl = self.idle_ttl;
        tokio::spawn(async move {
            tokio::time::sleep(ttl).await;
            let mut state = shared.state.lock().await;
            if !state.closed && state.active_readers == 0 && state.idle_seq == seq {
                if let Some(handle) = state.handle.take() {
                    drop(state);
                    handle.close().await;
                }
            }
        });
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
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct TestHandle {
        id: usize,
        closes: Arc<AtomicUsize>,
    }

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

    #[tokio::test]
    async fn close_waits_for_in_flight_reads() {
        let closes = Arc::new(AtomicUsize::new(0));
        let opens = Arc::new(AtomicUsize::new(0));
        let cache = cache(Duration::from_secs(3600), &closes, &opens);
        let entered = Arc::new(tokio::sync::Notify::new());
        let entered_clone = entered.clone();
        let cache_clone = cache.clone();
        // Operations are synchronous and short by contract (they run while
        // the cache mutex is held); `close` waits for the mutex, so the
        // in-flight read provably finishes first.
        let reader = tokio::spawn(async move {
            cache_clone
                .with_read(|_| {
                    entered_clone.notify_one();
                    7
                })
                .await
                .unwrap()
        });
        entered.notified().await;
        cache.close().await;
        assert_eq!(reader.await.unwrap(), 7);
        assert_eq!(closes.load(Ordering::SeqCst), 1);
        assert!(matches!(
            cache.with_read(|_| {}).await,
            Err(SessionError::Closed)
        ));
    }
}
