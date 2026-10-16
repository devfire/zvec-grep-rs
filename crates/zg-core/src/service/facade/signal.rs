//! Abort plumbing for blocking index runs: polls the caller's `AbortCheck`
//! on a helper thread and trips a shared [`CancelFlag`]. The blocking run
//! only ever sees the flag.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::pipeline::indexing::scanner::CancelFlag;

/// How often the `AbortCheck` poll thread probes during indexing.
const SIGNAL_POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Owns the poll thread; dropping it (any exit path, including `?` on a
/// failed `WorkspaceIndex::open`) signals `done` and joins the thread, so
/// the probe closure and [`CancelFlag`] clone are never leaked detached.
pub(super) struct SignalWatch {
    handle: Option<std::thread::JoinHandle<()>>,
    done: Arc<AtomicBool>,
}

impl SignalWatch {
    fn finish(&mut self) {
        self.done.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for SignalWatch {
    fn drop(&mut self) {
        self.finish();
    }
}

/// Polls `signal` on a helper thread until the returned [`SignalWatch`] is
/// dropped; the blocking index run only ever sees the flag.
pub(super) fn spawn_signal_watch(
    signal: Option<crate::service::types::AbortCheck>,
    cancel: &CancelFlag,
) -> Option<SignalWatch> {
    let signal = signal?;
    if signal() {
        cancel.cancel();
        return None;
    }
    let done = Arc::new(AtomicBool::new(false));
    let handle = {
        let signal = Arc::clone(&signal);
        let cancel = cancel.clone();
        let done = Arc::clone(&done);
        std::thread::spawn(move || {
            while !done.load(Ordering::Relaxed) {
                if signal() {
                    cancel.cancel();
                    break;
                }
                std::thread::sleep(SIGNAL_POLL_INTERVAL);
            }
        })
    };
    Some(SignalWatch {
        handle: Some(handle),
        done,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[test]
    fn dropping_watch_stops_and_joins_poll_thread() {
        let calls = Arc::new(AtomicUsize::new(0));
        let probe = Arc::clone(&calls);
        let signal: crate::service::types::AbortCheck = Arc::new(move || {
            probe.fetch_add(1, Ordering::Relaxed);
            false
        });
        let watch = spawn_signal_watch(Some(signal), &CancelFlag::new())
            .expect("watch spawns when signal is not tripped");
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while calls.load(Ordering::Relaxed) < 2 {
            assert!(
                std::time::Instant::now() < deadline,
                "poll thread never ran"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        drop(watch);
        let stopped_at = calls.load(Ordering::Relaxed);
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(calls.load(Ordering::Relaxed), stopped_at);
    }
}
