//! Scheduler index runs: lazy takes, M6 permits, blocking bodies.
//!
//! [`build_index_run`] builds the [`JobRun`](crate::job_scheduler::JobRun)
//! for one submitted job: it takes the pending snapshot lazily, holds the
//! M6 embed permit across model load and the blocking index, and reports
//! back through [`RootCommand::IndexFinished`](super::actor::RootCommand).
//! [`plan_search_permit`] serves the actor search path in
//! [`super::search`].

use std::path::PathBuf;
use std::sync::Arc;

use futures::future::BoxFuture;
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;
use zg_core::authorization::operation::{
    RemoteEmbeddingAuthorizationManager, with_remote_embedding_operation_permit,
};
use zg_core::authorization::planner::{
    PlanIndexInput, PlanSearchInput, plan_remote_index_authorization,
    plan_remote_search_authorization,
};
use zg_core::authorization::types::RemoteEmbeddingPermit;
use zg_core::error::{EngineError, EngineResult};
use zg_core::models::{EmbeddingModel, EmbeddingModelInfo};
use zg_core::service::facade::ZvecGrepService;
use zg_core::service::types::{ZvecGrepIndexOptions, ZvecGrepInfoResult};

use super::actor::{FinishedIndex, FinishedOk, IndexOutcome, IndexRunFailure, RootCommand};
use super::config::BackendShared;
use super::runtime::load_request_for;
use crate::change_set::ChangeSetSnapshot;
use crate::index_coordinator::TakePending;
use crate::job_scheduler::{JobFailure, JobOutcome, JobRun};
use crate::model_pool::{AcquireError, ModelLease};
use crate::root_runtime::RootKey;

/// Builds the scheduler run for one submitted job: takes the pending
/// snapshot lazily, holds the M6 embed permit across the blocking index,
/// and reports back through [`RootCommand::IndexFinished`].
pub(crate) fn build_index_run(
    tx: UnboundedSender<RootCommand>,
    key: RootKey,
    shared: BackendShared,
    take: TakePending,
) -> JobRun {
    Arc::new(move |reporter, token| {
        let tx = tx.clone();
        let key = key.clone();
        let shared = shared.clone();
        // Cloned per invocation: retries replay the same take handle, and
        // `Fn` cannot move the captured original into the future.
        let take = take.clone();
        Box::pin(async move {
            let (snapshot, revision) = take();
            let force_full = snapshot.force_full_reconcile;
            if !force_full
                && snapshot.touched_files.is_empty()
                && snapshot.rescan_directories.is_empty()
                && snapshot.deleted_prefixes.is_empty()
            {
                let _ = tx.send(RootCommand::IndexFinished {
                    finished: Box::new(FinishedIndex {
                        revision,
                        force_full: false,
                        outcome: IndexOutcome::Noop,
                    }),
                });
                return Ok(());
            }
            if token.is_cancelled() {
                return Err(JobFailure::Cancelled);
            }
            // M6: one embed permit per index job, held across model load
            // and the blocking run, bounding CPU oversubscription.
            let _embed = shared
                .pool
                .embed_permits()
                .acquire_owned()
                .await
                .map_err(|_| JobFailure::Cancelled)?;
            let resolved = match resolve_model(&shared, &key).await {
                Ok(resolved) => resolved,
                Err(AcquireError::Closed) => return Err(JobFailure::Cancelled),
                Err(AcquireError::Load { error, .. }) => return Err(JobFailure::Engine(error)),
            };
            // `resolved` (and its pool lease) lives to the end of this
            // scope — past the blocking run below; dropping it releases
            // the lease.
            let model = resolved.model.clone();
            let _resolved = resolved;
            let outcome = run_blocking_index(
                &shared, &key, model, &snapshot, force_full, reporter, &token,
            )
            .await;
            let (outcome, job_result) = match outcome {
                Ok(Some(finished)) => (IndexOutcome::Completed(Box::new(finished)), Ok(())),
                Ok(None) => (IndexOutcome::Noop, Ok(())),
                Err(IndexRunError::Engine(error)) => (
                    IndexOutcome::Failed(IndexRunFailure::Engine(error.clone())),
                    Err(JobFailure::Engine(error)),
                ),
                Err(IndexRunError::Join(message)) => (
                    IndexOutcome::Failed(IndexRunFailure::Join(message.clone())),
                    Err(JobFailure::Failed(message)),
                ),
            };
            let _ = tx.send(RootCommand::IndexFinished {
                finished: Box::new(FinishedIndex {
                    revision,
                    force_full,
                    outcome,
                }),
            });
            if token.is_cancelled() {
                return Err(JobFailure::Cancelled);
            }
            job_result
        }) as BoxFuture<'static, JobOutcome>
    })
}

enum IndexRunError {
    Engine(EngineError),
    Join(String),
}

/// Blocking index body: permit-scoped, cancellation-aware, returning the
/// fresh status alongside the counters.
async fn run_blocking_index(
    shared: &BackendShared,
    key: &RootKey,
    model: Arc<dyn EmbeddingModel>,
    snapshot: &ChangeSetSnapshot,
    force_full: bool,
    reporter: zg_core::pipeline::indexing::IndexProgressSink,
    token: &CancellationToken,
) -> Result<Option<FinishedOk>, IndexRunError> {
    let facade = ZvecGrepService::new(
        shared
            .service
            .facade_options(key.as_str(), Some(model.clone())),
    );
    // The manifest read and permit planning below are blocking/synchronous
    // work: they run inside the blocking body, never on the calling async
    // worker (#44). Only owned values cross into the closure.
    let auth = Arc::clone(&shared.auth);
    let model_info = model.info().clone();
    let needs_update = !snapshot.touched_files.is_empty()
        || !snapshot.rescan_directories.is_empty()
        || !snapshot.deleted_prefixes.is_empty();
    // Cooperative cancel crosses as an owned abort probe (M4): the facade
    // polls it on a helper thread and trips the blocking body's CancelFlag.
    let abort = Arc::new({
        let token = token.clone();
        move || token.is_cancelled()
    });
    let changed: Vec<PathBuf> = snapshot
        .touched_files
        .iter()
        .chain(snapshot.rescan_directories.iter())
        .chain(snapshot.deleted_prefixes.iter())
        .map(PathBuf::from)
        .collect();
    let root = PathBuf::from(key.as_str());
    let joined = tokio::task::spawn_blocking(move || {
        let info = facade.workspace_info(None)?;
        let permit = plan_index_permit(&auth, &info, &model_info, force_full, needs_update)?;
        with_remote_embedding_operation_permit(permit, || {
            let index_result = facade.ensure_index(&ZvecGrepIndexOptions {
                root: Some(root.as_path()),
                rebuild: force_full,
                changed_paths: changed,
                on_progress: Some(reporter),
                signal: Some(abort),
                ..ZvecGrepIndexOptions::default()
            })?;
            let status = facade.index_status(None)?;
            Ok(FinishedOk {
                index_result,
                status,
            })
        })
    })
    .await;
    match joined {
        Err(error) => Err(IndexRunError::Join(format!("index task panicked: {error}"))),
        Ok(Err(error)) => Err(IndexRunError::Engine(error)),
        Ok(Ok(finished)) => Ok(Some(finished)),
    }
}

/// Model for one run plus the pool lease that keeps it resident (held
/// until the run completes; overrides carry no lease).
struct ResolvedModel {
    /// Model handle for the facade.
    model: Arc<dyn EmbeddingModel>,
    /// Pool lease, alive for the whole run.
    _lease: Option<ModelLease>,
}

/// Resolves the model for one run: override first, pool otherwise.
async fn resolve_model(
    shared: &BackendShared,
    key: &RootKey,
) -> Result<ResolvedModel, AcquireError> {
    if let Some(model) = shared.service.model_override.clone() {
        return Ok(ResolvedModel {
            model,
            _lease: None,
        });
    }
    let request = load_request_for(&shared.service, key)
        .await
        .map_err(|error| AcquireError::Load {
            reference: key.to_string(),
            error,
        })?;
    let lease = shared.pool.acquire(&request).await?;
    Ok(ResolvedModel {
        model: lease.model().clone(),
        _lease: Some(lease),
    })
}

/// Index-side permit: plans remote authorization and returns the existing
/// workspace permit, if any. `None` runs unscoped — local models ignore
/// the scope and remote backends fail closed without a grant.
fn plan_index_permit(
    auth: &RemoteEmbeddingAuthorizationManager,
    info: &ZvecGrepInfoResult,
    model: &EmbeddingModelInfo,
    rebuild: bool,
    needs_update: bool,
) -> EngineResult<Option<RemoteEmbeddingPermit>> {
    let plan = plan_remote_index_authorization(&PlanIndexInput {
        info,
        model,
        rebuild,
        needs_update,
    })?;
    match plan {
        None => Ok(None),
        Some(plan) => auth.existing_workspace_permit(&plan.target),
    }
}

/// Search-side permit (sessions never auto-update).
pub(crate) fn plan_search_permit(
    auth: &RemoteEmbeddingAuthorizationManager,
    info: &ZvecGrepInfoResult,
    model: &EmbeddingModelInfo,
    needs_reconciliation: bool,
) -> EngineResult<Option<RemoteEmbeddingPermit>> {
    let plan = plan_remote_search_authorization(&PlanSearchInput {
        info,
        model,
        uses_vector: true,
        auto_update: false,
        freshness_wait: false,
        runtime_needs_reconciliation: needs_reconciliation,
    })?;
    match plan {
        None => Ok(None),
        Some(plan) => auth.existing_workspace_permit(&plan.target),
    }
}
