//! [`ZvecGrepService`]: construction, root handling, and the lexical
//! passthrough. Indexing, search, info, sessions, and model resolution are
//! `impl` blocks in the sibling modules sharing this struct.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::error::EngineResult;
use crate::lexical::{LexicalSearchOptions, LexicalSearchResult, run_lexical_search};
use crate::manifest::{WorkspaceManifest, read_workspace_manifest};
use crate::models::EmbeddingModel;
use crate::models::catalog::ModelReference;
use crate::paths::to_display_path;
use crate::service::root::{
    WorkspaceIndexLocation, find_nearest_workspace_index, resolve_zvec_grep_root,
    workspace_index_location,
};
use crate::service::types::workspace_index_not_found;

/// Options for [`create_zvec_grep`], mirroring `CreateZvecGrepOptions`.
///
/// All fields are optional; `Default::default()` binds the working directory
/// with model resolution from manifest/env/defaults. The constructor is
/// infallible: nothing is rejected here. Unknown roots fall back to the
/// working directory (or `"."`), and model failures surface later at
/// `ensure_index`/`context` time, not at construction.
///
/// Model precedence: `embedding_model` (injected handle, never touches the
/// network) wins over `embedding` (explicit catalog reference), which wins
/// over manifest/env/defaults. `api_key`/`endpoint` apply only to remote
/// providers; `model_cache_dir` overrides only the local model cache.
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
    /// API key for remote embedding providers (`None` = env/default).
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
///
/// Fields are `pub(super)` so the sibling concern modules (`index`,
/// `models`, `info`, `context`, `session`) can implement methods on this
/// type; external callers go through the constructors and methods.
pub struct ZvecGrepService {
    pub(super) root: String,
    pub(super) embedding: Option<ModelReference>,
    pub(super) embedding_model: Option<Arc<dyn EmbeddingModel>>,
    pub(super) api_key: Option<String>,
    pub(super) endpoint: Option<String>,
    pub(super) model_cache_dir: Option<PathBuf>,
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

    /// Exhaustive in-process lexical search — never a subprocess, mirroring
    /// `rg` handling via `lexical/mod.rs`.
    ///
    /// # Errors
    ///
    /// Returns an error when search paths are invalid or the in-process walk fails.
    pub fn rg_search(&self, options: &LexicalSearchOptions) -> EngineResult<LexicalSearchResult> {
        run_lexical_search(options)
    }

    pub(super) fn root_string(&self, root: Option<&Path>) -> String {
        root.map_or_else(|| self.root.clone(), to_display_path)
    }

    pub(super) fn require_indexed_location(
        &self,
        root: Option<&Path>,
    ) -> EngineResult<WorkspaceIndexLocation> {
        let start = self.root_string(root);
        find_nearest_workspace_index(&start).ok_or_else(|| workspace_index_not_found(&start))
    }

    pub(super) fn require_manifest(
        &self,
        location: &WorkspaceIndexLocation,
    ) -> EngineResult<WorkspaceManifest> {
        read_workspace_manifest(Path::new(&location.home))?
            .ok_or_else(|| workspace_index_not_found(&location.root))
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
