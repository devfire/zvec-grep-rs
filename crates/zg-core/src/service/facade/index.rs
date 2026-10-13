//! Workspace indexing for the facade: manifest assembly, root-path
//! resolution, and the blocking index run with abort plumbing.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::models::embedding_schema;
use super::service::ZvecGrepService;
use super::signal::{finish_signal_watch, spawn_signal_watch};
use crate::config::EmbeddingRuntimeConfig;
use crate::error::EngineResult;
use crate::manifest::{
    CURRENT_MANIFEST_VERSION, WorkspaceManifest, delete_workspace_manifest,
    read_workspace_manifest, write_workspace_manifest,
};
use crate::paths::to_display_path;
use crate::pipeline::indexing::scanner::CancelFlag;
use crate::service::root::{
    WorkspaceIndexLocation, acquire_index_maintenance_guard, has_workspace_index,
    reset_workspace_index,
};
use crate::service::types::{RootPathSpec, ZvecGrepIndexOptions};
use crate::service::workspace_index::{
    IndexMode, IndexOptions, WorkspaceIndex, WorkspaceIndexOptions, is_workspace_indexed,
};
use crate::storage::layout::delete_workspace_index_storage;
use crate::types::{CURRENT_INDEX_VERSION, RootPath, UnixMillis, WorkspaceIndexInfo};

impl ZvecGrepService {
    /// Indexes the workspace, creating the manifest and storage on first run.
    ///
    /// Progress flows through the owned [`IndexProgressSink`](crate::pipeline::indexing::IndexProgressSink);
    /// cancellation through `options.signal`, polled on a helper thread that
    /// trips a [`CancelFlag`] shared with the blocking index run.
    ///
    /// # Errors
    ///
    /// Returns an error when root or manifest handling fails, no embedding model resolves, the
    /// index cannot be opened, or the indexing run fails.
    pub fn ensure_index(
        &self,
        options: &ZvecGrepIndexOptions<'_>,
    ) -> EngineResult<crate::types::IndexResult> {
        let root = self.root_string(options.root);
        let location = self.location_for(&root, options.index_location)?;
        let home = PathBuf::from(&location.home);
        let existing = read_workspace_manifest(&home)?;
        let model = self.model_for_manifest(existing.as_ref())?;

        // Exclusive maintenance guard spanning deletes + manifest write: the
        // same write lock `WorkspaceIndex::open` takes below. A live
        // reader/writer or concurrent rebuild fails BUSY here instead of
        // racing us. Dropped before `open`, which acquires the same lock for
        // creation + the indexing run.
        //
        // `fresh_build` doubles as the #50 version gate below: only a first
        // build or rebuild withholds the stamp pre-run (#39 torn-build
        // guard). Incremental runs keep the existing stamp so a failed or
        // cancelled run stays searchable instead of bricking the index.
        let fresh_build = options.rebuild || !is_indexed(existing.as_ref());
        let maintenance = if fresh_build {
            let guard = acquire_index_maintenance_guard(&home, "index.rebuild")?;
            delete_workspace_manifest(&home)?;
            delete_workspace_index_storage(&home)?;
            Some(guard)
        } else {
            None
        };
        // A rebuild wipes storage, not configuration: the prior manifest
        // still supplies the workspace id, creation time, embedding
        // runtime, and — via `resolve_root_paths` — the stored root paths
        // (nested-git policy included) unless the caller replaces or
        // resets them. Discarding it here made every default-options
        // rebuild (e.g. the daemon catch-up reconcile) silently revert
        // the index to a bare default root.
        // #50: incremental runs carry the existing stamp through the
        // pre-run write; only fresh builds run versionless. No `unwrap`:
        // a missing stamp degrades to the fresh-build shape, never a panic.
        let pre_run_version = if fresh_build {
            None
        } else {
            existing
                .as_ref()
                .and_then(|manifest| manifest.info.index_version)
        };

        let root_paths = Self::resolve_root_paths(&location, options, existing.as_ref())?;
        let now = UnixMillis::now();
        let manifest = WorkspaceManifest {
            info: WorkspaceIndexInfo {
                id: existing
                    .as_ref()
                    .map_or_else(new_workspace_id, |manifest| manifest.info.id.clone()),
                name: existing.as_ref().map_or_else(
                    || workspace_name(&location),
                    |manifest| manifest.info.name.clone(),
                ),
                // Storage resolves against `info.path`, so it must be the
                // `.zvec-grep-rs` home, not the workspace root (mirrors
                // `prepareWorkspaceManifest`: `path: location.home`).
                path: location.home.clone(),
                root_paths,
                index_policy: Some(crate::types::WorkspaceIndexPolicy::Enabled),
                embedding: Some(Some(embedding_schema(model.as_ref()))),
                // Fresh builds withhold the stamp until the run below
                // commits (#39): a kill between this write and completion
                // leaves a manifest that reads as incomplete, never a torn
                // complete. Incremental runs (#50) keep the existing stamp
                // so a failed or cancelled run stays searchable and the
                // next run resumes incrementally instead of wiping storage.
                index_version: pre_run_version,
                created_time: existing
                    .as_ref()
                    .map_or(now, |manifest| manifest.info.created_time),
                updated_time: now,
            },
            manifest_version: CURRENT_MANIFEST_VERSION,
            embedding_runtime: existing
                .as_ref()
                .map_or_else(EmbeddingRuntimeConfig::default, |manifest| {
                    manifest.embedding_runtime.clone()
                }),
        };
        write_workspace_manifest(&home, &manifest)?;
        // Handoff: replacement storage creation takes the same write lock.
        drop(maintenance);
        let cancel = CancelFlag::new();
        let watcher = spawn_signal_watch(options.signal.clone(), &cancel);
        // Fresh builds persist versionless (incomplete) while the run is in
        // flight; only this in-memory copy claims the current version so
        // `open` + validation succeed for the run itself. Incremental runs
        // already persist the carried stamp above.
        let open_info = WorkspaceIndexInfo {
            index_version: Some(CURRENT_INDEX_VERSION),
            ..manifest.info.clone()
        };
        let mut index = WorkspaceIndex::open(
            open_info,
            WorkspaceIndexOptions {
                mode: IndexMode::Write,
                embedding_model: Some(Arc::clone(&model)),
            },
        )?;
        let changed_paths = if options.changed_paths.is_empty() {
            None
        } else {
            Some(
                options
                    .changed_paths
                    .iter()
                    .map(|path| to_display_path(path))
                    .collect(),
            )
        };
        let result = index.index(&IndexOptions {
            embedding_concurrency: options.embedding_concurrency,
            on_progress: options.on_progress.clone(),
            changed_paths,
            cancel: Some(cancel),
        });
        index.close();
        finish_signal_watch(watcher);

        let result = result?;
        // Commit: a successful run stamps the current version, flipping a
        // fresh pre-run versionless manifest back to complete and refreshing
        // an incremental run's carried stamp.
        let manifest = WorkspaceManifest {
            info: WorkspaceIndexInfo {
                updated_time: UnixMillis::now(),
                index_version: Some(CURRENT_INDEX_VERSION),
                ..manifest.info
            },
            ..manifest
        };
        write_workspace_manifest(&home, &manifest)?;
        Ok(result)
    }

    /// Deletes the manifest and index storage; `Ok(false)` when nothing
    /// existed, mirroring `dropIndex`.
    ///
    /// # Errors
    ///
    /// Returns an error when the workspace root cannot be resolved or stored index data cannot
    /// be deleted.
    pub fn drop_index(&self, root: Option<&Path>) -> EngineResult<bool> {
        let location = self.location_for(&self.root_string(root), None)?;
        if !has_workspace_index(&location) {
            return Ok(false);
        }
        reset_workspace_index(&location)?;
        Ok(true)
    }

    fn resolve_root_paths(
        location: &WorkspaceIndexLocation,
        options: &ZvecGrepIndexOptions<'_>,
        existing: Option<&WorkspaceManifest>,
    ) -> EngineResult<Vec<RootPath>> {
        if !options.root_paths.is_empty() {
            let mut roots = Vec::with_capacity(options.root_paths.len());
            for spec in &options.root_paths {
                roots.push(Self::convert_root_spec(location, options, spec));
            }
            return crate::pipeline::indexing::root_paths::validate_root_paths(&roots);
        }
        if !options.reset_paths
            && let Some(manifest) = existing
            && !manifest.info.root_paths.is_empty()
        {
            return Ok(manifest.info.root_paths.clone());
        }
        crate::pipeline::indexing::root_paths::validate_root_paths(&[RootPath {
            absolute_path: location.root.clone(),
            recursive: true,
            include: options.include_paths.clone(),
            exclude: options.exclude_paths.clone(),
            globs: options.globs.clone(),
            insensitive_globs: options.insensitive_globs.clone(),
            file_types: options.file_types.clone(),
            excluded_file_types: options.excluded_file_types.clone(),
            hidden: options.hidden,
            no_ignore: options.no_ignore,
            ignore_files: options.ignore_files.clone(),
            max_depth: options.max_depth,
            max_file_size_bytes: options.max_file_size_bytes,
            follow: options.follow,
            include_nested_git: options.include_nested_git,
        }])
    }

    fn convert_root_spec(
        location: &WorkspaceIndexLocation,
        options: &ZvecGrepIndexOptions<'_>,
        spec: &RootPathSpec<'_>,
    ) -> RootPath {
        match spec {
            RootPathSpec::Full(root) => root.as_ref().clone(),
            RootPathSpec::Path(path) => {
                let absolute = if Path::new(path).is_absolute() {
                    path.to_string()
                } else {
                    to_display_path(&Path::new(&location.root).join(path))
                };
                RootPath {
                    absolute_path: absolute,
                    recursive: true,
                    include: options.include_paths.clone(),
                    exclude: options.exclude_paths.clone(),
                    globs: options.globs.clone(),
                    insensitive_globs: options.insensitive_globs.clone(),
                    file_types: options.file_types.clone(),
                    excluded_file_types: options.excluded_file_types.clone(),
                    hidden: options.hidden,
                    no_ignore: options.no_ignore,
                    ignore_files: options.ignore_files.clone(),
                    max_depth: options.max_depth,
                    max_file_size_bytes: options.max_file_size_bytes,
                    follow: options.follow,
                    include_nested_git: options.include_nested_git,
                }
            }
        }
    }
}

fn is_indexed(manifest: Option<&WorkspaceManifest>) -> bool {
    manifest
        .as_ref()
        .is_some_and(|manifest| is_workspace_indexed(&manifest.info))
}

fn new_workspace_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn workspace_name(location: &WorkspaceIndexLocation) -> String {
    Path::new(&location.root).file_name().map_or_else(
        || location.root.clone(),
        |name| name.to_string_lossy().into_owned(),
    )
}
