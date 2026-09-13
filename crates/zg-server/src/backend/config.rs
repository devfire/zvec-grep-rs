//! Shared service configuration for every root actor.
//!
//! [`ServiceConfig`] selects the embedding model and credentials;
//! [`BackendShared`] is the cloneable handle bundle threaded through the
//! [`RuntimeManager`](crate::runtime_manager::RuntimeManager) into each
//! actor; [`DaemonBackendOptions`] is the public constructor input for
//! [`DaemonBackend`](super::DaemonBackend).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use zg_core::authorization::operation::RemoteEmbeddingAuthorizationManager;
use zg_core::models::EmbeddingModel;
use zg_core::models::catalog::ModelReference;
use zg_core::models::embeddings::{CreateEmbeddingModelOptions, DeviceKind};
use zg_core::service::facade::CreateZvecGrepOptions;

use crate::job_scheduler::{JobScheduler, JobSchedulerOptions};
use crate::logger::DaemonLogger;
use crate::model_pool::{EmbeddingModelPool, EmbeddingModelPoolOptions};

/// Service-level options shared by every root actor.
#[derive(Clone, Default)]
pub struct ServiceConfig {
    /// Explicit embedding reference; wins over the manifest entry.
    pub embedding: Option<ModelReference>,
    /// API key for remote providers.
    pub api_key: Option<String>,
    /// Endpoint override for remote providers.
    pub endpoint: Option<String>,
    /// Local model cache directory override.
    pub model_cache_dir: Option<PathBuf>,
    /// Injected model handle (tests, pool bypass). Wins over everything.
    pub model_override: Option<Arc<dyn EmbeddingModel>>,
}

impl ServiceConfig {
    /// Facade options bound to `root` with an explicit model handle.
    #[must_use]
    pub fn facade_options(
        &self,
        root: &str,
        model: Option<Arc<dyn EmbeddingModel>>,
    ) -> CreateZvecGrepOptions {
        CreateZvecGrepOptions {
            root: Some(PathBuf::from(root)),
            embedding: self.embedding.clone(),
            embedding_model: model.or_else(|| self.model_override.clone()),
            api_key: self.api_key.clone(),
            endpoint: self.endpoint.clone(),
            model_cache_dir: self.model_cache_dir.clone(),
        }
    }

    /// Model construction options for pool loads.
    #[must_use]
    pub fn load_options(&self) -> CreateEmbeddingModelOptions {
        CreateEmbeddingModelOptions {
            api_key: self.api_key.clone(),
            endpoint: self.endpoint.clone(),
            model_cache_dir: self.model_cache_dir.clone(),
            device: DeviceKind::Auto,
        }
    }
}

/// State shared by the backend, the manager, and every root actor.
#[derive(Clone)]
pub struct BackendShared {
    /// Shared index job scheduler.
    pub scheduler: JobScheduler,
    /// Shared model pool (owns the M6 embed gate).
    pub pool: EmbeddingModelPool,
    /// Service options for facade construction.
    pub service: ServiceConfig,
    /// Remote-embedding grant manager.
    pub auth: Arc<RemoteEmbeddingAuthorizationManager>,
    /// Daemon logger.
    pub logger: Option<DaemonLogger>,
    /// Read-session idle TTL.
    pub read_session_ttl: Duration,
    /// Quiet-actor idle TTL.
    pub runtime_idle_ttl: Duration,
}

impl BackendShared {
    /// Facade options for `root` with no explicit model (catalog path).
    #[must_use]
    pub fn catalog_options(&self, root: &str) -> CreateZvecGrepOptions {
        self.service.facade_options(root, None)
    }
}

/// Options for [`DaemonBackend`](super::DaemonBackend).
#[derive(Clone, Default)]
pub struct DaemonBackendOptions {
    /// Service options (embedding selection, credentials, test override).
    pub service: ServiceConfig,
    /// Scheduler tuning.
    pub scheduler: JobSchedulerOptions,
    /// Pool tuning.
    pub pool: EmbeddingModelPoolOptions,
    /// Grant manager; defaults to a fresh manager.
    pub auth: Option<Arc<RemoteEmbeddingAuthorizationManager>>,
    /// Daemon logger.
    pub logger: Option<DaemonLogger>,
    /// Read-session idle TTL.
    pub read_session_ttl: Option<Duration>,
    /// Quiet-actor idle TTL.
    pub runtime_idle_ttl: Option<Duration>,
}
