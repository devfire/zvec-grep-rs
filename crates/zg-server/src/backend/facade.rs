//! Daemon command surface over per-root actors.
//!
//! [`DaemonBackend`] owns the [`RuntimeManager`](crate::runtime_manager::RuntimeManager)
//! plus the shared scheduler and pool handles. It spawns no actors itself:
//! the first command for a root activates one (see [`super::runtime`]),
//! and every command after that is a [`send_recv`](super::actor) round-trip.

use std::path::PathBuf;

use zg_core::lexical::LexicalSearchResult;
use zg_core::service::facade::ZvecGrepService;
use zg_core::types::UnixMillis;

use super::actor::{RootCommand, send_recv};
use super::config::{BackendShared, DaemonBackendOptions, ServiceConfig};
use super::error::BackendError;
use super::request_types::{DaemonIndexStatus, DaemonServerStatus, IndexInput, RgQuery};
use super::search_types::{DaemonSearchResult, SearchQuery};
use super::util::join_backend_error;
use crate::job_scheduler::{JobScheduler, SubmitIndexJobResult};
use crate::model_pool::EmbeddingModelPool;
use crate::root_runtime::resolve_requested_root;
use crate::runtime_manager::RuntimeManager;

/// Daemon command surface over per-root actors.
#[derive(Clone)]
pub struct DaemonBackend {
    manager: RuntimeManager,
    scheduler: JobScheduler,
    pool: EmbeddingModelPool,
    started_at_ms: u64,
}

impl DaemonBackend {
    /// Builds the backend: shared scheduler, pool, and actor registry.
    /// No actors spawn until the first command.
    #[must_use]
    pub fn new(options: DaemonBackendOptions) -> Self {
        let scheduler = JobScheduler::new(options.scheduler);
        let pool = EmbeddingModelPool::new(options.pool);
        let shared = BackendShared {
            scheduler: scheduler.clone(),
            pool: pool.clone(),
            service: options.service,
            auth: options.auth.unwrap_or_default(),
            logger: options.logger,
            read_session_ttl: options
                .read_session_ttl
                .unwrap_or(crate::read_session_cache::DEFAULT_READ_SESSION_IDLE_TTL),
            runtime_idle_ttl: options
                .runtime_idle_ttl
                .unwrap_or(crate::runtime_manager::DEFAULT_RUNTIME_IDLE_TTL),
        };
        let manager = RuntimeManager::new(shared);
        Self {
            manager,
            scheduler,
            pool,
            // Clock-unavailable direction: display-only (pairs with
            // `uptime_ms`); `0` skews the display, inert otherwise.
            started_at_ms: UnixMillis::now_ms_or(0),
        }
    }

    /// Shared scheduler (waiting on jobs, load reporting).
    #[must_use]
    pub fn scheduler(&self) -> &JobScheduler {
        &self.scheduler
    }

    /// Shared model pool.
    #[must_use]
    pub fn pool(&self) -> &EmbeddingModelPool {
        &self.pool
    }

    /// Hybrid search over an indexed root.
    ///
    /// # Errors
    ///
    /// Returns [`BackendError::Daemon`] when the root cannot be activated or the actor call
    /// fails, or [`BackendError::Engine`] when the search itself fails.
    pub async fn search(
        &self,
        root: &str,
        query: SearchQuery,
    ) -> Result<DaemonSearchResult, BackendError> {
        let handle = self.manager.activate_for_search(root).await?;
        send_recv(&handle, |reply| RootCommand::Search { query, reply }).await?
    }

    /// Submits an index job for a root.
    ///
    /// # Errors
    ///
    /// Returns [`BackendError::Daemon`] when the root cannot be activated or the actor call
    /// fails, or [`BackendError::Engine`] when job submission fails.
    pub async fn index(
        &self,
        root: &str,
        input: IndexInput,
    ) -> Result<SubmitIndexJobResult, BackendError> {
        let handle = self.manager.activate_for_index(root)?;
        send_recv(&handle, |reply| RootCommand::Index { input, reply }).await?
    }

    /// Deletes a root's index storage and stops its actor.
    ///
    /// # Errors
    ///
    /// Returns [`BackendError::Daemon`] when the root is invalid or the actor call fails,
    /// or [`BackendError::Engine`] when dropping the index storage fails.
    pub async fn index_drop(&self, root: &str) -> Result<bool, BackendError> {
        let key = resolve_requested_root(root, true)?;
        if let Some(handle) = self.manager.get(&key) {
            let dropped: bool = send_recv(&handle, |reply| RootCommand::Drop { reply }).await??;
            let _ = self.manager.unregister(&key);
            return Ok(dropped);
        }
        // Never-activated roots drop straight through the facade: no model
        // is needed to delete a manifest and storage.
        let service =
            ZvecGrepService::new(ServiceConfig::default().facade_options(key.as_str(), None));
        let root_buf = PathBuf::from(key.as_str());
        tokio::task::spawn_blocking(move || service.drop_index(Some(root_buf.as_path())))
            .await
            .map_err(join_backend_error)?
            .map_err(BackendError::Engine)
    }

    /// In-process lexical search over a root.
    ///
    /// # Errors
    ///
    /// Returns [`BackendError::Daemon`] when the root cannot be activated or the actor call
    /// fails, or [`BackendError::Engine`] when the lexical search fails.
    pub async fn rg_search(
        &self,
        root: &str,
        query: RgQuery,
    ) -> Result<LexicalSearchResult, BackendError> {
        let handle = self.manager.activate_for_index(root)?;
        send_recv(&handle, |reply| RootCommand::Rg { query, reply }).await?
    }

    /// Index status with live job overlay.
    ///
    /// # Errors
    ///
    /// Returns [`BackendError::Daemon`] when the root cannot be activated or the actor call
    /// fails, or [`BackendError::Engine`] when status collection fails.
    pub async fn index_status(&self, root: &str) -> Result<DaemonIndexStatus, BackendError> {
        let handle = self.manager.activate_for_index(root)?;
        send_recv(&handle, |reply| RootCommand::Status { reply }).await?
    }

    /// Daemon liveness snapshot.
    #[must_use]
    pub fn server_status(&self) -> DaemonServerStatus {
        let load = self.scheduler.load();
        let pool = self.pool.snapshot();
        DaemonServerStatus {
            started_at_ms: self.started_at_ms,
            runtimes: self.manager.actor_count(),
            queued_jobs: load.queued,
            running_jobs: load.running,
            pool_loaded: pool.loaded,
            pool_leases: pool.active_leases,
        }
    }

    /// Stops every actor, then awaits in-flight work (M6) and unloads models.
    pub async fn close(&self) {
        self.manager.close().await;
    }
}
