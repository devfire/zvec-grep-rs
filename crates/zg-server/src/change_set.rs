//! Watcher change accumulation: `ChangeSet` with rescan collapsing.
//!
//! Mirrors `../zvec-grep/src/daemon/change-set.ts` (`created` / `changed` /
//! `deleted` events plus rescan-directory / deleted-prefix accumulation,
//! `.gitignore` widening, and the path-budget backstop). Collapse and
//! budget semantics are verbatim; only the error shape changes (TS throws a
//! plain `Error` for relative paths, Rust returns
//! [`DaemonError::RootNotAbsolute`]).

use std::collections::BTreeSet;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::errors::DaemonError;

/// Default cap on accumulated paths before widening kicks in.
pub const DEFAULT_MAX_CHANGED_PATHS: usize = 1_000;

/// Cap on accumulated paths before the set widens scopes or forces a full
/// reconcile. Newtype (M2) so a bare `usize` cannot flow in silently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MaxChangedPaths(usize);

impl MaxChangedPaths {
    /// Default budget (`1000`), mirroring the TS constructor default.
    pub const DEFAULT: Self = Self(DEFAULT_MAX_CHANGED_PATHS);

    /// Wraps a raw budget. Zero means "widen immediately".
    #[must_use]
    pub const fn new(value: usize) -> Self {
        Self(value)
    }

    /// Raw budget value.
    #[must_use]
    pub const fn get(self) -> usize {
        self.0
    }
}

impl Default for MaxChangedPaths {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// What happened to one watched path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    /// Path appeared (or a `rename` event over an existing path).
    Created,
    /// Path content or metadata changed.
    Changed,
    /// Path disappeared.
    Deleted,
}

/// Collapsed, sorted view of accumulated changes handed to the coordinator.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeSetSnapshot {
    /// Exact files to re-scan.
    pub touched_files: Vec<String>,
    /// Directories to re-scan wholesale.
    pub rescan_directories: Vec<String>,
    /// Deleted path prefixes (everything beneath is gone).
    pub deleted_prefixes: Vec<String>,
    /// True when incremental state is untrustworthy; reconcile everything.
    pub force_full_reconcile: bool,
}

/// Options for [`ChangeSet`].
#[derive(Debug, Clone, Default)]
pub struct ChangeSetOptions {
    /// Workspace root, used to clamp widened scopes. `None` forces a full
    /// reconcile once the budget is exceeded.
    pub root: Option<String>,
    /// Path budget before widening; defaults to [`MaxChangedPaths::DEFAULT`].
    pub max_changed_paths: Option<MaxChangedPaths>,
}

/// Accumulates watcher events between debounced flushes.
///
/// Semantics mirror the TS class exactly: `.gitignore` touches widen to
/// their parent directory, deletes become prefixes, covered paths are
/// dropped, and an over-budget set widens leaf scopes to parent
/// directories (falling back to a full reconcile without a root or when
/// widening stops shrinking the set).
#[derive(Debug)]
pub struct ChangeSet {
    touched_files: BTreeSet<String>,
    rescan_directories: BTreeSet<String>,
    deleted_prefixes: BTreeSet<String>,
    force_full_reconcile: bool,
    root: Option<String>,
    max_changed_paths: usize,
}

impl ChangeSet {
    /// Empty accumulator with the given root and budget.
    #[must_use]
    pub fn new(options: ChangeSetOptions) -> Self {
        Self {
            touched_files: BTreeSet::new(),
            rescan_directories: BTreeSet::new(),
            deleted_prefixes: BTreeSet::new(),
            force_full_reconcile: false,
            root: options.root.map(|root| normalize_change_path(&root)),
            max_changed_paths: options.max_changed_paths.unwrap_or_default().get(),
        }
    }

    /// Records one event. Relative paths are rejected: watcher output must
    /// be absolute for prefix collapsing to mean anything.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError::RootNotAbsolute`] when the path is not absolute.
    pub fn add(
        &mut self,
        path: &str,
        kind: ChangeKind,
        is_directory: bool,
    ) -> Result<(), DaemonError> {
        if !Path::new(path).is_absolute() {
            return Err(DaemonError::RootNotAbsolute {
                root: path.to_owned(),
            });
        }
        if self.force_full_reconcile && self.has_paths() {
            return Ok(());
        }
        let normalized = normalize_change_path(path);
        if Self::covered_by(&self.rescan_directories, &normalized)
            || Self::covered_by(&self.deleted_prefixes, &normalized)
        {
            return Ok(());
        }
        if file_name(&normalized) == ".gitignore" {
            self.rescan_directories.insert(parent_dir(&normalized));
        } else if kind == ChangeKind::Deleted {
            self.deleted_prefixes.insert(normalized);
        } else if is_directory {
            self.rescan_directories.insert(normalized);
        } else {
            self.touched_files.insert(normalized);
        }
        if self.len() >= self.max_changed_paths {
            self.enforce_path_budget();
        }
        Ok(())
    }

    /// Marks incremental state untrustworthy; the next snapshot reconciles
    /// everything.
    pub fn require_full_reconcile(&mut self) {
        self.force_full_reconcile = true;
    }

    /// Folds another snapshot in, mirroring TS `merge`.
    pub fn merge(&mut self, other: &ChangeSetSnapshot) {
        if self.force_full_reconcile && self.has_paths() {
            return;
        }
        self.touched_files
            .extend(other.touched_files.iter().cloned());
        self.rescan_directories
            .extend(other.rescan_directories.iter().cloned());
        self.deleted_prefixes
            .extend(other.deleted_prefixes.iter().cloned());
        self.force_full_reconcile |= other.force_full_reconcile;
        if !self.force_full_reconcile && self.len() >= self.max_changed_paths {
            self.enforce_path_budget();
        }
    }

    /// Collapsed, sorted snapshot; also collapses in place (TS `snapshot`
    /// collapses before copying out).
    pub fn snapshot(&mut self) -> ChangeSetSnapshot {
        self.collapse_paths();
        ChangeSetSnapshot {
            touched_files: self.touched_files.iter().cloned().collect(),
            rescan_directories: self.rescan_directories.iter().cloned().collect(),
            deleted_prefixes: self.deleted_prefixes.iter().cloned().collect(),
            force_full_reconcile: self.force_full_reconcile,
        }
    }

    /// Collapsed snapshot that also drains the accumulator, so the next
    /// batch contains only post-flush changes. The reconcile flag resets
    /// once consumed; root and budget are preserved.
    pub fn take_snapshot(&mut self) -> ChangeSetSnapshot {
        let snapshot = self.snapshot();
        self.touched_files.clear();
        self.rescan_directories.clear();
        self.deleted_prefixes.clear();
        self.force_full_reconcile = false;
        snapshot
    }

    /// Total accumulated paths across all three sets.
    #[must_use]
    pub fn len(&self) -> usize {
        self.touched_files.len() + self.rescan_directories.len() + self.deleted_prefixes.len()
    }

    /// True when nothing is accumulated.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        !self.has_paths() && !self.force_full_reconcile
    }

    /// True when any path is accumulated, ignoring the reconcile flag —
    /// unlike [`is_empty`](Self::is_empty), which also reports flag state.
    /// Exists so the budget short-circuit keeps TS `size > 0` semantics
    /// without tripping `clippy::len_zero`.
    fn has_paths(&self) -> bool {
        !self.touched_files.is_empty()
            || !self.rescan_directories.is_empty()
            || !self.deleted_prefixes.is_empty()
    }

    fn collapse_paths(&mut self) {
        collapse_set(&mut self.rescan_directories);
        collapse_set(&mut self.deleted_prefixes);
        self.touched_files.retain(|file| {
            !has_ancestor(&self.rescan_directories, file)
                && !has_ancestor(&self.deleted_prefixes, file)
        });
        self.rescan_directories
            .retain(|dir| !has_ancestor(&self.deleted_prefixes, dir));
    }

    fn enforce_path_budget(&mut self) {
        self.collapse_paths();
        if self.len() < self.max_changed_paths {
            return;
        }
        let Some(root) = self.root.clone() else {
            self.force_full_reconcile = true;
            return;
        };
        // Exact events stay trustworthy when the batch is large: widen leaf
        // scopes to parent directories instead of reconciling everything.
        let leaf_scopes: Vec<String> = self
            .touched_files
            .iter()
            .chain(self.deleted_prefixes.iter())
            .cloned()
            .collect();
        if !leaf_scopes.is_empty() {
            self.touched_files.clear();
            self.deleted_prefixes.clear();
            for path in &leaf_scopes {
                self.rescan_directories.insert(parent_scope(&root, path));
            }
            self.collapse_paths();
        }
        while self.len() >= self.max_changed_paths {
            let previous = self.len();
            let widened: Vec<String> = self
                .rescan_directories
                .iter()
                .map(|path| parent_scope(&root, path))
                .collect();
            self.rescan_directories.clear();
            self.rescan_directories.extend(widened);
            self.collapse_paths();
            if self.len() >= previous {
                break;
            }
        }
    }

    fn covered_by(paths: &BTreeSet<String>, path: &str) -> bool {
        paths.contains(path) || has_ancestor(paths, path)
    }
}

/// Normalizes a change path the way TS `normalizePath` does on posix:
/// forward slashes, no trailing slash except the filesystem root.
fn normalize_change_path(path: &str) -> String {
    let mut out = path.replace('\\', "/");
    while out.len() > 1 && out.ends_with('/') {
        out.pop();
    }
    out
}

fn file_name(path: &str) -> &str {
    Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(path)
}

fn parent_dir(path: &str) -> String {
    let parent = Path::new(path)
        .parent()
        .map_or("/", |parent| parent.to_str().unwrap_or("/"));
    if parent.is_empty() {
        return "/".to_owned();
    }
    normalize_change_path(parent)
}

/// Drops every path that has an ancestor already in the set.
fn collapse_set(paths: &mut BTreeSet<String>) {
    let mut sorted: Vec<String> = paths.iter().cloned().collect();
    sorted.sort_by_key(|path| path.len());
    for path in sorted {
        if has_ancestor(paths, &path) {
            paths.remove(&path);
        }
    }
}

/// True when any strict ancestor directory of `target` is in `paths`.
fn has_ancestor(paths: &BTreeSet<String>, target: &str) -> bool {
    let mut current = parent_dir(target);
    loop {
        if paths.contains(&current) {
            return true;
        }
        let parent = parent_dir(&current);
        if parent == current {
            return false;
        }
        current = parent;
    }
}

/// Widening scope for an over-budget path: its parent directory, clamped
/// to `root` when the parent escapes it.
fn parent_scope(root: &str, path: &str) -> String {
    if path == root {
        return path.to_owned();
    }
    let parent = parent_dir(path);
    match Path::new(&parent).strip_prefix(Path::new(root)) {
        Ok(_) => parent,
        Err(_) => root.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn changes() -> ChangeSet {
        ChangeSet::new(ChangeSetOptions {
            root: Some("/repo".to_owned()),
            max_changed_paths: None,
        })
    }

    #[test]
    fn accumulates_created_changed_deleted() {
        let mut set = changes();
        set.add("/repo/a.rs", ChangeKind::Created, false).unwrap();
        set.add("/repo/b.rs", ChangeKind::Changed, false).unwrap();
        set.add("/repo/gone.rs", ChangeKind::Deleted, false)
            .unwrap();
        set.add("/repo/sub", ChangeKind::Created, true).unwrap();
        let snapshot = set.snapshot();
        assert_eq!(snapshot.touched_files, vec!["/repo/a.rs", "/repo/b.rs"]);
        assert_eq!(snapshot.rescan_directories, vec!["/repo/sub"]);
        assert_eq!(snapshot.deleted_prefixes, vec!["/repo/gone.rs"]);
        assert!(!snapshot.force_full_reconcile);
    }

    #[test]
    fn covered_paths_are_dropped() {
        let mut set = changes();
        set.add("/repo/sub", ChangeKind::Created, true).unwrap();
        // Covered by the rescan directory: dropped silently.
        set.add("/repo/sub/file.rs", ChangeKind::Changed, false)
            .unwrap();
        let snapshot = set.snapshot();
        assert!(snapshot.touched_files.is_empty());
    }

    #[test]
    fn gitignore_widens_to_parent() {
        let mut set = changes();
        set.add("/repo/sub/.gitignore", ChangeKind::Changed, false)
            .unwrap();
        let snapshot = set.snapshot();
        assert_eq!(snapshot.rescan_directories, vec!["/repo/sub"]);
    }

    #[test]
    fn relative_paths_are_rejected() {
        let mut set = changes();
        let err = set
            .add("relative/path.rs", ChangeKind::Changed, false)
            .unwrap_err();
        assert_eq!(
            err,
            DaemonError::RootNotAbsolute {
                root: "relative/path.rs".to_owned()
            }
        );
    }

    #[test]
    fn over_budget_widens_before_reconciling() {
        let mut set = ChangeSet::new(ChangeSetOptions {
            root: Some("/repo".to_owned()),
            max_changed_paths: Some(MaxChangedPaths::new(2)),
        });
        set.add("/repo/a.rs", ChangeKind::Changed, false).unwrap();
        set.add("/repo/b.rs", ChangeKind::Changed, false).unwrap();
        let snapshot = set.snapshot();
        assert!(!snapshot.force_full_reconcile);
        assert_eq!(snapshot.rescan_directories, vec!["/repo"]);
    }

    #[test]
    fn no_root_forces_full_reconcile_over_budget() {
        let mut set = ChangeSet::new(ChangeSetOptions {
            root: None,
            max_changed_paths: Some(MaxChangedPaths::new(1)),
        });
        set.add("/repo/a.rs", ChangeKind::Changed, false).unwrap();
        assert!(set.snapshot().force_full_reconcile);
    }
}
