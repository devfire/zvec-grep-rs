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
use zg_core::pipeline::indexing::scanner::{PathKind, path_can_affect_index};
use zg_core::types::RootPath;

use crate::change_set::{
    ChangeKind, ChangeSet, ChangeSetOptions, ChangeSetSnapshot, MaxChangedPaths,
};
use crate::errors::DaemonError;

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
        let roots_cache = load_roots(&options.get_root_paths).unwrap_or_default();
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

    /// Starts the recursive notify watcher. Watching `.git` / `.zvec-grep`
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
    /// automatically via [`Shared::roots_stale`] and workspace-signal events.
    pub fn refresh_roots(&self) {
        refresh_roots_blocking(&self.shared);
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

/// Classifies one raw event and folds it into the pending set. Synchronous
/// (used by `inject_event`/tests): filters against the cached roots
/// snapshot, so it never calls the blocking [`RootPathsSource`] per event.
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
    if is_internal_path(&shared.root, &absolute) {
        return Ok(());
    }
    if is_workspace_signal(&absolute) {
        refresh_roots_blocking(shared);
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
    if !should_track_cached(shared, &absolute, is_directory) {
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

/// True for paths under `.git` / `.zvec-grep`: never indexed. Pure string
/// prefix check, safe on any thread.
fn is_internal_path(root: &str, absolute: &str) -> bool {
    if let Ok(relative) = Path::new(absolute).strip_prefix(Path::new(root))
        && relative.components().any(|component| {
            matches!(
                component.as_os_str().to_str(),
                Some(".git") | Some(".zvec-grep")
            )
        })
    {
        return true;
    }
    false
}

/// True for events that may change index scoping (ignore rules, workspace
/// manifest): these refresh the cached roots snapshot instead of using it.
fn is_workspace_signal(absolute: &str) -> bool {
    matches!(
        Path::new(absolute)
            .file_name()
            .and_then(|name| name.to_str()),
        Some(".gitignore") | Some("manifest.json")
    )
}

/// Fail-open filtering against the cached roots snapshot: `.gitignore`
/// always tracks; unreadable rules or a failing predicate track rather
/// than risk a missed change. Never calls the source: the hot path stays
/// off blocking I/O.
fn should_track_cached(shared: &Shared, absolute: &str, is_directory: bool) -> bool {
    if Path::new(absolute)
        .file_name()
        .and_then(|name| name.to_str())
        == Some(".gitignore")
    {
        return true;
    }
    if shared.get_root_paths.is_none() {
        return true;
    }
    let roots = shared
        .roots_cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let kind = if is_directory {
        PathKind::Dir
    } else {
        PathKind::File
    };
    path_can_affect_index(&roots, absolute, kind).unwrap_or(true)
}

/// Calls the roots source fail-open: a panicking source keeps the previous
/// snapshot (`None` = keep, not clear) rather than risk a missed change.
fn load_roots(source: &Option<RootPathsSource>) -> Option<Vec<RootPath>> {
    let source = source.as_ref()?;
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| source())).ok()
}

/// Re-snapshots [`Shared::roots_cache`] and clears the stale flag. Blocking
/// (reads the manifest): runs on a `spawn_blocking` thread, at construction,
/// or via [`WatchManager::refresh_roots`] — never inline on the async loop.
fn refresh_roots_blocking(shared: &Shared) {
    if let Some(roots) = load_roots(&shared.get_root_paths) {
        *shared
            .roots_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = roots;
    }
    shared
        .roots_stale
        .store(false, std::sync::atomic::Ordering::SeqCst);
}

/// Merges a same-path repeat into the conflated kind. `None` (unknown)
/// stays unknown so the kind is re-derived; conflicting hints collapse to
/// `Changed`, except create-then-delete which nets to `Deleted`.
fn merge_kind(first: Option<ChangeKind>, second: Option<ChangeKind>) -> Option<ChangeKind> {
    match (first, second) {
        (None, _) | (_, None) => None,
        (a, b) if a == b => a,
        (Some(ChangeKind::Created), Some(ChangeKind::Deleted)) => Some(ChangeKind::Deleted),
        (Some(ChangeKind::Deleted), Some(ChangeKind::Created)) => Some(ChangeKind::Changed),
        (Some(ChangeKind::Created), Some(ChangeKind::Changed)) => Some(ChangeKind::Created),
        _ => Some(ChangeKind::Changed),
    }
}

/// Conflates a dropped (full-queue) event into the bounded overflow map.
/// Same-path repeats merge; past capacity the storm collapses to one full
/// reconcile so memory stays bounded no matter the burst size.
fn coalesce_overflow(shared: &Arc<Shared>, event: RawEvent) {
    let mut overflow = shared
        .overflow
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if overflow.len() >= OVERFLOW_CONFLATED_CAPACITY && !overflow.contains_key(&event.path) {
        overflow.clear();
        drop(overflow);
        shared
            .changes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .require_full_reconcile();
        shared
            .reconcile_requested
            .store(true, std::sync::atomic::Ordering::SeqCst);
        set_pending(shared, true);
        shared.poke();
        return;
    }
    overflow
        .entry(event.path)
        .and_modify(|kind| *kind = merge_kind(*kind, event.kind))
        .or_insert(event.kind);
}

/// Drains raw events, conflates same-path repeats, and classifies the batch
/// on a blocking thread: `symlink_metadata` plus the roots-source snapshot
/// never run on the tokio worker. One poke per batch keeps the bounded flush
/// queue from queueing behind a burst, so the flush path cannot starve.
async fn classify_loop(shared: Arc<Shared>, mut events: Receiver<RawEvent>) {
    loop {
        if shared.is_closed() {
            return;
        }
        // Conflate the burst: overflow leftovers plus whatever is queued.
        let mut batch: HashMap<PathBuf, Option<ChangeKind>> = shared
            .overflow
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .drain()
            .collect();
        let mut drained = 0;
        while let Ok(event) = events.try_recv() {
            batch
                .entry(event.path)
                .and_modify(|kind| *kind = merge_kind(*kind, event.kind))
                .or_insert(event.kind);
            drained += 1;
            if drained >= EVENT_QUEUE_CAPACITY {
                break;
            }
        }
        if batch.is_empty() {
            // Nothing queued: wait for at least one event. `None` means every
            // sender is gone (shutting down).
            let Some(event) = events.recv().await else {
                return;
            };
            if shared.is_closed() {
                return;
            }
            batch.insert(event.path, event.kind);
            while let Ok(event) = events.try_recv() {
                batch
                    .entry(event.path)
                    .and_modify(|kind| *kind = merge_kind(*kind, event.kind))
                    .or_insert(event.kind);
                drained += 1;
                if drained >= EVENT_QUEUE_CAPACITY {
                    break;
                }
            }
            // Anything that overflowed while we waited joins the same batch.
            for (path, kind) in shared
                .overflow
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .drain()
            {
                batch
                    .entry(path)
                    .and_modify(|current| *current = merge_kind(*current, kind))
                    .or_insert(kind);
            }
        }
        if shared.is_closed() {
            return;
        }
        // Blocking FS + roots work leaves the async loop entirely.
        let worker = Arc::clone(&shared);
        let classified = tokio::task::spawn_blocking(move || classify_batch(&worker, batch)).await;
        if shared.is_closed() {
            return;
        }
        match classified {
            Ok(entries) => fold_classified(&shared, &entries),
            Err(_) => {
                // A panicking classifier cannot be trusted: reconcile
                // everything (mirrors the watcher-error path, no new error
                // taxonomy needed).
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
    }
}

/// Classified survivor: absolute path plus the values `ChangeSet::add` needs.
struct ClassifiedEntry {
    absolute: String,
    kind: ChangeKind,
    is_directory: bool,
}

/// Blocking half of classification (runs in `spawn_blocking`): refreshes
/// the roots snapshot on workspace signals, stats each path, and filters
/// against the snapshot. Returns only survivors for the async fold step.
fn classify_batch(
    shared: &Arc<Shared>,
    batch: HashMap<PathBuf, Option<ChangeKind>>,
) -> Vec<ClassifiedEntry> {
    if shared.is_roots_stale() || batch.keys().any(|path| is_workspace_signal_lossy(path)) {
        refresh_roots_blocking(shared);
    }
    let roots = shared
        .roots_cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    let has_source = shared.get_root_paths.is_some();
    let mut out = Vec::new();
    for (path, kind_hint) in &batch {
        let absolute = path.to_string_lossy().into_owned();
        if !path.is_absolute() {
            continue;
        }
        if is_internal_path(&shared.root, &absolute) {
            continue;
        }
        let metadata = std::fs::symlink_metadata(path).ok();
        let is_directory = metadata.as_ref().is_some_and(|metadata| metadata.is_dir());
        let kind = kind_hint.unwrap_or_else(|| {
            if metadata.is_some() {
                ChangeKind::Changed
            } else {
                ChangeKind::Deleted
            }
        });
        if should_track_roots(&absolute, is_directory, has_source, &roots) {
            out.push(ClassifiedEntry {
                absolute,
                kind,
                is_directory,
            });
        }
    }
    out
}

/// Workspace-signal check on a pre-`String` path (blocking thread side).
fn is_workspace_signal_lossy(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|name| name.to_str()),
        Some(".gitignore") | Some("manifest.json")
    )
}

/// [`should_track_cached`] without the cache lock: the blocking batch owns
/// a cloned snapshot, so per-event filtering borrows it.
fn should_track_roots(
    absolute: &str,
    is_directory: bool,
    has_source: bool,
    roots: &[RootPath],
) -> bool {
    if Path::new(absolute)
        .file_name()
        .and_then(|name| name.to_str())
        == Some(".gitignore")
    {
        return true;
    }
    if !has_source {
        return true;
    }
    let kind = if is_directory {
        PathKind::Dir
    } else {
        PathKind::File
    };
    path_can_affect_index(roots, absolute, kind).unwrap_or(true)
}

/// Async fold half: merges one classified batch into the pending set with a
/// single lock hold and a single (coalesced) poke.
fn fold_classified(shared: &Arc<Shared>, entries: &[ClassifiedEntry]) {
    if entries.is_empty() {
        return;
    }
    let became_pending = {
        let mut changes = shared
            .changes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let was_empty = changes.is_empty();
        for entry in entries {
            // Paths are verified absolute by `classify_batch`; budget
            // widening never errors, so only relative-path failures (which
            // cannot happen here) are dropped.
            let _ = changes.add(&entry.absolute, entry.kind, entry.is_directory);
        }
        was_empty && !changes.is_empty()
    };
    if became_pending {
        set_pending(shared, true);
    }
    shared.poke();
}

async fn flush_loop(
    shared: Arc<Shared>,
    mut pokes: Receiver<()>,
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
            // Bounded to one pending poke, so this is at most one extra
            // iteration per burst and never a backlog to starve behind.
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

    fn write_manifest_policy(home: &Path, root: &str, include_nested_git: Option<bool>) {
        use zg_core::config::EmbeddingRuntimeConfig;
        use zg_core::manifest::{
            CURRENT_MANIFEST_VERSION, WorkspaceManifest, write_workspace_manifest,
        };
        use zg_core::types::{UnixMillis, WorkspaceIndexInfo, WorkspaceIndexPolicy};

        std::fs::create_dir_all(home).expect("mkdir home");
        let manifest = WorkspaceManifest {
            info: WorkspaceIndexInfo {
                id: "watch-test".to_owned(),
                name: "watch-test".to_owned(),
                path: home.to_string_lossy().into_owned(),
                root_paths: vec![RootPath {
                    absolute_path: root.to_owned(),
                    recursive: true,
                    include: Vec::new(),
                    exclude: Vec::new(),
                    globs: Vec::new(),
                    insensitive_globs: Vec::new(),
                    file_types: Vec::new(),
                    excluded_file_types: Vec::new(),
                    hidden: None,
                    no_ignore: None,
                    ignore_files: Vec::new(),
                    max_depth: None,
                    max_file_size_bytes: None,
                    follow: None,
                    include_nested_git,
                }],
                index_policy: Some(WorkspaceIndexPolicy::Enabled),
                embedding: Some(None),
                index_version: Some(zg_core::types::CURRENT_INDEX_VERSION),
                created_time: UnixMillis::now(),
                updated_time: UnixMillis::now(),
            },
            manifest_version: CURRENT_MANIFEST_VERSION,
            embedding_runtime: EmbeddingRuntimeConfig::default(),
        };
        write_workspace_manifest(home, &manifest).expect("write manifest");
    }

    #[tokio::test]
    async fn nested_git_policy_gates_watcher_batches() {
        use zg_core::manifest::read_workspace_manifest;

        let dir = tempfile::tempdir().expect("tempdir");
        let root = zg_core::paths::to_display_path(dir.path());
        std::fs::create_dir_all(dir.path().join("repo-a/.git")).expect("mkdir");
        std::fs::write(dir.path().join("repo-a/.git/HEAD"), "ref\n").expect("marker");
        std::fs::write(dir.path().join("repo-a/a.txt"), "a\n").expect("nested file");
        let home = dir.path().join(".zvec-grep");
        write_manifest_policy(&home, &root, None);

        let home_clone = home.clone();
        let source: RootPathsSource = Arc::new(move || {
            read_workspace_manifest(&home_clone)
                .ok()
                .flatten()
                .map(|manifest| manifest.info.root_paths)
                .unwrap_or_default()
        });
        // One flush per manager: `flush_snapshot` redelivers pending batches,
        // so each phase below uses a fresh manager and flushes exactly once.
        let watch_once = |source: RootPathsSource, events: Vec<(String, bool)>| {
            let batches: RecordedBatches = Arc::new(StdMutex::new(Vec::new()));
            let batches_clone = batches.clone();
            let root = root.clone();
            async move {
                let manager = WatchManager::new(WatchManagerOptions {
                    root,
                    debounce: Some(Duration::from_secs(3600)),
                    max_wait: Some(Duration::from_secs(3600)),
                    max_changed_paths: None,
                    on_changes: Arc::new(move |snapshot, reason| {
                        batches_clone.lock().unwrap().push((snapshot, reason));
                    }),
                    get_root_paths: Some(source),
                    on_pending: None,
                });
                for (path, is_directory) in &events {
                    manager
                        .inject_event(path, ChangeKind::Changed, *is_directory)
                        .unwrap();
                }
                manager.flush_now().await;
                let batches = batches.lock().unwrap();
                batches
                    .iter()
                    .map(|(snapshot, _)| snapshot.touched_files.clone())
                    .collect::<Vec<_>>()
            }
        };

        let nested = format!("{root}/repo-a/a.txt");
        assert!(
            watch_once(source.clone(), vec![(nested.clone(), false)])
                .await
                .is_empty(),
            "disabled policy tracks no nested change"
        );

        write_manifest_policy(&home, &root, Some(true));
        assert_eq!(
            watch_once(source.clone(), vec![(nested.clone(), false)]).await,
            vec![vec![nested.clone()]],
            "enabled policy yields the nested touched path"
        );

        assert!(
            watch_once(
                source.clone(),
                vec![
                    (format!("{root}/repo-a/.git/HEAD"), false),
                    (format!("{root}/.zvec-grep/manifest.json"), false),
                ],
            )
            .await
            .is_empty(),
            ".git and .zvec-grep changes stay untracked"
        );
    }
}
