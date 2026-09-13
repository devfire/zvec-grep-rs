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
    ContextSource, EmbeddingInfo, GroupResult, GroupRole, RootPathSpec, ZvecGrepContextOptions,
    ZvecGrepContextResult, ZvecGrepIndexOptions, ZvecGrepInfoResult, empty_query_error,
    workspace_index_disabled, workspace_index_not_found,
};
use crate::service::workspace_index::{
    IndexMode, IndexOptions, WorkspaceIndex, WorkspaceIndexOptions, is_workspace_indexed,
};
use crate::types::{
    CURRENT_INDEX_VERSION, RootPath, SearchHit, SearchPlan, SearchPlanResult, SearchPlanRoute,
    SearchPlanRouteMode, UnixMillis, WorkspaceIndexEmbeddingSchema, WorkspaceIndexInfo,
    WorkspaceIndexStatus,
};

/// Fallback embedding model when nothing selects one, mirroring
/// `DEFAULT_LOCAL_EMBEDDING` in the TS service.
pub const DEFAULT_EMBEDDING_REFERENCE: &str = "local/potion-code-16m-v2";

/// Default result limit, mirroring `DEFAULT_CONTEXT_LIMIT`.
pub const DEFAULT_CONTEXT_LIMIT: usize = 10;

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
        let (query, plan) = build_search_plan(options)?;
        let location = self.require_indexed_location(options.root)?;
        let manifest = self.require_manifest(&location)?;
        if options.wants_auto_update() {
            self.refresh(&location)?;
            return self.context_after_refresh(options, query, plan);
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
        let result = index.search_plan(&plan)?;
        Ok(assemble_context(
            &location.root,
            &query,
            &info,
            result,
            options.trace,
        ))
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
        query: String,
        plan: SearchPlan,
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
        let result = index.search_plan(&plan)?;
        Ok(assemble_context(
            &location.root,
            &query,
            &info,
            result,
            options.trace,
        ))
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
        let (query, plan) = build_search_plan(options)?;
        let result = index.search_plan(&plan)?;
        Ok(assemble_context(
            &self.root,
            &query,
            &self.info,
            result,
            options.trace,
        ))
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

/// Builds the primary query string plus [`SearchPlan`] from context options.
///
/// Explicit `routes` win; the `query`/`queries` shorthands become vector
/// routes and `fts`/`vector` expand to their modes, mirroring
/// `normalizeContextRequest`.
fn build_search_plan(options: &ZvecGrepContextOptions<'_>) -> EngineResult<(String, SearchPlan)> {
    let query = options
        .query
        .clone()
        .or_else(|| options.queries.first().cloned())
        .filter(|query| !query.trim().is_empty())
        .ok_or_else(empty_query_error)?;
    let mut routes = Vec::new();
    if let Some(query) = &options.query {
        routes.push(SearchPlanRoute {
            mode: SearchPlanRouteMode::Vector,
            query: query.clone(),
        });
    }
    for query in &options.queries {
        routes.push(SearchPlanRoute {
            mode: SearchPlanRouteMode::Vector,
            query: query.clone(),
        });
    }
    for term in &options.fts {
        routes.push(SearchPlanRoute {
            mode: SearchPlanRouteMode::Fts,
            query: term.clone(),
        });
    }
    for query in &options.vector {
        routes.push(SearchPlanRoute {
            mode: SearchPlanRouteMode::Vector,
            query: query.clone(),
        });
    }
    routes.extend(options.routes.clone());
    Ok((
        query,
        SearchPlan {
            routes,
            limit: Some(options.limit.unwrap_or(DEFAULT_CONTEXT_LIMIT)),
            trace: Some(options.trace),
            track_entity_id: options.track_entity_id.clone(),
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
        },
    ))
}

/// Assembles the wire [`ZvecGrepContextResult`] from an executed plan.
fn assemble_context(
    root: &str,
    query: &str,
    info: &WorkspaceIndexInfo,
    result: SearchPlanResult,
    trace: bool,
) -> ZvecGrepContextResult {
    let SearchPlanResult {
        plan,
        hits,
        timings,
        ..
    } = result;
    let items = hits.iter().map(|hit| hit_to_item(hit, trace)).collect();
    let group_results = plan
        .routes
        .iter()
        .enumerate()
        .map(|(position, route)| {
            let hits: Vec<SearchHit> = hits
                .iter()
                .filter(|hit| {
                    hit.evidence
                        .iter()
                        .any(|evidence| evidence.route_id.as_deref() == Some(route.id.as_str()))
                })
                .cloned()
                .collect();
            GroupResult {
                id: route.id.clone(),
                query: route.query.clone(),
                role: Some(if position == 0 {
                    GroupRole::Primary
                } else {
                    GroupRole::Supplemental
                }),
                hits,
                timings: None,
            }
        })
        .collect();
    ZvecGrepContextResult {
        query: query.to_owned(),
        root: root.to_owned(),
        source: ContextSource::Index,
        coverage: ContextCoverage::RankedSample,
        workspace_index: Some(info.clone()),
        items,
        group_results: Some(group_results),
        diagnostics: ContextDiagnostics {
            index: timings.and_then(|timings| serde_json::to_value(timings).ok()),
            ..ContextDiagnostics::default()
        },
    }
}

fn hit_to_item(hit: &SearchHit, trace: bool) -> ContextItem {
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
        query_groups: Vec::new(),
        container: None,
        selection_reason: None,
    }
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
