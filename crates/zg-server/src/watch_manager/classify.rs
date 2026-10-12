//! Async batch classification: conflates raw events, stats/filters them on
//! a blocking thread, and folds survivors into the pending set.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::mpsc::Receiver;

use crate::change_set::ChangeKind;

use super::record::{
    is_internal_path, is_workspace_signal_lossy, refresh_roots_blocking, should_track_roots,
};
use super::{EVENT_QUEUE_CAPACITY, OVERFLOW_CONFLATED_CAPACITY, RawEvent, Shared, set_pending};

/// Merges a same-path repeat into the conflated kind. `None` (unknown)
/// stays unknown so the kind is re-derived; conflicting hints collapse to
/// `Changed`, except create-then-delete which nets to `Deleted`.
fn merge_kind(first: Option<ChangeKind>, second: Option<ChangeKind>) -> Option<ChangeKind> {
    match (first, second) {
        (None, _) | (_, None) => None,
        (a, b) if a == b => a,
        (Some(ChangeKind::Created), Some(ChangeKind::Deleted)) => Some(ChangeKind::Deleted),
        (Some(ChangeKind::Created), Some(ChangeKind::Changed)) => Some(ChangeKind::Created),
        _ => Some(ChangeKind::Changed),
    }
}

/// Conflates a dropped (full-queue) event into the bounded overflow map.
/// Same-path repeats merge; past capacity the storm collapses to one full
/// reconcile so memory stays bounded no matter the burst size.
pub(super) fn coalesce_overflow(shared: &Arc<Shared>, event: RawEvent) {
    let mut overflow = shared
        .overflow
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if overflow.len() >= OVERFLOW_CONFLATED_CAPACITY && !overflow.contains_key(&event.path) {
        overflow.clear();
        drop(overflow);
        shared
            .changes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
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
pub(super) async fn classify_loop(shared: Arc<Shared>, mut events: Receiver<RawEvent>) {
    loop {
        if shared.is_closed() {
            return;
        }
        // Conflate the burst: overflow leftovers plus whatever is queued.
        let mut batch: HashMap<PathBuf, Option<ChangeKind>> = shared
            .overflow
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
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
                .unwrap_or_else(std::sync::PoisonError::into_inner)
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
        let classified = tokio::task::spawn_blocking(move || classify_batch(&worker, &batch)).await;
        if shared.is_closed() {
            return;
        }
        if let Ok(entries) = classified {
            fold_classified(&shared, &entries);
        } else {
            // A panicking classifier cannot be trusted: reconcile
            // everything (mirrors the watcher-error path, no new error
            // taxonomy needed).
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
    }
}

/// Classified survivor: absolute path plus the values `ChangeSet::add` needs.
pub(super) struct ClassifiedEntry {
    absolute: String,
    kind: ChangeKind,
    is_directory: bool,
}

/// Blocking half of classification (runs in `spawn_blocking`): refreshes
/// the roots snapshot on workspace signals, stats each path, and filters
/// against the snapshot. Returns only survivors for the async fold step.
pub(super) fn classify_batch(
    shared: &Arc<Shared>,
    batch: &HashMap<PathBuf, Option<ChangeKind>>,
) -> Vec<ClassifiedEntry> {
    // Refresh only on signals that survive filtering: the roots source is a
    // blocking manifest read (storage open), and internal writes
    // (`.zvec-grep` storage, lock files, the per-run manifest rewrite) must
    // never trigger it — the index's own writes would re-open storage on
    // every batch and hold read locks across maintenance (drop/reset).
    // Mirrors `record_raw`, which returns before the refresh for internals.
    let needs_refresh = shared.is_roots_stale()
        || batch.keys().any(|path| {
            path.is_absolute()
                && is_workspace_signal_lossy(path)
                && !is_internal_path(&shared.root, &path.to_string_lossy())
        });
    if needs_refresh {
        refresh_roots_blocking(shared);
    }
    let roots = shared
        .roots_cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    let has_source = shared.get_root_paths.is_some();
    let mut out = Vec::new();
    for (path, kind_hint) in batch {
        let absolute = path.to_string_lossy().into_owned();
        if !path.is_absolute() {
            continue;
        }
        if is_internal_path(&shared.root, &absolute) {
            continue;
        }
        let metadata = std::fs::symlink_metadata(path).ok();
        let is_directory = metadata.as_ref().is_some_and(std::fs::Metadata::is_dir);
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
            .unwrap_or_else(std::sync::PoisonError::into_inner);
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
