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
use super::index_run::build_index_run;
use super::root_actor::RootActor;
use super::util::log_event;
use crate::change_set::ChangeSetSnapshot;
use crate::index_coordinator::{CoordinatorReason, IndexCoordinator};
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
    generation: u64,
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
    // Blocking manifest reads off the async worker (#44): one snapshot up
    // front warms the watcher roots cache and reports whether a persisted
    // index exists for catch-up seeding below (#34).
    let seed_service = shared.service.clone();
    let seed_root = key.to_string();
    let (initial_roots, had_index) = tokio::task::spawn_blocking(move || {
        let svc = ZvecGrepService::new(seed_service.facade_options(&seed_root, None));
        svc.workspace_info(None)
            .map(|info| {
                let roots = info
                    .workspace_index
                    .as_ref()
                    .map(|index| index.root_paths.clone())
                    .unwrap_or_default();
                (roots, info.indexed)
            })
            .unwrap_or_default()
    })
    .await
    .unwrap_or_default();
    let mut watcher = WatchManager::new(WatchManagerOptions {
        root: key.to_string(),
        debounce: None,
        max_wait: None,
        max_changed_paths: None,
        on_changes,
        get_root_paths: Some(get_roots),
        initial_roots: Some(initial_roots),
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
    let mut runtime = RootRuntime::new(key.clone());
    // Fresh actors start zeroed, so `needs_reconciliation()` is false even
    // when files changed while no actor was live. Seed one catch-up
    // reconciliation when a persisted index exists so the gap cannot read
    // as fresh (#34); roots without an index need nothing (the first
    // explicit index call already forces a full build).
    if let Some(seed) = catch_up_snapshot(had_index) {
        let seed_tx = tx.clone();
        let seed_key = key.clone();
        let seed_shared = shared.clone();
        if coordinator
            .enqueue(
                &seed,
                CoordinatorReason::Reconcile,
                &mut runtime,
                &shared.scheduler,
                |take| build_index_run(seed_tx, seed_key, seed_shared, take),
            )
            .is_ok()
        {
            runtime.set_writer_pending(true);
        }
    }
    let mut actor = RootActor {
        key,
        generation,
        shared,
        manager,
        runtime,
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
            let request = load_request_for(&shared.service, &key)
                .await
                .map_err(SessionError::Open)?;
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

/// Resolves the manifest embedding schema to a pool load request. The
/// manifest read is blocking storage IO: it runs on a blocking thread,
/// never on the calling async worker (#44).
pub(crate) async fn load_request_for(
    service: &ServiceConfig,
    key: &RootKey,
) -> EngineResult<ModelLoadRequest> {
    if let Some(reference) = &service.embedding {
        return Ok(ModelLoadRequest {
            reference: reference.clone(),
            options: service.load_options(),
        });
    }
    let options = service.facade_options(key.as_str(), None);
    let info =
        tokio::task::spawn_blocking(move || ZvecGrepService::new(options).workspace_info(None))
            .await
            .map_err(|error| {
                EngineError::new(
                    zg_core::error::codes::daemon_blocking_join_failed(),
                    "embedding schema read task failed",
                )
                .with_context(format!("detail={error}"))
            })??;
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

/// Catch-up changes for a fresh actor from persisted state: `None` when no
/// index was ever built (the first explicit index call already forces a
/// full build); otherwise one full reconciliation so on-disk changes from
/// while no actor was live cannot read as fresh (#34).
fn catch_up_snapshot(indexed: bool) -> Option<ChangeSetSnapshot> {
    indexed.then(|| ChangeSetSnapshot {
        force_full_reconcile: true,
        ..ChangeSetSnapshot::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catch_up_seeds_full_reconcile_only_when_prior_index_exists() {
        assert!(catch_up_snapshot(false).is_none());
        let seed = catch_up_snapshot(true).expect("seed when indexed");
        assert!(seed.force_full_reconcile);
        assert!(seed.touched_files.is_empty());
        assert!(seed.rescan_directories.is_empty());
        assert!(seed.deleted_prefixes.is_empty());
    }
}
