//! `ZvecGrepService` facade: the sync engine entry point.
//!
//! Mirrors `../zvec-grep/src/engine/service/zvec-grep.ts` (`createZvecGrep`,
//! `ZvecGrepService`, `openWorkspaceReadSession`). Every method delegates to
//! the existing [`WorkspaceIndex`],
//! [`pipeline::indexing`](crate::pipeline::indexing), and
//! [`pipeline::search`](crate::pipeline::search) pieces; the facade owns no
//! timers and no caches.
//!
//! Divergence notes (see `docs/ts-divergence.md`): the facade is sync —
//! [`EmbeddingModel::embed`] is sync,
//! so no tokio dependency is needed here and the async wrapping happens in
//! the daemon (phase G) via `spawn_blocking`. Read sessions are explicit
//! RAII guards ([`ReadSession`]); idle-TTL eviction belongs to the daemon,
//! the only layer with a runtime. The method is named [`context`](ZvecGrepService::context)
//! after the TS method and the `ZvecGrepContext*` DTOs, not `search`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::config::EmbeddingRuntimeConfig;
use crate::error::{EngineError, EngineResult, codes};
use crate::lexical::{LexicalSearchOptions, LexicalSearchResult, run_lexical_search};
use crate::manifest::{
    CURRENT_MANIFEST_VERSION, WorkspaceManifest, read_workspace_manifest, write_workspace_manifest,
};
use crate::models::catalog::{EmbeddingCatalogEntry, ModelReference};
use crate::models::embeddings::{CreateEmbeddingModelOptions, DeviceKind};
use crate::models::factory::create_embedding_model;
use crate::models::resolution::{ResolveEmbeddingReferenceOptions, resolve_embedding_reference};
use crate::models::{EmbeddingModel, EmbeddingModelInfo};
use crate::paths::to_display_path;
use crate::pipeline::indexing::scanner::CancelFlag;
use crate::service::root::{
    WorkspaceIndexLocation, find_nearest_workspace_index, has_workspace_index,
    reset_workspace_index, resolve_zvec_grep_root, workspace_index_location,
};
use crate::service::types::{
    ContentStatus, ContextCoverage, ContextDiagnostics, ContextFile, ContextItem, ContextItemKind,
    ContextSource, EmbeddingInfo, GroupResult, GroupRole, QueryGroupRef, RootPathSpec,
    SelectionReason, ZvecGrepContextOptions, ZvecGrepContextResult, ZvecGrepIndexOptions,
    ZvecGrepInfoResult, empty_query_error, workspace_index_disabled, workspace_index_not_found,
};
use crate::service::workspace_index::{
    IndexMode, IndexOptions, WorkspaceIndex, WorkspaceIndexOptions, is_workspace_indexed,
};
use crate::types::{
    CURRENT_INDEX_VERSION, RootPath, SearchHit, SearchMatchedBy, SearchPlan, SearchPlanRoute,
    SearchPlanRouteMode, TimingEntry, UnixMillis, WorkspaceIndexEmbeddingSchema,
    WorkspaceIndexInfo, WorkspaceIndexStatus,
};
/// Fallback embedding model when nothing selects one, mirroring
/// `DEFAULT_LOCAL_EMBEDDING` in the TS service.
pub const DEFAULT_EMBEDDING_REFERENCE: &str = "local/potion-code-16m-v2";

/// Default result limit, mirroring `DEFAULT_CONTEXT_LIMIT`.
pub const DEFAULT_CONTEXT_LIMIT: usize = 10;

/// Default total result budget across groups, mirroring
/// `DEFAULT_CONTEXT_TOTAL_LIMIT`.
pub const DEFAULT_CONTEXT_TOTAL_LIMIT: usize = 30;

/// Prioritized (coverage + fill) item cap, mirroring
/// `DEFAULT_CONTEXT_PRIORITY_LIMIT`.
const DEFAULT_CONTEXT_PRIORITY_LIMIT: usize = 6;

/// Reciprocal-rank-fusion constant, mirroring `CONTEXT_GROUP_RRF_K`.
const CONTEXT_GROUP_RRF_K: f64 = 60.0;

/// How often the `AbortCheck` poll thread probes during indexing.
const SIGNAL_POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Options for [`create_zvec_grep`], mirroring `CreateZvecGrepOptions`.
///
/// No `Debug`: the injected model handle is an opaque trait object.
#[derive(Clone, Default)]
pub struct CreateZvecGrepOptions {
    /// Workspace root; defaults to the process working directory.
    pub root: Option<PathBuf>,
    /// Explicit embedding model, winning over manifest/env/defaults.
    pub embedding: Option<ModelReference>,
    /// Injected model handle (tests, daemon pool leases). Wins over every
    /// catalog path and never touches the network.
    pub embedding_model: Option<Arc<dyn EmbeddingModel>>,
    /// API key for remote embedding providers.
    pub api_key: Option<String>,
    /// Endpoint override for remote embedding providers.
    pub endpoint: Option<String>,
    /// Local model cache directory override.
    pub model_cache_dir: Option<PathBuf>,
}

/// Builds a [`ZvecGrepService`], mirroring `createZvecGrep`.
#[must_use]
pub fn create_zvec_grep(options: CreateZvecGrepOptions) -> ZvecGrepService {
    ZvecGrepService::new(options)
}

/// Sync engine facade over one workspace root.
pub struct ZvecGrepService {
    root: String,
    embedding: Option<ModelReference>,
    embedding_model: Option<Arc<dyn EmbeddingModel>>,
    api_key: Option<String>,
    endpoint: Option<String>,
    model_cache_dir: Option<PathBuf>,
}

impl ZvecGrepService {
    /// Binds the facade to `options.root` (or the working directory).
    pub fn new(options: CreateZvecGrepOptions) -> Self {
        let root = options.root.as_deref().map(Path::to_string_lossy);
        let root = root
            .as_deref()
            .and_then(|root| resolve_zvec_grep_root(Some(root)).ok());
        let root =
            root.unwrap_or_else(|| resolve_zvec_grep_root(None).unwrap_or_else(|_| ".".to_owned()));
        Self {
            root,
            embedding: options.embedding,
            embedding_model: options.embedding_model,
            api_key: options.api_key,
            endpoint: options.endpoint,
            model_cache_dir: options.model_cache_dir,
        }
    }

    /// Workspace root the facade is bound to.
    #[must_use]
    pub fn root(&self) -> &str {
        &self.root
    }

    /// Resolves `root` (or the bound root) to its index locations, mirroring
    /// `openWorkspace` location handling.
    ///
    /// # Errors
    ///
    /// Returns `WORKSPACE.ROOT_UNAVAILABLE` when the workspace root cannot be resolved.
    pub fn open_workspace(&self, root: Option<&Path>) -> EngineResult<WorkspaceIndexLocation> {
        workspace_index_location(&self.root_string(root))
    }

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
        let location = workspace_index_location(&root)?;
        let home = PathBuf::from(&location.home);
        let existing = read_workspace_manifest(&home)?;
        let model = self.model_for_manifest(existing.as_ref())?;

        if options.rebuild || !is_indexed(existing.as_ref()) {
            reset_workspace_index(&location)?;
        }
        let existing = if options.rebuild { None } else { existing };

        let root_paths = self.resolve_root_paths(&location, options, existing.as_ref())?;
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
                // `.zvec-grep` home, not the workspace root (mirrors
                // `prepareWorkspaceManifest`: `path: location.home`).
                path: location.home.clone(),
                root_paths,
                index_policy: Some(crate::types::WorkspaceIndexPolicy::Enabled),
                embedding: Some(Some(embedding_schema(model.as_ref()))),
                index_version: Some(CURRENT_INDEX_VERSION),
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

        let cancel = CancelFlag::new();
        let watcher = spawn_signal_watch(options.signal.clone(), &cancel);
        let mut index = WorkspaceIndex::open(
            manifest.info.clone(),
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
        let manifest = WorkspaceManifest {
            info: WorkspaceIndexInfo {
                updated_time: UnixMillis::now(),
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
        let location = workspace_index_location(&self.root_string(root))?;
        if !has_workspace_index(&location) {
            return Ok(false);
        }
        reset_workspace_index(&location)?;
        Ok(true)
    }

    /// Hybrid search over the workspace index, mirroring `context()`.
    ///
    /// With `auto_update` set, a stale index is refreshed first via
    /// [`ensure_index`](Self::ensure_index); read sessions (phase G) always
    /// pass it cleared.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty query, when no built index is found, or when model
    /// resolution, index open, or search fails.
    pub fn context(
        &self,
        options: &ZvecGrepContextOptions<'_>,
    ) -> EngineResult<ZvecGrepContextResult> {
        let request = normalize_context_request(options)?;
        let location = self.require_indexed_location(options.root)?;
        let manifest = self.require_manifest(&location)?;
        if options.wants_auto_update() {
            self.refresh(&location)?;
            return self.context_after_refresh(options, &request);
        }
        let model = self.model_for_manifest(Some(&manifest))?;
        let info = manifest.info.clone();
        let index = WorkspaceIndex::open(
            info.clone(),
            WorkspaceIndexOptions {
                mode: IndexMode::Read,
                embedding_model: Some(model),
            },
        )?;
        run_context_search(&index, &location.root, &info, &request, options)
    }

    /// Exhaustive in-process lexical search — never a subprocess, mirroring
    /// `rg` handling via `lexical/mod.rs`.
    ///
    /// # Errors
    ///
    /// Returns an error when search paths are invalid or the in-process walk fails.
    pub fn rg_search(&self, options: &LexicalSearchOptions) -> EngineResult<LexicalSearchResult> {
        run_lexical_search(options)
    }

    /// Index freshness derived from stored files, mirroring `indexStatus`.
    ///
    /// # Errors
    ///
    /// Returns an error when no built index is found or the index cannot be opened for status.
    pub fn index_status(&self, root: Option<&Path>) -> EngineResult<WorkspaceIndexStatus> {
        let location = self.require_indexed_location(root)?;
        let manifest = self.require_manifest(&location)?;
        let index = WorkspaceIndex::open(
            manifest.info.clone(),
            WorkspaceIndexOptions {
                mode: IndexMode::Read,
                embedding_model: None,
            },
        )?;
        index.status()
    }

    /// Workspace identity, policy, embedding, and status, mirroring `info()`.
    ///
    /// # Errors
    ///
    /// Returns an error when the manifest cannot be read or the workspace index is disabled.
    pub fn workspace_info(&self, root: Option<&Path>) -> EngineResult<ZvecGrepInfoResult> {
        let start = self.root_string(root);
        let Some(location) = find_nearest_workspace_index(&start) else {
            return Ok(not_indexed_result(&start));
        };
        let manifest = self.require_manifest(&location)?;
        if manifest.info.index_policy == Some(crate::types::WorkspaceIndexPolicy::Disabled) {
            return Err(workspace_index_disabled(&location.root));
        }
        let indexed = is_workspace_indexed(&manifest.info);
        let embedding = manifest
            .info
            .embedding
            .clone()
            .flatten()
            .map(|schema| EmbeddingInfo {
                provider: schema.provider,
                model: schema.model,
                dimension: schema.dimension,
                metric: schema.metric,
            });
        let (status, suggestion) = if indexed {
            let status = WorkspaceIndex::open(
                manifest.info.clone(),
                WorkspaceIndexOptions {
                    mode: IndexMode::Read,
                    embedding_model: None,
                },
            )
            .and_then(|index| index.status())
            .ok();
            (status, None)
        } else {
            (
                None,
                Some("workspace is not indexed; run zg index first".to_owned()),
            )
        };
        Ok(ZvecGrepInfoResult {
            root: location.root,
            indexed,
            index_policy: manifest.info.index_policy,
            embedding,
            workspace_index: Some(manifest.info.clone()),
            status,
            suggestion,
        })
    }

    /// Opens an explicit RAII read session; the daemon owns TTL eviction.
    ///
    /// # Errors
    ///
    /// Returns an error when no built index is found, the index is disabled or unbuilt, or model
    /// resolution or index open fails.
    pub fn open_read_session(&self, root: Option<&Path>) -> EngineResult<ReadSession> {
        let location = self.require_indexed_location(root)?;
        let manifest = self.require_manifest(&location)?;
        if manifest.info.index_policy == Some(crate::types::WorkspaceIndexPolicy::Disabled) {
            return Err(workspace_index_disabled(&location.root));
        }
        if !is_workspace_indexed(&manifest.info) {
            return Err(workspace_index_not_found(&location.root));
        }
        let model = self.model_for_manifest(Some(&manifest))?;
        let index = WorkspaceIndex::open(
            manifest.info.clone(),
            WorkspaceIndexOptions {
                mode: IndexMode::Read,
                embedding_model: Some(model),
            },
        )?;
        Ok(ReadSession {
            root: location.root,
            info: manifest.info,
            index: Some(index),
        })
    }

    fn refresh(&self, location: &WorkspaceIndexLocation) -> EngineResult<()> {
        let root = PathBuf::from(&location.root);
        self.ensure_index(&ZvecGrepIndexOptions {
            root: Some(&root),
            ..ZvecGrepIndexOptions::default()
        })?;
        Ok(())
    }

    fn context_after_refresh(
        &self,
        options: &ZvecGrepContextOptions<'_>,
        request: &NormalizedContextRequest,
    ) -> EngineResult<ZvecGrepContextResult> {
        let location = self.require_indexed_location(options.root)?;
        let manifest = self.require_manifest(&location)?;
        let model = self.model_for_manifest(Some(&manifest))?;
        let info = manifest.info.clone();
        let index = WorkspaceIndex::open(
            info.clone(),
            WorkspaceIndexOptions {
                mode: IndexMode::Read,
                embedding_model: Some(model),
            },
        )?;
        run_context_search(&index, &location.root, &info, request, options)
    }

    fn root_string(&self, root: Option<&Path>) -> String {
        root.map_or_else(|| self.root.clone(), to_display_path)
    }

    fn require_indexed_location(
        &self,
        root: Option<&Path>,
    ) -> EngineResult<WorkspaceIndexLocation> {
        let start = self.root_string(root);
        find_nearest_workspace_index(&start).ok_or_else(|| workspace_index_not_found(&start))
    }

    fn require_manifest(
        &self,
        location: &WorkspaceIndexLocation,
    ) -> EngineResult<WorkspaceManifest> {
        read_workspace_manifest(Path::new(&location.home))?
            .ok_or_else(|| workspace_index_not_found(&location.root))
    }

    fn model_for_manifest(
        &self,
        manifest: Option<&WorkspaceManifest>,
    ) -> EngineResult<Arc<dyn EmbeddingModel>> {
        if let Some(model) = &self.embedding_model {
            return Ok(Arc::clone(model));
        }
        let existing = manifest.and_then(schema_reference);
        let reference = resolve_embedding_reference(&ResolveEmbeddingReferenceOptions {
            explicit: self.embedding.clone(),
            existing,
            global_default: None,
            environment: None,
            fallback: Some(ModelReference::new(DEFAULT_EMBEDDING_REFERENCE)),
        })?
        .ok_or_else(|| {
            EngineError::new(
                codes::config_invalid_embedding_runtime(),
                "no embedding model is selected",
            )
        })?;
        let options = CreateEmbeddingModelOptions {
            api_key: self.api_key.clone(),
            endpoint: self.endpoint.clone(),
            model_cache_dir: self.model_cache_dir.clone(),
            device: DeviceKind::Auto,
        };
        create_embedding_model(&reference, &options).map_err(EngineError::from)
    }

    fn resolve_root_paths(
        &self,
        location: &WorkspaceIndexLocation,
        options: &ZvecGrepIndexOptions<'_>,
        existing: Option<&WorkspaceManifest>,
    ) -> EngineResult<Vec<RootPath>> {
        if !options.root_paths.is_empty() {
            let mut roots = Vec::with_capacity(options.root_paths.len());
            for spec in &options.root_paths {
                roots.push(self.convert_root_spec(location, options, spec)?);
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
        }])
    }

    fn convert_root_spec(
        &self,
        location: &WorkspaceIndexLocation,
        options: &ZvecGrepIndexOptions<'_>,
        spec: &RootPathSpec<'_>,
    ) -> EngineResult<RootPath> {
        match spec {
            RootPathSpec::Full(root) => Ok(root.as_ref().clone()),
            RootPathSpec::Path(path) => {
                let absolute = if Path::new(path).is_absolute() {
                    path.to_string()
                } else {
                    to_display_path(&Path::new(&location.root).join(path))
                };
                Ok(RootPath {
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
                })
            }
        }
    }
}

impl std::fmt::Debug for ZvecGrepService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ZvecGrepService")
            .field("root", &self.root)
            .field("embedding", &self.embedding)
            .field("has_embedding_model", &self.embedding_model.is_some())
            .finish()
    }
}

/// Explicit read-session guard: `context()` with `auto_update` forced off.
///
/// The facade holds no timers; TTL eviction belongs to the daemon (phase G).
/// Closing is RAII — [`close`](ReadSession::close) consumes the guard and
/// [`Drop`] closes the storage handle either way.
pub struct ReadSession {
    root: String,
    info: WorkspaceIndexInfo,
    index: Option<WorkspaceIndex>,
}

impl ReadSession {
    /// Workspace root the session was opened for.
    #[must_use]
    pub fn root(&self) -> &str {
        &self.root
    }

    /// Searches through the open read handle; errors once closed.
    ///
    /// # Errors
    ///
    /// Returns an error when the session is closed, the query is empty, or search fails.
    pub fn context(
        &self,
        options: &ZvecGrepContextOptions<'_>,
    ) -> EngineResult<ZvecGrepContextResult> {
        let Some(index) = self.index.as_ref() else {
            return Err(EngineError::new(
                codes::service_read_session_closed(),
                "workspace read session is already closed",
            ));
        };
        let request = normalize_context_request(options)?;
        run_context_search(index, &self.root, &self.info, &request, options)
    }

    /// Consumes the guard and closes the storage handle.
    pub fn close(mut self) {
        if let Some(index) = self.index.as_mut() {
            index.close();
        }
        self.index = None;
    }
}

impl Drop for ReadSession {
    fn drop(&mut self) {
        if let Some(index) = self.index.as_mut() {
            index.close();
        }
    }
}

fn is_indexed(manifest: Option<&WorkspaceManifest>) -> bool {
    manifest
        .as_ref()
        .is_some_and(|manifest| is_workspace_indexed(&manifest.info))
}

fn not_indexed_result(root: &str) -> ZvecGrepInfoResult {
    ZvecGrepInfoResult {
        root: root.to_owned(),
        indexed: false,
        suggestion: Some("workspace is not indexed; run zg index first".to_owned()),
        ..ZvecGrepInfoResult::default()
    }
}

fn new_workspace_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn workspace_name(location: &WorkspaceIndexLocation) -> String {
    Path::new(&location.root)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| location.root.clone())
}

fn embedding_schema(model: &dyn EmbeddingModel) -> WorkspaceIndexEmbeddingSchema {
    let info: &EmbeddingModelInfo = model.info();
    WorkspaceIndexEmbeddingSchema {
        provider: info.provider.clone(),
        model: info.model.clone(),
        dimension: info.dimension,
        metric: info.metric,
    }
}

/// Maps a recorded embedding schema back to its catalog reference by
/// provider + model identity.
fn schema_reference(manifest: &WorkspaceManifest) -> Option<ModelReference> {
    let schema = manifest.info.embedding.clone().flatten()?;
    let entry = get_embedding_model_catalog_entry_for_schema(&schema.provider, &schema.model)?;
    Some(ModelReference::new(entry))
}

fn get_embedding_model_catalog_entry_for_schema(
    provider: &str,
    model: &str,
) -> Option<&'static str> {
    crate::models::catalog::EMBEDDING_MODEL_CATALOG
        .iter()
        .find_map(|entry| {
            let (entry_provider, entry_model, reference) = match entry {
                EmbeddingCatalogEntry::LlamaCpp(entry) => {
                    (entry.provider, entry.model, entry.reference)
                }
                EmbeddingCatalogEntry::QwenText(entry) => {
                    (entry.provider, entry.model, entry.reference)
                }
                EmbeddingCatalogEntry::QwenMultimodal(entry) => {
                    (entry.provider, entry.model, entry.reference)
                }
                EmbeddingCatalogEntry::TransformersJs(entry) => {
                    (entry.provider, entry.model, entry.reference)
                }
                EmbeddingCatalogEntry::Model2Vec(entry) => {
                    (entry.provider, entry.model, entry.reference)
                }
            };
            (entry_provider == provider && entry_model == model).then_some(reference)
        })
}

/// One normalized query group (`Q1`… with its display query, role, and
/// routes), mirroring TS `NormalizedContextGroup`.
#[derive(Debug, Clone)]
struct ContextGroup {
    id: String,
    query: String,
    role: GroupRole,
    routes: Vec<SearchPlanRoute>,
}

/// Normalized context request: the display query plus per-group routes,
/// mirroring TS `NormalizedContextRequest`.
#[derive(Debug, Clone)]
struct NormalizedContextRequest {
    display_query: String,
    groups: Vec<ContextGroup>,
}

/// Normalizes context options into per-group hybrid routes, mirroring TS
/// `normalizeContextRequest`/`normalizePrimaryQueries`/`contextGroups`:
/// every primary query becomes one primary group with an FTS+vector route
/// pair, and every extra route becomes one supplemental group.
///
/// # Errors
///
/// Returns [`empty_query_error`] when no primary query and no extra route is
/// present, or `SERVICE.EMPTY_ROUTE_QUERY` for a blank extra route.
fn normalize_context_request(
    options: &ZvecGrepContextOptions<'_>,
) -> EngineResult<NormalizedContextRequest> {
    let primaries: Vec<String> = options
        .query
        .iter()
        .chain(options.queries.iter())
        .map(|query| query.trim().to_owned())
        .filter(|query| !query.is_empty())
        .collect();
    let extras = normalize_context_routes(options)?;
    if primaries.is_empty() && extras.is_empty() {
        return Err(empty_query_error());
    }
    let display_query = if primaries.is_empty() {
        extras
            .iter()
            .map(|route| route.query.as_str())
            .collect::<Vec<_>>()
            .join(" | ")
    } else {
        primaries.join(" | ")
    };
    let mut groups = Vec::with_capacity(primaries.len() + extras.len());
    for (index, query) in primaries.iter().enumerate() {
        groups.push(ContextGroup {
            id: format!("Q{}", index + 1),
            query: query.clone(),
            role: GroupRole::Primary,
            routes: vec![
                SearchPlanRoute {
                    mode: SearchPlanRouteMode::Fts,
                    query: query.clone(),
                },
                SearchPlanRoute {
                    mode: SearchPlanRouteMode::Vector,
                    query: query.clone(),
                },
            ],
        });
    }
    let offset = groups.len();
    for (index, route) in extras.into_iter().enumerate() {
        groups.push(ContextGroup {
            id: format!("Q{}", offset + index + 1),
            query: route.query.clone(),
            role: GroupRole::Supplemental,
            routes: vec![route],
        });
    }
    Ok(NormalizedContextRequest {
        display_query,
        groups,
    })
}

/// Trims extra routes and rejects blank ones before they consume a group
/// slot, mirroring TS `normalizeContextRoutes`.
///
/// # Errors
///
/// Returns `SERVICE.EMPTY_ROUTE_QUERY` for a blank route query.
fn normalize_context_routes(
    options: &ZvecGrepContextOptions<'_>,
) -> EngineResult<Vec<SearchPlanRoute>> {
    let mut extras: Vec<SearchPlanRoute> = Vec::new();
    extras.extend(options.routes.iter().cloned());
    extras.extend(options.fts.iter().map(|term| SearchPlanRoute {
        mode: SearchPlanRouteMode::Fts,
        query: term.clone(),
    }));
    extras.extend(options.vector.iter().map(|term| SearchPlanRoute {
        mode: SearchPlanRouteMode::Vector,
        query: term.clone(),
    }));
    for (index, route) in extras.iter_mut().enumerate() {
        let query = route.query.trim().to_owned();
        if query.is_empty() {
            let mode = match route.mode {
                SearchPlanRouteMode::Fts => "fts",
                SearchPlanRouteMode::Vector => "vector",
            };
            return Err(EngineError::new(
                codes::service_empty_route_query(),
                "zvec-grep context route requires a non-empty query",
            )
            .with_context(format!("routeIndex={index} mode={mode}")));
        }
        route.query = query;
    }
    Ok(extras)
}

/// Per-group result limit, mirroring TS `contextGroupLimit`: an explicit
/// limit wins, three or fewer groups share the default, and larger fan-outs
/// split the total budget.
#[must_use]
fn context_group_limit(limit: Option<usize>, group_count: usize) -> usize {
    if let Some(limit) = limit {
        return limit;
    }
    if group_count <= 3 {
        return DEFAULT_CONTEXT_LIMIT;
    }
    DEFAULT_CONTEXT_TOTAL_LIMIT.div_ceil(group_count).max(1)
}

/// Executes one [`SearchPlan`] per query group and merges the results,
/// mirroring TS `contextFromOpenWorkspaceIndex`: `fuse` collapses every
/// group into a single `Q1` plan, otherwise each group searches with its own
/// per-group limit and the items merge through [`select_and_rank`].
///
/// # Errors
///
/// Returns an error when any per-group search fails.
fn run_context_search(
    index: &WorkspaceIndex,
    root: &str,
    info: &WorkspaceIndexInfo,
    request: &NormalizedContextRequest,
    options: &ZvecGrepContextOptions<'_>,
) -> EngineResult<ZvecGrepContextResult> {
    let groups: Vec<ContextGroup> = if options.fuse {
        vec![ContextGroup {
            id: "Q1".to_owned(),
            query: request.display_query.clone(),
            role: if request
                .groups
                .iter()
                .any(|group| group.role == GroupRole::Primary)
            {
                GroupRole::Primary
            } else {
                GroupRole::Supplemental
            },
            routes: request
                .groups
                .iter()
                .flat_map(|group| group.routes.iter().cloned())
                .collect(),
        }]
    } else {
        request.groups.clone()
    };
    let limit = context_group_limit(options.limit, groups.len());
    let mut searches = Vec::with_capacity(groups.len());
    for group in &groups {
        searches.push(index.search_plan(&group_search_plan(group, options, limit))?);
    }
    let group_items: Vec<Vec<ContextItem>> = searches
        .iter()
        .zip(groups.iter())
        .map(|(search, group)| {
            search
                .hits
                .iter()
                .map(|hit| hit_to_item(hit, group, options.trace))
                .collect()
        })
        .collect();
    let coverage_ids: Vec<String> = groups
        .iter()
        .filter(|group| group.role == GroupRole::Primary)
        .map(|group| group.id.clone())
        .collect();
    let items = select_and_rank(group_items.concat(), &coverage_ids);
    let group_results: Vec<GroupResult> = groups
        .into_iter()
        .zip(group_items)
        .map(|(group, items)| GroupResult {
            id: group.id,
            query: group.query,
            role: Some(group.role),
            items,
            timings: None,
        })
        .collect();
    let routes: Vec<serde_json::Value> = searches
        .iter()
        .flat_map(|search| search.plan.routes.iter())
        .filter_map(|route| serde_json::to_value(route).ok())
        .collect();
    let mut timings: Vec<TimingEntry> = Vec::new();
    for search in &searches {
        timings.extend(search.timings.iter().flatten().cloned());
    }
    let query_groups: Vec<serde_json::Value> = group_results
        .iter()
        .map(|group| {
            serde_json::json!({
                "id": group.id,
                "query": group.query,
                "role": group.role,
            })
        })
        .collect();
    let item_count = items.len();
    Ok(ZvecGrepContextResult {
        query: request.display_query.clone(),
        root: root.to_owned(),
        source: ContextSource::Index,
        coverage: ContextCoverage::RankedSample,
        workspace_index: Some(info.clone()),
        items,
        group_results: Some(group_results),
        diagnostics: ContextDiagnostics {
            empty_reason: if item_count == 0 {
                Some("no_matches".to_owned())
            } else {
                None
            },
            index: Some(serde_json::json!({
                "hitsReturned": item_count,
                "queryGroups": query_groups,
                "routes": routes,
            })),
            rg: None,
            structure: None,
            timings: if timings.is_empty() {
                None
            } else {
                serde_json::to_value(&timings).ok()
            },
        },
    })
}

/// Builds the per-group [`SearchPlan`]: the group's routes with the shared
/// per-group limit and filters. `track_entity_id` is deliberately not
/// forwarded — the TS context `searchPlan` call omits it (see
/// `docs/ts-divergence.md`).
fn group_search_plan(
    group: &ContextGroup,
    options: &ZvecGrepContextOptions<'_>,
    limit: usize,
) -> SearchPlan {
    SearchPlan {
        routes: group.routes.clone(),
        limit: Some(limit),
        trace: Some(options.trace),
        track_entity_id: None,
        prefer_symbol: Some(options.prefer_symbol),
        symbol_types: options.symbol_types.clone(),
        include_paths: options.include_paths.clone(),
        exclude_paths: options.exclude_paths.clone(),
        globs: options.globs.clone(),
        insensitive_globs: options.insensitive_globs.clone(),
        file_types: options.file_types.clone(),
        excluded_file_types: options.excluded_file_types.clone(),
        modified_after: options.modified_after,
        modified_before: options.modified_before,
    }
}

/// Converts one hit into an item tagged with its query group, mirroring TS
/// `searchPlanToContextItems`.
fn hit_to_item(hit: &SearchHit, group: &ContextGroup, trace: bool) -> ContextItem {
    ContextItem {
        kind: ContextItemKind::IndexedEntity,
        rank: hit.rank,
        file: ContextFile {
            absolute_path: hit.file.absolute_path.clone(),
            relative_path: hit.file.relative_path.clone(),
            root_path: hit.file.root_path.clone(),
        },
        range: hit
            .evidence
            .first()
            .map(|evidence| evidence.range.clone())
            .unwrap_or_else(|| hit.entity.range.clone()),
        excerpt_range: None,
        content: hit.entity.content.clone(),
        content_role: None,
        outline: None,
        status: ContentStatus::Fresh,
        score: Some(hit.score),
        matched_by: Some(hit.matched_by.as_str().to_owned()),
        metadata: hit.entity.metadata.clone(),
        entity_id: Some(hit.entity.id.clone()),
        trace: trace
            .then(|| hit.trace.clone())
            .flatten()
            .and_then(|trace| serde_json::to_value(trace).ok()),
        query_groups: vec![QueryGroupRef {
            id: group.id.clone(),
            query: group.query.clone(),
            role: group.role,
            rank: hit.rank,
            matched_by: hit.matched_by,
        }],
        container: None,
        selection_reason: None,
        coverage_group: None,
    }
}

/// Dedupe key for an item, mirroring TS `contextItemDedupeKey`: entity id
/// when present, else the absolute path plus the serialized range.
#[must_use]
fn dedupe_key(item: &ContextItem) -> String {
    match &item.entity_id {
        Some(id) => format!("entity:{}", id.as_str()),
        None => match serde_json::to_string(&item.range) {
            Ok(range) => format!("range:{}:{range}", item.file.absolute_path),
            Err(_) => format!("range:{}:", item.file.absolute_path),
        },
    }
}

/// Numeric group order for `Q<n>` ids; unparseable ids sort last, mirroring
/// `Math.min(...[])` yielding `+Infinity` for empty match lists.
#[must_use]
fn group_number(id: &str) -> usize {
    id.strip_prefix('Q')
        .and_then(|number| number.parse().ok())
        .unwrap_or(usize::MAX)
}

/// Merged `matched_by` across one item's group matches, mirroring TS
/// `mergedContextMatchedBy`.
#[must_use]
fn merged_matched_by(matches: &[QueryGroupRef]) -> SearchMatchedBy {
    let has_fts = matches.iter().any(|group_match| {
        matches!(
            group_match.matched_by,
            SearchMatchedBy::Fts | SearchMatchedBy::FtsVector
        )
    });
    let has_vector = matches.iter().any(|group_match| {
        matches!(
            group_match.matched_by,
            SearchMatchedBy::Vector | SearchMatchedBy::FtsVector
        )
    });
    if has_fts && has_vector {
        SearchMatchedBy::FtsVector
    } else if has_fts {
        SearchMatchedBy::Fts
    } else {
        SearchMatchedBy::Vector
    }
}

/// RRF score over an item's group matches: `Σ 1/(60+rank)`.
#[must_use]
fn rrf_score(matches: &[QueryGroupRef]) -> f64 {
    matches
        .iter()
        .map(|group_match| 1.0 / (CONTEXT_GROUP_RRF_K + group_match.rank as f64))
        .sum()
}

/// Best (lowest) group rank across an item's matches.
#[must_use]
fn best_group_rank(matches: &[QueryGroupRef]) -> usize {
    matches
        .iter()
        .map(|group_match| group_match.rank)
        .min()
        .unwrap_or(usize::MAX)
}

/// Lowest group number across an item's matches.
#[must_use]
fn first_group_number(matches: &[QueryGroupRef]) -> usize {
    matches
        .iter()
        .map(|group_match| group_number(&group_match.id))
        .min()
        .unwrap_or(usize::MAX)
}
/// Global item order, mirroring TS `compareContextGlobalRank`: RRF score
/// descending, then best group rank, group number, and dedupe key ascending.
/// The key comparison is byte order, which diverges from TS `localeCompare`
/// for non-ASCII paths (see `docs/ts-divergence.md`).
#[must_use]
fn compare_global(left: &ContextItem, right: &ContextItem) -> std::cmp::Ordering {
    rrf_score(&right.query_groups)
        .partial_cmp(&rrf_score(&left.query_groups))
        .unwrap_or(std::cmp::Ordering::Equal)
        .then_with(|| {
            best_group_rank(&left.query_groups).cmp(&best_group_rank(&right.query_groups))
        })
        .then_with(|| {
            first_group_number(&left.query_groups).cmp(&first_group_number(&right.query_groups))
        })
        .then_with(|| dedupe_key(left).cmp(&dedupe_key(right)))
}

/// Group rank of `item` inside `group_id`; absent matches sort last.
#[must_use]
fn rank_in_group(item: &ContextItem, group_id: &str) -> usize {
    item.query_groups
        .iter()
        .find(|group_match| group_match.id == group_id)
        .map_or(usize::MAX, |group_match| group_match.rank)
}

/// Merges per-group items into the final ranked list, mirroring TS
/// `selectAndRankContextItems`: dedupe by entity/range (merging group
/// matches with min rank per group), primary-group coverage pass, global
/// fill to the priority cap, then the unprioritized tail — renumbered 1..n.
/// Surviving `score` is whichever group's instance came first while
/// `matched_by` is merged, exactly like TS.
#[must_use]
fn select_and_rank(
    items: Vec<ContextItem>,
    coverage_group_ids: &[String],
) -> Vec<ContextItem> {
    let mut merged: Vec<ContextItem> = Vec::with_capacity(items.len());
    let mut position_by_key: HashMap<String, usize> = HashMap::new();
    for item in items {
        let key = dedupe_key(&item);
        if let Some(&position) = position_by_key.get(&key) {
            if let Some(existing) = merged.get_mut(position) {
                for group_match in item.query_groups {
                    if let Some(known) = existing
                        .query_groups
                        .iter_mut()
                        .find(|known| known.id == group_match.id)
                    {
                        if group_match.rank < known.rank {
                            known.rank = group_match.rank;
                        }
                    } else {
                        existing.query_groups.push(group_match);
                    }
                }
                existing
                    .query_groups
                    .sort_by_key(|group_match| group_number(&group_match.id));
                existing.matched_by = Some(
                    merged_matched_by(&existing.query_groups)
                        .as_str()
                        .to_owned(),
                );
            }
            continue;
        }
        position_by_key.insert(key, merged.len());
        merged.push(item);
    }
    merged.sort_by(compare_global);
    let mut prioritized: Vec<ContextItem> = Vec::new();
    for coverage_id in coverage_group_ids {
        if prioritized.len() >= DEFAULT_CONTEXT_PRIORITY_LIMIT {
            break;
        }
        let best = merged
            .iter()
            .enumerate()
            .filter(|(_, item)| {
                item.query_groups
                    .iter()
                    .any(|group_match| group_match.id == *coverage_id)
            })
            .min_by(|(_, left), (_, right)| {
                rank_in_group(left, coverage_id)
                    .cmp(&rank_in_group(right, coverage_id))
                    .then_with(|| compare_global(left, right))
            })
            .map(|(position, _)| position);
        if let Some(position) = best {
            let mut item = merged.remove(position);
            item.selection_reason = Some(SelectionReason::Coverage);
            item.coverage_group = Some(coverage_id.clone());
            prioritized.push(item);
        }
    }
    while prioritized.len() < DEFAULT_CONTEXT_PRIORITY_LIMIT && !merged.is_empty() {
        let mut item = merged.remove(0);
        item.selection_reason = Some(SelectionReason::GlobalFill);
        prioritized.push(item);
    }
    prioritized.extend(merged);
    for (index, item) in prioritized.iter_mut().enumerate() {
        item.rank = index + 1;
    }
    prioritized
}

/// Polls `signal` on a helper thread until `done` trips the shared
/// [`CancelFlag`]; the blocking index run only ever sees the flag (M6).
fn spawn_signal_watch(
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

fn finish_signal_watch(watch: Option<(std::thread::JoinHandle<()>, Arc<AtomicBool>)>) {
    if let Some((handle, done)) = watch {
        done.store(true, Ordering::Relaxed);
        let _ = handle.join();
    }
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::error::EngineErrorCode;
    use crate::ids::EntityId;
    use crate::types::{Content, Range};

    fn options_with(
        query: Option<&str>,
        queries: &[&str],
        routes: Vec<SearchPlanRoute>,
        fts: &[&str],
    ) -> ZvecGrepContextOptions<'static> {
        ZvecGrepContextOptions {
            query: query.map(str::to_owned),
            queries: queries.iter().map(|query| (*query).to_owned()).collect(),
            routes,
            fts: fts.iter().map(|term| (*term).to_owned()).collect(),
            ..ZvecGrepContextOptions::default()
        }
    }

    fn fts_route(query: &str) -> SearchPlanRoute {
        SearchPlanRoute {
            mode: SearchPlanRouteMode::Fts,
            query: query.to_owned(),
        }
    }

    #[test]
    fn bare_query_expands_to_hybrid_primary_group() {
        let request =
            normalize_context_request(&options_with(Some("alpha"), &[], Vec::new(), &[]))
                .expect("valid request");
        assert_eq!(request.display_query, "alpha");
        assert_eq!(request.groups.len(), 1);
        let group = &request.groups[0];
        assert_eq!(group.id, "Q1");
        assert_eq!(group.role, GroupRole::Primary);
        assert_eq!(group.routes.len(), 2);
        assert_eq!(group.routes[0].mode, SearchPlanRouteMode::Fts);
        assert_eq!(group.routes[1].mode, SearchPlanRouteMode::Vector);
        assert!(group.routes.iter().all(|route| route.query == "alpha"));
    }

    #[test]
    fn queries_share_display_and_extras_continue_numbering() {
        let request = normalize_context_request(&options_with(
            None,
            &["a", "b"],
            vec![fts_route("c")],
            &[],
        ))
        .expect("valid request");
        assert_eq!(request.display_query, "a | b");
        assert_eq!(request.groups.len(), 3);
        assert_eq!(request.groups[0].id, "Q1");
        assert_eq!(request.groups[1].id, "Q2");
        assert!(request.groups[..2]
            .iter()
            .all(|group| group.role == GroupRole::Primary
                && group.routes.len() == 2));
        let extra = &request.groups[2];
        assert_eq!(extra.id, "Q3");
        assert_eq!(extra.role, GroupRole::Supplemental);
        assert_eq!(extra.routes.len(), 1);
        assert_eq!(extra.routes[0].query, "c");
    }

    #[test]
    fn routes_only_request_is_valid_with_display_fallback() {
        let request =
            normalize_context_request(&options_with(None, &[], Vec::new(), &["sym"]))
                .expect("routes-only is valid");
        assert_eq!(request.display_query, "sym");
        assert_eq!(request.groups.len(), 1);
        assert_eq!(request.groups[0].role, GroupRole::Supplemental);
    }

    #[test]
    fn empty_request_errors() {
        let error = normalize_context_request(&options_with(None, &[], Vec::new(), &[]))
            .expect_err("empty request errors");
        assert_eq!(*error.code(), EngineErrorCode::ContextEmptyQuery);
    }

    #[test]
    fn blank_queries_are_trimmed_and_dropped() {
        let request = normalize_context_request(&options_with(
            Some("   "),
            &["", " b "],
            Vec::new(),
            &[],
        ))
        .expect("valid request");
        assert_eq!(request.display_query, "b");
        assert_eq!(request.groups.len(), 1);
        assert_eq!(request.groups[0].routes[0].query, "b");
    }

    #[test]
    fn blank_route_errors_before_consuming_a_group_slot() {
        let error = normalize_context_request(&options_with(
            None,
            &[],
            vec![fts_route("   ")],
            &[],
        ))
        .expect_err("blank route errors");
        assert_eq!(*error.code(), codes::service_empty_route_query());
    }

    #[test]
    fn route_queries_are_trimmed() {
        let request = normalize_context_request(&options_with(
            None,
            &[],
            vec![fts_route("  padded  ")],
            &[],
        ))
        .expect("valid request");
        assert_eq!(request.groups[0].query, "padded");
        assert_eq!(request.groups[0].routes[0].query, "padded");
    }

    #[test]
    fn group_limit_divides_total_budget() {
        assert_eq!(context_group_limit(Some(50), 4), 50);
        assert_eq!(context_group_limit(None, 1), DEFAULT_CONTEXT_LIMIT);
        assert_eq!(context_group_limit(None, 3), DEFAULT_CONTEXT_LIMIT);
        assert_eq!(context_group_limit(None, 4), 8);
        assert_eq!(context_group_limit(None, 10), 3);
        assert_eq!(context_group_limit(None, 0), DEFAULT_CONTEXT_LIMIT);
        assert_eq!(context_group_limit(None, 100), 1);
    }

    fn merge_item(
        entity_suffix: u8,
        group_id: &str,
        group_query: &str,
        rank: usize,
        matched_by: SearchMatchedBy,
    ) -> ContextItem {
        ContextItem {
            kind: ContextItemKind::IndexedEntity,
            rank,
            file: ContextFile {
                absolute_path: "/repo/a.rs".to_owned(),
                relative_path: "a.rs".to_owned(),
                root_path: "/repo".to_owned(),
            },
            range: Range::Text {
                start_line: 1,
                end_line: 2,
                start_offset: 0,
                end_offset: 10,
            },
            excerpt_range: None,
            content: Content::Text {
                text: "fn f() {}".to_owned(),
            },
            content_role: None,
            outline: None,
            status: ContentStatus::Fresh,
            score: Some(1.0 / rank as f64),
            matched_by: Some(matched_by.as_str().to_owned()),
            metadata: None,
            entity_id: Some(
                EntityId::parse(&format!("{entity_suffix:064}")).expect("valid entity id"),
            ),
            trace: None,
            query_groups: vec![QueryGroupRef {
                id: group_id.to_owned(),
                query: group_query.to_owned(),
                role: GroupRole::Primary,
                rank,
                matched_by,
            }],
            container: None,
            selection_reason: None,
            coverage_group: None,
        }
    }

    #[test]
    fn merge_dedupes_coverage_and_fill() {
        use SearchMatchedBy::{Fts, Vector};
        let items = vec![
            merge_item(1, "Q1", "a", 2, Fts),
            merge_item(1, "Q2", "b", 1, Vector),
            merge_item(2, "Q1", "a", 1, Fts),
            merge_item(3, "Q2", "b", 2, Fts),
            merge_item(4, "Q1", "a", 3, Fts),
            merge_item(5, "Q1", "a", 4, Fts),
            merge_item(6, "Q1", "a", 5, Fts),
            merge_item(7, "Q1", "a", 6, Fts),
        ];
        let ranked = select_and_rank(items, &["Q1".to_owned(), "Q2".to_owned()]);
        assert_eq!(ranked.len(), 7);
        for (index, item) in ranked.iter().enumerate() {
            assert_eq!(item.rank, index + 1);
        }
        assert!(ranked[0].entity_id.as_ref().is_some_and(|id| id
            .as_str()
            .ends_with('2')));
        assert_eq!(ranked[0].selection_reason, Some(SelectionReason::Coverage));
        assert_eq!(ranked[0].coverage_group.as_deref(), Some("Q1"));
        let merged = &ranked[1];
        assert_eq!(merged.query_groups.len(), 2);
        assert_eq!(merged.matched_by.as_deref(), Some("fts+vector"));
        assert_eq!(merged.selection_reason, Some(SelectionReason::Coverage));
        assert_eq!(merged.coverage_group.as_deref(), Some("Q2"));
        for item in &ranked[2..6] {
            assert_eq!(item.selection_reason, Some(SelectionReason::GlobalFill));
        }
        assert_eq!(ranked[6].selection_reason, None);
    }
    #[test]
    fn groupless_items_sort_last() {
        use SearchMatchedBy::Fts;
        let mut grouped = merge_item(1, "Q9", "a", 50, Fts);
        grouped.entity_id = None;
        let mut groupless = merge_item(2, "Q1", "a", 1, Fts);
        groupless.entity_id = None;
        groupless.query_groups.clear();
        groupless.range = Range::Text {
            start_line: 9,
            end_line: 10,
            start_offset: 0,
            end_offset: 10,
        };
        let ranked = select_and_rank(vec![groupless, grouped], &[]);
        assert_eq!(ranked[0].query_groups.len(), 1);
        assert!(ranked[1].query_groups.is_empty());
    }
}
