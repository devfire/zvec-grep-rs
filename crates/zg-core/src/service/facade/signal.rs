//! Abort plumbing for blocking index runs: polls the caller's `AbortCheck`
//! on a helper thread and trips a shared [`CancelFlag`]. The blocking run
//! only ever sees the flag (M6).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::pipeline::indexing::scanner::CancelFlag;

/// How often the `AbortCheck` poll thread probes during indexing.
const SIGNAL_POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Polls `signal` on a helper thread until `done` trips the shared
/// [`CancelFlag`]; the blocking index run only ever sees the flag (M6).
pub(super) fn spawn_signal_watch(
    signal: Option<crate::service::types::AbortCheck>,
    cancel: &CancelFlag,
) -> Option<(std::thread::JoinHandle<()>, Arc<AtomicBool>)> {
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
    Some((handle, done))
}

pub(super) fn finish_signal_watch(watch: Option<(std::thread::JoinHandle<()>, Arc<AtomicBool>)>) {
    if let Some((handle, done)) = watch {
        done.store(true, Ordering::Relaxed);
        let _ = handle.join();
    }
}
