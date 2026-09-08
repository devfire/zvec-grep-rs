//! Root-actor spawning and session bootstrap.
//!
//! [`spawn_root_actor`] builds one actor task: coordinator, read-session
//! cache bound to [`session_opener`], and a watcher whose flushed batches
//! re-enter as [`RootCommand::WatchBatch`](super::actor::RootCommand).
//! [`load_request_for`] resolves the manifest embedding schema to a pool
//! load request (also reused by the index-run model resolver).

use std::sync::Arc;

use futures::future::BoxFuture;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use zg_core::error::{EngineError, EngineResult};
use zg_core::models::catalog::{EMBEDDING_MODEL_CATALOG, EmbeddingCatalogEntry, ModelReference};
use zg_core::models::error::ModelError;
use zg_core::service::facade::ZvecGrepService;
use zg_core::service::types::workspace_index_not_found;

use super::actor::{CachedSession, RootCommand};
use super::config::{BackendShared, ServiceConfig};
use super::root_actor::RootActor;
use super::util::log_event;
use crate::change_set::ChangeSetSnapshot;
use crate::index_coordinator::IndexCoordinator;
use crate::logger::LogField;
use crate::model_pool::{AcquireError, ModelLoadRequest};
use crate::read_session_cache::{OpenSession, SessionError, WorkspaceReadSessionCache};
use crate::root_runtime::RootKey;
use crate::root_runtime::RootRuntime;
use crate::runtime_manager::RuntimeManager;
use crate::watch_manager::{WatchManager, WatchManagerOptions, WatchReason};

/// Spawns one root actor task. Called by the manager with a fresh channel;
/// `tx` is shared back into the watcher callbacks so flushed batches
/// re-enter as [`RootCommand::WatchBatch`].
pub(crate) async fn spawn_root_actor(
    shared: BackendShared,
    manager: RuntimeManager,
    key: RootKey,
    tx: UnboundedSender<RootCommand>,
    rx: UnboundedReceiver<RootCommand>,
) {
    let coordinator = IndexCoordinator::new(key.as_str(), None);
    let opener = session_opener(shared.clone(), key.clone());
    let sessions = WorkspaceReadSessionCache::new(opener, Some(shared.read_session_ttl));
    let batch_sender = tx.clone();
    let on_changes = Arc::new(move |snapshot: ChangeSetSnapshot, reason: WatchReason| {
        let _ = batch_sender.send(RootCommand::WatchBatch {
            changes: snapshot,
            reason,
        });
    });
    let service = shared.service.clone();
    let root = key.to_string();
    let get_roots = Arc::new(move || {
        let svc = ZvecGrepService::new(service.facade_options(&root, None));
        svc.workspace_info(None)
            .ok()
            .and_then(|info| info.workspace_index)
            .map(|index| index.root_paths)
            .unwrap_or_default()
    });
    let mut watcher = WatchManager::new(WatchManagerOptions {
        root: key.to_string(),
        debounce: None,
        max_wait: None,
        max_changed_paths: None,
        on_changes,
        get_root_paths: Some(get_roots),
        on_pending: None,
    });
    if let Err(error) = watcher.start() {
        log_event(
            &shared.logger,
            "watch.start_failed",
            [
                ("root_id", LogField::from(key.to_string())),
                ("error_code", LogField::from(error.code().to_owned())),
            ],
        );
    }
    let mut actor = RootActor {
        key: key.clone(),
        shared,
        manager,
        runtime: RootRuntime::new(key),
        coordinator,
        sessions,
        watcher,
        status: None,
        scan: None,
        rx,
        tx,
    };
    actor.run().await;
}

/// Session opener: resolves the manifest model (or the override) and
/// checks out a lease the session keeps alive.
fn session_opener(shared: BackendShared, key: RootKey) -> OpenSession<CachedSession> {
    Arc::new(move || {
        let shared = shared.clone();
        let key = key.clone();
        Box::pin(async move {
            if let Some(model) = shared.service.model_override.clone() {
                let service = ZvecGrepService::new(
                    shared
                        .service
                        .facade_options(key.as_str(), Some(model.clone())),
                );
                let session = service
                    .open_read_session(None)
                    .map_err(SessionError::Open)?;
                return Ok(CachedSession::new(session, model.info().clone(), None));
            }
            let request = load_request_for(&shared.service, &key).map_err(SessionError::Open)?;
            let lease = shared
                .pool
                .acquire(&request)
                .await
                .map_err(|error| match error {
                    AcquireError::Closed => SessionError::Closed,
                    AcquireError::Load { error, .. } => SessionError::Open(error),
                })?;
            let model_info = lease.model().info().clone();
            let service = ZvecGrepService::new(
                shared
                    .service
                    .facade_options(key.as_str(), Some(lease.model().clone())),
            );
            let session = service
                .open_read_session(None)
                .map_err(SessionError::Open)?;
            Ok(CachedSession::new(session, model_info, Some(lease)))
        }) as BoxFuture<'static, Result<CachedSession, SessionError>>
    })
}

/// Resolves the manifest embedding schema to a pool load request.
pub(crate) fn load_request_for(
    service: &ServiceConfig,
    key: &RootKey,
) -> EngineResult<ModelLoadRequest> {
    if let Some(reference) = &service.embedding {
        return Ok(ModelLoadRequest {
            reference: reference.clone(),
            options: service.load_options(),
        });
    }
    let facade = ZvecGrepService::new(service.facade_options(key.as_str(), None));
    let info = facade.workspace_info(None)?;
    let schema = info
        .workspace_index
        .as_ref()
        .and_then(|index| index.embedding.clone())
        .flatten()
        .ok_or_else(|| workspace_index_not_found(key.as_str()))?;
    let reference =
        catalog_reference_for_schema(&schema.provider, &schema.model).ok_or_else(|| {
            EngineError::from(ModelError::CatalogModelNotFound {
                reference: format!("{}/{}", schema.provider, schema.model),
            })
        })?;
    Ok(ModelLoadRequest {
        reference,
        options: service.load_options(),
    })
}

/// Finds the catalog reference for a manifest embedding schema.
fn catalog_reference_for_schema(provider: &str, model: &str) -> Option<ModelReference> {
    EMBEDDING_MODEL_CATALOG.iter().find_map(|entry| {
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
        (entry_provider == provider && entry_model == model).then(|| ModelReference::new(reference))
    })
}
