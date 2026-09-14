//! Poison-tolerant mutex access for short critical sections.
//!
//! Every guarded section in the daemon is short, uncontended, and never held
//! across `.await` (see the state docs at each use); a poisoned mutex
//! therefore yields its inner value instead of wedging the daemon — the
//! panicking holder already records its own failure through its job outcome.
//! `MutexExt::lock_ignore_poison` is the single spelling of that policy
//! (R3 collapsed the per-module `fn lock` copies here).

use std::sync::{Mutex, MutexGuard};

/// Poison-tolerant [`Mutex`] guard for short critical sections.
pub(crate) trait MutexExt<T> {
    /// Locks, yielding the inner value when poisoned. Never held across `.await`.
    /// (`MutexGuard` is already `#[must_use]`, so no attribute here.)
    fn lock_ignore_poison(&self) -> MutexGuard<'_, T>;
}

impl<T> MutexExt<T> for Mutex<T> {
    fn lock_ignore_poison(&self) -> MutexGuard<'_, T> {
        self.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}
