//! Debounced delivery: flush loop (debounce + max-wait backstop) and the
//! snapshot/deliver step shared with `WatchManager::flush_now`.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc::Receiver;

use crate::change_set::ChangeSetSnapshot;

use super::{Shared, WatchReason, set_pending};

pub(super) async fn flush_loop(
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

/// Drains the pending set and delivers it. Snapshot and drain happen under
/// one lock acquisition; delivery runs outside the lock. A panicking
/// receiver is contained: the batch merges back with a forced full
/// reconcile and a new flush is scheduled (mirrors TS `flush`'s catch path).
pub(super) fn flush_snapshot(shared: &Arc<Shared>) {
    let (snapshot, reason) = {
        let mut changes = shared
            .changes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if changes.is_empty()
            && !shared
                .reconcile_requested
                .load(std::sync::atomic::Ordering::SeqCst)
        {
            return;
        }
        // Drain on snapshot: the next batch must hold only post-flush
        // changes, and the reconcile flag resets once consumed.
        let snapshot = changes.take_snapshot();
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
            .unwrap_or_else(std::sync::PoisonError::into_inner);
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
