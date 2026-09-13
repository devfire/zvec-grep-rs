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

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher, recommended_watcher};
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use tokio::task::JoinHandle;
use zg_core::pipeline::indexing::scanner::path_can_affect_index;
use zg_core::types::RootPath;

use crate::change_set::{
    ChangeKind, ChangeSet, ChangeSetOptions, ChangeSetSnapshot, MaxChangedPaths,
};
use crate::errors::DaemonError;

/// Debounce before a quiet batch flushes; mirrors TS `debounceMs`.
pub const DEFAULT_WATCH_DEBOUNCE: Duration = Duration::from_millis(750);

/// Backstop before a busy batch flushes anyway; mirrors TS `maxWaitMs`.
pub const DEFAULT_WATCH_MAX_WAIT: Duration = Duration::from_secs(5);

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
    on_changes: WatchChanges,
    on_pending: Option<WatchPending>,
    get_root_paths: Option<RootPathsSource>,
    flush_poke: UnboundedSender<()>,
}

impl Shared {
    fn poke(&self) {
        let _ = self.flush_poke.send(());
    }

    fn is_closed(&self) -> bool {
        self.closed.load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// Recursive filesystem watcher coalescing events into [`ChangeSetSnapshot`]
/// batches. Owned by one root actor; `start` begins delivery, `close`
/// stops it.
pub struct WatchManager {
    shared: Arc<Shared>,
    event_tx: UnboundedSender<RawEvent>,
    debounce: Duration,
    max_wait: Duration,
    watcher: Option<RecommendedWatcher>,
    tasks: Vec<JoinHandle<()>>,
}

impl WatchManager {
    /// Builds an idle manager; call [`WatchManager::start`] to watch.
    #[must_use]
    pub fn new(options: WatchManagerOptions) -> Self {
        let (flush_poke, flush_rx) = unbounded_channel();
        let (event_tx, event_rx) = unbounded_channel();
        let shared = Arc::new(Shared {
            root: options.root.clone(),
            changes: Mutex::new(ChangeSet::new(ChangeSetOptions {
                root: Some(options.root.clone()),
                max_changed_paths: options.max_changed_paths,
            })),
            reconcile_requested: std::sync::atomic::AtomicBool::new(false),
            closed: std::sync::atomic::AtomicBool::new(false),
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

    /// Starts the recursive notify watcher. Watching `.git` / `.zvec-grep`
    /// is skipped at record time (see `record_raw()`).
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
                match result {
                    Ok(event) => {
                        let kind = match event.kind {
                            EventKind::Create(_) => Some(ChangeKind::Created),
                            EventKind::Modify(_) => Some(ChangeKind::Changed),
                            EventKind::Remove(_) => Some(ChangeKind::Deleted),
                            EventKind::Any | EventKind::Access(_) | EventKind::Other => None,
                        };
                        for path in event.paths {
                            let _ = sender.send(RawEvent { path, kind });
                        }
                    }
                    Err(_) => {
                        // A dead watcher cannot be trusted: reconcile everything.
                        shared
                            .changes
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                            .require_full_reconcile();
                        shared
                            .reconcile_requested
                            .store(true, std::sync::atomic::Ordering::SeqCst);
                        set_pending(&shared, true);
                        shared.poke();
                    }
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
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .require_full_reconcile();
        self.shared
            .reconcile_requested
            .store(true, std::sync::atomic::Ordering::SeqCst);
        set_pending(&self.shared, true);
        self.shared.poke();
    }

    /// Delivers whatever is pending right now, bypassing the debounce.
    pub async fn flush_now(&self) {
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

    fn spawn(
        &mut self,
        event_rx: tokio::sync::mpsc::UnboundedReceiver<RawEvent>,
        flush_rx: tokio::sync::mpsc::UnboundedReceiver<()>,
    ) {
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

/// Classifies one raw event and folds it into the pending set.
fn record_raw(
    shared: &Arc<Shared>,
    path: &Path,
    kind: Option<ChangeKind>,
    is_dir_hint: Option<bool>,
) -> Result<(), DaemonError> {
    let absolute = if path.is_absolute() {
        path.to_string_lossy().into_owned()
    } else {
        return Err(DaemonError::RootNotAbsolute {
            root: path.to_string_lossy().into_owned(),
        });
    };
    // Never index internals.
    if let Ok(relative) = Path::new(&absolute).strip_prefix(Path::new(&shared.root))
        && relative.components().any(|component| {
            matches!(
                component.as_os_str().to_str(),
                Some(".git") | Some(".zvec-grep")
            )
        })
    {
        return Ok(());
    }
    let metadata = std::fs::symlink_metadata(&absolute).ok();
    let is_directory =
        is_dir_hint.unwrap_or_else(|| metadata.as_ref().is_some_and(|metadata| metadata.is_dir()));
    let kind = kind.unwrap_or_else(|| {
        if metadata.is_some() {
            ChangeKind::Changed
        } else {
            ChangeKind::Deleted
        }
    });
    if !should_track(shared, &absolute, is_directory) {
        return Ok(());
    }
    let became_pending = {
        let mut changes = shared
            .changes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let was_empty = changes.is_empty();
        changes.add(&absolute, kind, is_directory)?;
        was_empty && !changes.is_empty()
    };
    if became_pending {
        set_pending(shared, true);
    }
    shared.poke();
    Ok(())
}

/// Fail-open filtering: `.gitignore` always tracks; unreadable rules or
/// a failing predicate track rather than risk a missed change.
fn should_track(shared: &Shared, absolute: &str, is_directory: bool) -> bool {
    if Path::new(absolute)
        .file_name()
        .and_then(|name| name.to_str())
        == Some(".gitignore")
    {
        return true;
    }
    let Some(source) = &shared.get_root_paths else {
        return true;
    };
    let roots =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| source())).unwrap_or_default();
    path_can_affect_index(&roots, absolute, is_directory).unwrap_or(true)
}

async fn classify_loop(
    shared: Arc<Shared>,
    mut events: tokio::sync::mpsc::UnboundedReceiver<RawEvent>,
) {
    while let Some(event) = events.recv().await {
        if shared.is_closed() {
            return;
        }
        let _ = record_raw(&shared, &event.path, event.kind, None);
    }
}

async fn flush_loop(
    shared: Arc<Shared>,
    mut pokes: tokio::sync::mpsc::UnboundedReceiver<()>,
    debounce: Duration,
    max_wait: Duration,
) {
    use std::time::Instant;
    while pokes.recv().await.is_some() {
        if shared.is_closed() {
            return;
        }
        let start = Instant::now();
        loop {
            tokio::time::sleep(debounce).await;
            if shared.is_closed() {
                return;
            }
            // Drain the burst: new pokes mean new events arrived mid-sleep.
            let mut fresh = false;
            while pokes.try_recv().is_ok() {
                fresh = true;
            }
            if !fresh || start.elapsed() >= max_wait {
                break;
            }
        }
        flush_snapshot(&shared);
    }
}

/// Snapshots the pending set and delivers it. A panicking receiver is
/// contained: the batch merges back with a forced full reconcile and a
/// new flush is scheduled (mirrors TS `flush`'s catch path).
fn flush_snapshot(shared: &Arc<Shared>) {
    let (snapshot, reason) = {
        let mut changes = shared
            .changes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if changes.is_empty()
            && !shared
                .reconcile_requested
                .load(std::sync::atomic::Ordering::SeqCst)
        {
            return;
        }
        let snapshot = changes.snapshot();
        let reason = if shared
            .reconcile_requested
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            WatchReason::Reconcile
        } else {
            WatchReason::Watch
        };
        shared
            .reconcile_requested
            .store(false, std::sync::atomic::Ordering::SeqCst);
        (snapshot, reason)
    };
    let delivered = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        (shared.on_changes)(snapshot.clone(), reason);
    }));
    if delivered.is_err() {
        let mut changes = shared
            .changes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        changes.merge(&ChangeSetSnapshot {
            force_full_reconcile: true,
            ..snapshot
        });
        shared
            .reconcile_requested
            .store(true, std::sync::atomic::Ordering::SeqCst);
        shared.poke();
    } else {
        set_pending(shared, false);
    }
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    /// Batches delivered by the test manager.
    type RecordedBatches = Arc<StdMutex<Vec<(ChangeSetSnapshot, WatchReason)>>>;

    fn manager(root: &str) -> (WatchManager, RecordedBatches) {
        let batches: RecordedBatches = Arc::new(StdMutex::new(Vec::new()));
        let batches_clone = batches.clone();
        let manager = WatchManager::new(WatchManagerOptions {
            root: root.to_owned(),
            debounce: Some(Duration::from_millis(5)),
            max_wait: Some(Duration::from_millis(50)),
            max_changed_paths: None,
            on_changes: Arc::new(move |snapshot, reason| {
                batches_clone.lock().unwrap().push((snapshot, reason));
            }),
            get_root_paths: None,
            on_pending: None,
        });
        (manager, batches)
    }

    #[tokio::test]
    async fn coalesces_events_into_one_batch() {
        let (manager, batches) = manager("/repo");
        manager
            .inject_event("/repo/a.rs", ChangeKind::Changed, false)
            .unwrap();
        manager
            .inject_event("/repo/b.rs", ChangeKind::Created, false)
            .unwrap();
        manager.flush_now().await;
        let batches = batches.lock().unwrap();
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].0.touched_files, vec!["/repo/a.rs", "/repo/b.rs"]);
        assert_eq!(batches[0].1, WatchReason::Watch);
    }

    #[tokio::test]
    async fn skips_index_internals() {
        let (manager, batches) = manager("/repo");
        manager
            .inject_event("/repo/.git/HEAD", ChangeKind::Changed, false)
            .unwrap();
        manager
            .inject_event("/repo/.zvec-grep/files.json", ChangeKind::Changed, false)
            .unwrap();
        manager.flush_now().await;
        assert!(batches.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn debounce_delivers_without_manual_flush() {
        let (manager, batches) = manager("/repo");
        manager
            .inject_event("/repo/a.rs", ChangeKind::Changed, false)
            .unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(batches.lock().unwrap().len(), 1);
        drop(manager);
    }

    #[tokio::test]
    async fn full_reconcile_flushes_as_reconcile() {
        let (manager, batches) = manager("/repo");
        manager.require_full_reconcile();
        manager.flush_now().await;
        let batches = batches.lock().unwrap();
        assert_eq!(batches.len(), 1);
        assert!(batches[0].0.force_full_reconcile);
        assert_eq!(batches[0].1, WatchReason::Reconcile);
    }
}
