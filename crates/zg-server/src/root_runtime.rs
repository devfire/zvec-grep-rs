//! Per-root runtime state: keys, generations, and reconciliation revisions.
//!
//! Mirrors the state half of `../zvec-grep/src/daemon/root-runtime.ts`
//! (`RootRuntime`: dirty/indexed revisions, full-reconciliation epochs,
//! watcher flags, activity counting) plus `resolveRequestedRoot` from
//! `runtime-manager.ts`. The TS class also manages read sessions, writer
//! contexts, and freshness probes; those live in the actor (`backend.rs`)
//! and [`WorkspaceReadSessionCache`](crate::read_session_cache) instead —
//! one actor task owns one `RootRuntime` as plain `&mut` state, so no lock
//! protects it (M3/M6, see `docs/ts-divergence.md`).

use std::fmt;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::errors::DaemonError;

/// Canonical workspace root: an absolute, symlink-resolved path.
///
/// Newtype (M2) so a requested (possibly relative, possibly aliased) path
/// can never flow where a canonical key is required. The field is private:
/// build with [`RootKey::parse`] (validates absolute) or
/// [`resolve_requested_root`] (validates everything).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RootKey(String);

impl RootKey {
    /// Validates an absolute path into a canonical key. Symlink resolution
    /// is [`resolve_requested_root`]'s job; this only enforces absoluteness.
    pub fn parse(value: &str) -> Result<Self, DaemonError> {
        if Path::new(value).is_absolute() {
            Ok(Self(value.to_owned()))
        } else {
            Err(DaemonError::RootNotAbsolute {
                root: value.to_owned(),
            })
        }
    }

    /// Raw canonical path.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RootKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Monotonic revision/epoch counter. Newtype (M2): a dirty revision must
/// never be compared against a reconciliation epoch, so each role gets its
/// own value even though both are `u64` underneath.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Generation(u64);

impl Generation {
    /// Zero value both counters start at (mirrors TS field initializers).
    pub const ZERO: Self = Self(0);

    /// Raw counter value.
    pub const fn get(self) -> u64 {
        self.0
    }

    /// Next value (`saturating_add(1)` — exhaustion saturates rather than
    /// wrapping, so staleness checks stay conservative).
    pub const fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }
}

impl fmt::Display for Generation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Observable runtime snapshot for status reporting and idle eviction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RootRuntimeSnapshot {
    /// Readers currently inside the session cache.
    pub active_readers: usize,
    /// Operations bracketed by `begin_activity`.
    pub active_operations: usize,
    /// An index/write job owns the root right now.
    pub writer_pending: bool,
    /// Watcher reported changes not yet flushed.
    pub watcher_pending: bool,
    /// Dirty revision (total `mark_dirty` calls).
    pub dirty_revision: Generation,
    /// Newest revision known indexed.
    pub indexed_revision: Generation,
}

/// Per-root indexing lifecycle state, owned by exactly one actor task.
///
/// Revision rule (mirrors TS): `indexed_revision < dirty_revision` means
/// known changes are unindexed; `reconciled_epoch < full_epoch` means a
/// full reconciliation is owed. `mark_reconciled` advances both.
#[derive(Debug)]
pub struct RootRuntime {
    key: RootKey,
    dirty_revision: Generation,
    indexed_revision: Generation,
    full_epoch: Generation,
    reconciled_epoch: Generation,
    non_probeable_epoch: Generation,
    watcher_active: bool,
    watcher_pending: bool,
    watcher_epoch: Generation,
    writer_pending: bool,
    active_operations: usize,
    active_readers: usize,
    closed: bool,
}

impl RootRuntime {
    /// Fresh runtime with zeroed revisions for `key`.
    pub fn new(key: RootKey) -> Self {
        Self {
            key,
            dirty_revision: Generation::ZERO,
            indexed_revision: Generation::ZERO,
            full_epoch: Generation::ZERO,
            reconciled_epoch: Generation::ZERO,
            non_probeable_epoch: Generation::ZERO,
            watcher_active: false,
            watcher_pending: false,
            watcher_epoch: Generation::ZERO,
            writer_pending: false,
            active_operations: 0,
            active_readers: 0,
            closed: false,
        }
    }

    /// Canonical root this runtime owns.
    pub fn key(&self) -> &RootKey {
        &self.key
    }

    /// Records new changes; returns the revision indexing must reach.
    pub fn mark_dirty(&mut self) -> Generation {
        self.dirty_revision = self.dirty_revision.next();
        self.dirty_revision
    }

    /// Records revision `revision` (default: current dirty) as indexed.
    pub fn mark_indexed(&mut self, revision: Generation) {
        if revision > self.indexed_revision {
            self.indexed_revision = revision;
        }
    }

    /// Demands a full reconciliation. `probe_allowed = false` (the
    /// default) also blocks the freshness-probe shortcut, mirroring TS
    /// `requireFullReconciliation(probeAllowed = false)`.
    pub fn require_full_reconciliation(&mut self, probe_allowed: bool) {
        self.full_epoch = self.full_epoch.next();
        if !probe_allowed {
            self.non_probeable_epoch = self.full_epoch;
        }
    }

    /// Current full-reconciliation epoch (stamped into proofs).
    pub fn reconciliation_epoch(&self) -> Generation {
        self.full_epoch
    }

    /// Records a reconciled epoch (and its revision) as done.
    pub fn mark_reconciled(&mut self, revision: Generation, epoch: Generation) {
        self.mark_indexed(revision);
        if epoch > self.reconciled_epoch {
            self.reconciled_epoch = epoch;
        }
    }

    /// True when a full reconciliation is still owed.
    pub fn requires_full_reconciliation(&self) -> bool {
        self.reconciled_epoch < self.full_epoch
    }

    /// True when anything is unindexed or unreconciled.
    pub fn needs_reconciliation(&self) -> bool {
        self.requires_full_reconciliation() || self.indexed_revision < self.dirty_revision
    }

    /// True when the watcher is up.
    pub fn watcher_active(&self) -> bool {
        self.watcher_active
    }

    /// Tracks watcher liveness (mirrors TS `setWatcherActive`).
    pub fn set_watcher_active(&mut self, active: bool) {
        self.watcher_active = active;
    }

    /// Tracks unflushed watcher changes, bumping the watcher epoch when
    /// new changes arrive (mirrors TS `setWatcherPending`).
    pub fn set_watcher_pending(&mut self, pending: bool) {
        if pending {
            self.watcher_epoch = self.watcher_epoch.next();
        }
        self.watcher_pending = pending;
    }

    /// Tracks whether a writer owns the root (searches route around it).
    pub fn set_writer_pending(&mut self, pending: bool) {
        self.writer_pending = pending;
    }

    /// Brackets one operation for idle-eviction accounting.
    pub fn begin_operation(&mut self) {
        self.active_operations += 1;
    }

    /// Closes one operation bracket.
    pub fn end_operation(&mut self) {
        self.active_operations = self.active_operations.saturating_sub(1);
    }

    /// Tracks session-cache readers for idle-eviction accounting.
    pub fn set_active_readers(&mut self, readers: usize) {
        self.active_readers = readers;
    }

    /// True while quiet enough to evict (no readers, ops, writer, or
    /// pending watcher changes).
    pub fn is_quiet(&self) -> bool {
        self.active_readers == 0
            && self.active_operations == 0
            && !self.writer_pending
            && !self.watcher_pending
    }

    /// Marks the runtime closed; the actor task exits after this.
    pub fn close(&mut self) {
        self.closed = true;
        self.watcher_active = false;
        self.watcher_pending = false;
        self.writer_pending = false;
    }

    /// True after [`RootRuntime::close`].
    pub fn is_closed(&self) -> bool {
        self.closed
    }

    /// Observable snapshot.
    pub fn snapshot(&self) -> RootRuntimeSnapshot {
        RootRuntimeSnapshot {
            active_readers: self.active_readers,
            active_operations: self.active_operations,
            writer_pending: self.writer_pending,
            watcher_pending: self.watcher_pending,
            dirty_revision: self.dirty_revision,
            indexed_revision: self.indexed_revision,
        }
    }
}

/// Resolves a caller-supplied root into a [`RootKey`], mirroring TS
/// `resolveRequestedRoot`: must be absolute, must exist as a directory,
/// and must be readable (plus writable when `writable`).
pub fn resolve_requested_root(requested: &str, writable: bool) -> Result<RootKey, DaemonError> {
    if !Path::new(requested).is_absolute() {
        return Err(DaemonError::RootNotAbsolute {
            root: requested.to_owned(),
        });
    }
    let metadata = std::fs::metadata(requested).map_err(|error| {
        use std::io::ErrorKind;
        match error.kind() {
            ErrorKind::PermissionDenied => DaemonError::RootPermissionDenied {
                root: requested.to_owned(),
            },
            _ => DaemonError::RootNotFound {
                root: requested.to_owned(),
            },
        }
    })?;
    if !metadata.is_dir() {
        return Err(DaemonError::RootNotFound {
            root: requested.to_owned(),
        });
    }
    if std::fs::read_dir(requested).is_err() {
        return Err(DaemonError::RootPermissionDenied {
            root: requested.to_owned(),
        });
    }
    if writable && metadata.permissions().readonly() {
        return Err(DaemonError::RootPermissionDenied {
            root: requested.to_owned(),
        });
    }
    let canonical = std::fs::canonicalize(requested).map_or_else(
        |_| requested.to_owned(),
        |path| path.to_string_lossy().into_owned(),
    );
    Ok(RootKey(canonical))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revisions_track_dirty_and_indexed() {
        let mut runtime = RootRuntime::new(RootKey::parse("/repo").unwrap());
        assert!(!runtime.needs_reconciliation());
        let first = runtime.mark_dirty();
        let second = runtime.mark_dirty();
        assert!(first < second);
        assert!(runtime.needs_reconciliation());
        // Stale marks do not move the revision backwards.
        runtime.mark_indexed(first);
        assert!(runtime.needs_reconciliation());
        runtime.mark_indexed(second);
        assert!(!runtime.needs_reconciliation());
    }

    #[test]
    fn full_reconciliation_epoch_flows_through_proofs() {
        let mut runtime = RootRuntime::new(RootKey::parse("/repo").unwrap());
        runtime.require_full_reconciliation(false);
        let epoch = runtime.reconciliation_epoch();
        assert!(runtime.requires_full_reconciliation());
        assert!(runtime.needs_reconciliation());
        let revision = runtime.mark_dirty();
        runtime.mark_reconciled(revision, epoch);
        assert!(!runtime.requires_full_reconciliation());
        assert!(!runtime.needs_reconciliation());
    }

    #[test]
    fn relative_roots_are_rejected() {
        assert!(matches!(
            RootKey::parse("relative/path"),
            Err(DaemonError::RootNotAbsolute { .. })
        ));
        assert!(matches!(
            resolve_requested_root("relative/path", false),
            Err(DaemonError::RootNotAbsolute { .. })
        ));
    }

    #[test]
    fn missing_and_file_roots_are_not_found() {
        assert!(matches!(
            resolve_requested_root("/definitely/not/here-zvec-grep", false),
            Err(DaemonError::RootNotFound { .. })
        ));
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("file.txt");
        std::fs::write(&file, "x").unwrap();
        assert!(matches!(
            resolve_requested_root(file.to_str().unwrap(), false),
            Err(DaemonError::RootNotFound { .. })
        ));
        // A real directory resolves and canonicalizes.
        let key = resolve_requested_root(dir.path().to_str().unwrap(), false).unwrap();
        assert!(Path::new(key.as_str()).is_absolute());
    }
}
