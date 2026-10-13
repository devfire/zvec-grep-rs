//! Filesystem watching: notify-based recursive watch coalesced to `ChangeSet`s.
//!
//! Mirrors `../zvec-grep/src/daemon/watch-manager.ts` (debounced flush,
//! max-wait backstop, `.gitignore` widening via [`ChangeSet`],
//! fail-open path filtering, full-reconcile on watcher errors). Two
//! divergences (see `docs/ts-divergence.md`):
//!
//! - TS walks the tree with per-directory watchers on Linux (Node's
//!   recursive watch ignores exclusions and exhausts inotify quotas).
//!   `notify`'s inotify backend registers recursive watches natively with
//!   exclusion filtering applied before recording, so one recursive watch
//!   suffices — no per-directory bookkeeping, no resume timers.
//! - `WatchManager` is owned by exactly one root actor task (never shared),
//!   so callbacks are plain `Arc<dyn Fn>` values, not negotiated factories.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher, recommended_watcher};
use tokio::sync::mpsc::{Receiver, Sender, channel};
use tokio::task::JoinHandle;
use zg_core::types::RootPath;

use crate::change_set::{
    ChangeKind, ChangeSet, ChangeSetOptions, ChangeSetSnapshot, MaxChangedPaths,
};
use crate::errors::DaemonError;

mod classify;
mod flush;
mod record;

#[cfg(test)]
mod tests;

use classify::{classify_loop, coalesce_overflow};
use flush::{flush_loop, flush_snapshot};
use record::{load_roots, record_raw, refresh_roots_blocking};

/// Debounce before a quiet batch flushes; mirrors TS `debounceMs`.
pub const DEFAULT_WATCH_DEBOUNCE: Duration = Duration::from_millis(750);

/// Backstop before a busy batch flushes anyway; mirrors TS `maxWaitMs`.
pub const DEFAULT_WATCH_MAX_WAIT: Duration = Duration::from_secs(5);

/// Bounded raw-event queue. Bursts beyond this conflate into the overflow
/// map (same-path events merge) and, past that, collapse to a single full
/// reconcile — so a checkout/build storm degrades to ~one re-index instead
/// of unbounded memory.
const EVENT_QUEUE_CAPACITY: usize = 1024;

/// Bounded cap for the conflated overflow map; see [`EVENT_QUEUE_CAPACITY`].
const OVERFLOW_CONFLATED_CAPACITY: usize = 1024;

/// Flush pokes carry no payload: at most one pending poke is ever needed.
/// Extra pokes while one is queued are duplicates and merge away.
const POKE_QUEUE_CAPACITY: usize = 1;

/// Why a batch flushed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchReason {
    /// Filesystem events.
    Watch,
    /// Reconciliation was requested (timer, error recovery, resume).
    Reconcile,
}

/// Receives a flushed batch. Synchronous: the actor sends it into its own
/// command queue (or the coordinator directly). Must never block.
pub type WatchChanges = Arc<dyn Fn(ChangeSetSnapshot, WatchReason) + Send + Sync>;

/// Current index roots for fail-open filtering (`None` tracks everything).
pub type RootPathsSource = Arc<dyn Fn() -> Vec<RootPath> + Send + Sync>;

/// Pending-state observer, mirroring TS `onPendingChange`.
pub type WatchPending = Arc<dyn Fn(bool) + Send + Sync>;

/// Options for [`WatchManager`].
pub struct WatchManagerOptions {
    /// Workspace root under watch.
    pub root: String,
    /// Quiet period before flush; defaults to 750 ms.
    pub debounce: Option<Duration>,
    /// Busy-batch backstop; defaults to 5 s.
    pub max_wait: Option<Duration>,
    /// Change-set budget; defaults to [`MaxChangedPaths::DEFAULT`].
    pub max_changed_paths: Option<MaxChangedPaths>,
    /// Batch receiver.
    pub on_changes: WatchChanges,
    /// Index roots for filtering; `None` tracks everything.
    pub get_root_paths: Option<RootPathsSource>,
    /// Pre-snapshotted index roots. When `Some`, [`WatchManager::new`]
    /// skips its blocking manifest read (the caller snapshotted off the
    /// async worker, #44); `None` keeps the warm-cache snapshot for
    /// synchronous and test callers.
    pub initial_roots: Option<Vec<RootPath>>,
    /// Pending-state observer.
    pub on_pending: Option<WatchPending>,
}
struct RawEvent {
    path: PathBuf,
    kind: Option<ChangeKind>,
}

struct Shared {
    root: String,
    changes: Mutex<ChangeSet>,
    reconcile_requested: std::sync::atomic::AtomicBool,
    closed: std::sync::atomic::AtomicBool,
    /// Set when roots may be stale (workspace change). The next blocking
    /// classify step refreshes [`Shared::roots_cache`] off the async loop.
    roots_stale: std::sync::atomic::AtomicBool,
    /// Last good roots snapshot. Refreshed on workspace-change signals, not
    /// per event, so the hot path never calls the [`RootPathsSource`]
    /// service (a blocking manifest read) per event.
    roots_cache: Mutex<Vec<RootPath>>,
    /// Conflated drops when the bounded event queue is full: path budgets
    /// stay bounded because same-path events merge here. Drained by the
    /// classify task alongside the channel.
    overflow: Mutex<HashMap<PathBuf, Option<ChangeKind>>>,
    on_changes: WatchChanges,
    on_pending: Option<WatchPending>,
    get_root_paths: Option<RootPathsSource>,
    flush_poke: Sender<()>,
}

impl Shared {
    fn poke(&self) {
        // Bounded with capacity one: `Full` means a poke is already queued,
        // so the duplicate merges away instead of growing a backlog.
        let _ = self.flush_poke.try_send(());
    }

    fn is_closed(&self) -> bool {
        self.closed.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn is_roots_stale(&self) -> bool {
        self.roots_stale.load(std::sync::atomic::Ordering::SeqCst)
    }
}

pub struct WatchManager {
    shared: Arc<Shared>,
    event_tx: Sender<RawEvent>,
    debounce: Duration,
    max_wait: Duration,
    watcher: Option<RecommendedWatcher>,
    tasks: Vec<JoinHandle<()>>,
}

impl WatchManager {
    /// Builds an idle manager; call [`WatchManager::start`] to watch.
    #[must_use]
    pub fn new(options: WatchManagerOptions) -> Self {
        let (flush_poke, flush_rx) = channel(POKE_QUEUE_CAPACITY);
        let (event_tx, event_rx) = channel(EVENT_QUEUE_CAPACITY);
        // One blocking snapshot up front so the synchronous `inject_event`
        // path filters with a warm cache; the async path refreshes off-loop.
        // A caller that already snapshotted off the async worker passes it
        // in (`initial_roots`) to skip the blocking read here (#44).
        let roots_cache = options
            .initial_roots
            .unwrap_or_else(|| load_roots(options.get_root_paths.as_ref()).unwrap_or_default());
        let shared = Arc::new(Shared {
            root: options.root.clone(),
            changes: Mutex::new(ChangeSet::new(ChangeSetOptions {
                root: Some(options.root.clone()),
                max_changed_paths: options.max_changed_paths,
            })),
            reconcile_requested: std::sync::atomic::AtomicBool::new(false),
            closed: std::sync::atomic::AtomicBool::new(false),
            roots_stale: std::sync::atomic::AtomicBool::new(false),
            roots_cache: Mutex::new(roots_cache),
            overflow: Mutex::new(HashMap::new()),
            on_changes: options.on_changes,
            on_pending: options.on_pending,
            get_root_paths: options.get_root_paths,
            flush_poke,
        });
        let debounce = options.debounce.unwrap_or(DEFAULT_WATCH_DEBOUNCE);
        let max_wait = options.max_wait.unwrap_or(DEFAULT_WATCH_MAX_WAIT);
        let mut manager = Self {
            shared: Arc::clone(&shared),
            event_tx,
            debounce,
            max_wait,
            watcher: None,
            tasks: Vec::new(),
        };
        manager.spawn(event_rx, flush_rx);
        manager
    }

    /// Starts the recursive notify watcher. Watching `.git` / `.zvec-grep-rs`
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError::IndexFailed`] when the watcher fails to start or watch the root.
    pub fn start(&mut self) -> Result<(), DaemonError> {
        if self.watcher.is_some() || self.shared.is_closed() {
            return Ok(());
        }
        let sender = self.event_tx.clone();
        let shared = Arc::clone(&self.shared);
        let mut watcher =
            recommended_watcher(move |result: Result<notify::Event, notify::Error>| {
                if let Ok(event) = result {
                    let kind = match event.kind {
                        EventKind::Create(_) => Some(ChangeKind::Created),
                        EventKind::Modify(_) => Some(ChangeKind::Changed),
                        EventKind::Remove(_) => Some(ChangeKind::Deleted),
                        EventKind::Any | EventKind::Access(_) | EventKind::Other => None,
                    };
                    for path in event.paths {
                        if shared.is_closed() {
                            return;
                        }
                        // Bounded queue: a full queue conflates into the
                        // overflow map (same-path events merge) instead
                        // of growing memory without bound.
                        match sender.try_send(RawEvent { path, kind }) {
                            Ok(()) => {}
                            Err(tokio::sync::mpsc::error::TrySendError::Full(event)) => {
                                coalesce_overflow(&shared, event);
                            }
                            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                                return;
                            }
                        }
                    }
                } else {
                    // A dead watcher cannot be trusted: reconcile everything.
                    shared
                        .changes
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .require_full_reconcile();
                    shared
                        .reconcile_requested
                        .store(true, std::sync::atomic::Ordering::SeqCst);
                    set_pending(&shared, true);
                    shared.poke();
                }
            })
            .map_err(|error| DaemonError::IndexFailed {
                message: format!("failed to start filesystem watcher: {error}"),
            })?;
        watcher
            .watch(Path::new(&self.shared.root), RecursiveMode::Recursive)
            .map_err(|error| DaemonError::IndexFailed {
                message: format!("failed to watch {}: {error}", self.shared.root),
            })?;
        self.watcher = Some(watcher);
        Ok(())
    }

    /// Records one event without a running watcher (tests, synthetic
    /// events). Runs the same skip/filter/add path as live events.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError::RootNotAbsolute`] when the path is relative, or propagates
    /// the [`ChangeSet`] budget failure when the pending set overflows.
    pub fn inject_event(
        &self,
        path: &str,
        kind: ChangeKind,
        is_directory: bool,
    ) -> Result<(), DaemonError> {
        if self.shared.is_closed() {
            return Ok(());
        }
        self.record(Path::new(path), Some(kind), Some(is_directory))
    }

    /// Queues a full reconciliation on the next flush.
    pub fn require_full_reconcile(&self) {
        if self.shared.is_closed() {
            return;
        }
        self.shared
            .changes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .require_full_reconcile();
        self.shared
            .reconcile_requested
            .store(true, std::sync::atomic::Ordering::SeqCst);
        // Reconciles often follow workspace changes; re-snapshot roots on the
        // next blocking classify step rather than reading the manifest here.
        self.shared
            .roots_stale
            .store(true, std::sync::atomic::Ordering::SeqCst);
        set_pending(&self.shared, true);
        self.shared.poke();
    }

    /// Re-snapshots index roots from the [`RootPathsSource`]. Call after a
    /// workspace change (manifest rewrite); per-event filtering uses the
    /// cached snapshot instead. Blocking (reads the manifest): call from a
    /// non-async context or infrequent path. The async classify path refreshes
    /// automatically via the `Shared::roots_stale` flag and workspace-signal events.
    pub fn refresh_roots(&self) {
        refresh_roots_blocking(&self.shared);
    }

    /// Delivers whatever is pending right now, bypassing the debounce.
    pub fn flush_now(&self) {
        flush_snapshot(&self.shared);
    }

    /// Stops the watcher and background tasks. Pending changes are
    /// dropped, mirroring TS `close`.
    pub async fn close(&mut self) {
        self.watcher = None;
        self.shared
            .closed
            .store(true, std::sync::atomic::Ordering::SeqCst);
        for task in self.tasks.drain(..) {
            task.abort();
            let _ = task.await;
        }
        set_pending(&self.shared, false);
    }

    fn spawn(&mut self, event_rx: Receiver<RawEvent>, flush_rx: Receiver<()>) {
        let shared = Arc::clone(&self.shared);
        self.tasks.push(tokio::spawn(async move {
            classify_loop(shared, event_rx).await;
        }));
        let shared = Arc::clone(&self.shared);
        let debounce = self.debounce;
        let max_wait = self.max_wait;
        self.tasks.push(tokio::spawn(async move {
            flush_loop(shared, flush_rx, debounce, max_wait).await;
        }));
    }

    fn record(
        &self,
        path: &Path,
        kind: Option<ChangeKind>,
        is_dir_hint: Option<bool>,
    ) -> Result<(), DaemonError> {
        record_raw(&self.shared, path, kind, is_dir_hint)
    }
}

impl Drop for WatchManager {
    /// Aborts (without awaiting) so a dropped manager never leaks its
    /// flush loop until runtime end. Prefer [`WatchManager::close`] for an
    /// orderly stop.
    fn drop(&mut self) {
        self.shared
            .closed
            .store(true, std::sync::atomic::Ordering::SeqCst);
        self.watcher = None;
        for task in self.tasks.drain(..) {
            task.abort();
        }
    }
}

fn set_pending(shared: &Shared, pending: bool) {
    if let Some(observe) = &shared.on_pending {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| observe(pending)));
        let _ = result;
    }
}
