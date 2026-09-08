//! Actor search path: freshness wait, cached-session search, overlays.
//!
//! Searches read the last committed session instead of the TS writer
//! context (documented staleness trade-off). [`RootActor::handle_search`]
//! wraps the inner search with operation tracking; `wait_for_fresh`
//! settles the live job (bounded: three rounds) before
//! `execute_search` runs the hybrid query through the session cache, and
//! `finish_search` attaches the daemon-computed freshness.

use std::sync::Arc;

use zg_core::authorization::operation::with_remote_embedding_operation_permit;
use zg_core::index_status::{
    IndexJobState, index_completion_for_job, index_completion_from_status,
};
use zg_core::service::types::{ZvecGrepContextOptions, ZvecGrepContextResult};
use zg_core::types::SearchPlanRoute;

use super::error::BackendError;
use super::index_run::{build_index_run, plan_search_permit};
use super::root_actor::RootActor;
use super::search_types::{
    BackgroundIndexState, DaemonSearchResult, ResultFreshness, SearchFreshness, SearchIndexing,
    SearchQuery,
};
use crate::change_set::ChangeSetSnapshot;
use crate::errors::DaemonError;
use crate::index_coordinator::CoordinatorReason;
use crate::job_scheduler::{IndexJobSnapshot, JobReason};

impl RootActor {
    pub(crate) async fn handle_search(
        &mut self,
        query: SearchQuery,
    ) -> Result<DaemonSearchResult, BackendError> {
        self.runtime.begin_operation();
        let result = self.search_inner(query).await;
        self.runtime.end_operation();
        result
    }

    async fn search_inner(
        &mut self,
        query: SearchQuery,
    ) -> Result<DaemonSearchResult, BackendError> {
        if query.freshness == SearchFreshness::WaitForFresh {
            self.wait_for_fresh().await?;
        }
        let result = self.execute_search(&query).await?;
        if query.auto_update {
            // Best effort: a failed background submit never fails a search.
            let _ = self.background_job();
        }
        Ok(self.finish_search(result))
    }

    async fn execute_search(
        &mut self,
        query: &SearchQuery,
    ) -> Result<ZvecGrepContextResult, BackendError> {
        let info = self.temp_service().workspace_info(None)?;
        let options = ZvecGrepContextOptions {
            query: query.query.clone(),
            queries: query.queries.clone(),
            routes: query
                .routes
                .iter()
                .map(|route| SearchPlanRoute {
                    mode: route.mode.plan_mode(),
                    query: route.query.clone(),
                })
                .collect(),
            fuse: query.fuse,
            limit: query.limit,
            trace: query.trace,
            prefer_symbol: query.prefer_symbol,
            symbol_types: query.symbol_types.clone(),
            globs: query.globs.clone(),
            insensitive_globs: query.insensitive_globs.clone(),
            file_types: query.file_types.clone(),
            excluded_file_types: query.excluded_file_types.clone(),
            modified_after: query.modified_after,
            modified_before: query.modified_before,
            // The daemon owns refresh through the scheduler; the facade
            // must not reindex inline inside a read session (its
            // read-session guard forces this off as well).
            auto_update: false,
            ..ZvecGrepContextOptions::default()
        };
        let auth = Arc::clone(&self.shared.auth);
        let needs_reconcile = self.runtime.needs_reconciliation();
        self.sessions
            .with_read(|cached| {
                let permit =
                    plan_search_permit(&auth, &info, cached.model_info(), needs_reconcile)?;
                Ok(with_remote_embedding_operation_permit(permit, || {
                    cached.session().context(&options)
                })?)
            })
            .await?
    }

    /// Waits for the active index to become fresh: settles the live job,
    /// triggers a reconcile when stale work is waiting, and re-checks.
    /// Bounded so watcher churn during the wait cannot loop forever.
    async fn wait_for_fresh(&mut self) -> Result<(), BackendError> {
        for _ in 0..3 {
            if !self.runtime.needs_reconciliation() {
                return Ok(());
            }
            let Some(job) = self.background_job() else {
                return Ok(());
            };
            if job.is_terminal() {
                return Self::check_terminal_job(&job, self.runtime.needs_reconciliation());
            }
            let snapshot = self.shared.scheduler.wait(&job.id, None).await?;
            Self::check_terminal_job(&snapshot, self.runtime.needs_reconciliation())?;
        }
        Ok(())
    }

    fn check_terminal_job(job: &IndexJobSnapshot, still_dirty: bool) -> Result<(), BackendError> {
        if !still_dirty {
            return Ok(());
        }
        match job.state {
            IndexJobState::Failed | IndexJobState::Cancelled => {
                let message = job
                    .error
                    .as_ref()
                    .map(|error| error.message.clone())
                    .unwrap_or_else(|| "index job did not complete".to_owned());
                Err(DaemonError::IndexFailed { message }.into())
            }
            IndexJobState::Queued | IndexJobState::Running | IndexJobState::Succeeded => Ok(()),
        }
    }

    /// Submits a background reconcile when stale work is waiting and no
    /// job is active, mirroring TS `background_reconcile`. Returns the
    /// live job to wait on, if any. Never enqueues when nothing is
    /// pending: the take would be empty and the dirty-revision bump
    /// would buy nothing.
    fn background_job(&mut self) -> Option<IndexJobSnapshot> {
        if self.runtime.needs_reconciliation()
            && !self.shared.scheduler.has_active_root(self.key.as_str())
        {
            let full = self.runtime.requires_full_reconciliation();
            if full || self.coordinator.has_pending() {
                let changes = ChangeSetSnapshot {
                    force_full_reconcile: full,
                    ..ChangeSetSnapshot::default()
                };
                let tx = self.tx.clone();
                let key = self.key.clone();
                let shared = self.shared.clone();
                if let Ok(submitted) = self.coordinator.enqueue(
                    &changes,
                    CoordinatorReason::BackgroundReconcile,
                    &mut self.runtime,
                    &self.shared.scheduler,
                    |take| build_index_run(tx, key, shared, take),
                ) {
                    self.runtime.set_writer_pending(true);
                    return Some(submitted.job);
                }
            }
        }
        self.shared.scheduler.get_by_root(self.key.as_str())
    }

    /// Attaches daemon-computed freshness to a search result, mirroring
    /// the TS `fresh` / `possibly_stale` derivation.
    fn finish_search(&mut self, result: ZvecGrepContextResult) -> DaemonSearchResult {
        let snapshot = self.runtime.snapshot();
        let job = self.shared.scheduler.get_by_root(self.key.as_str());
        let active_known_change = matches!(&job,
            Some(job) if job.reason != JobReason::BackgroundReconcile && !job.is_terminal());
        let freshness = if self.runtime.needs_reconciliation()
            || snapshot.watcher_pending
            || active_known_change
        {
            ResultFreshness::PossiblyStale
        } else {
            ResultFreshness::Fresh
        };
        let indexing = if freshness == ResultFreshness::PossiblyStale {
            Some(self.search_indexing(job.as_ref()))
        } else {
            None
        };
        DaemonSearchResult {
            result,
            freshness,
            indexing,
        }
    }

    /// Compact background-indexing snapshot for possibly-stale results.
    fn search_indexing(&self, job: Option<&IndexJobSnapshot>) -> SearchIndexing {
        let overlaid = index_completion_for_job(
            index_completion_from_status(self.status.as_ref()),
            job.map(|job| job.state),
            job.and_then(|job| job.progress.as_ref()),
        );
        SearchIndexing {
            state: BackgroundIndexState::of(job),
            completed: overlaid.as_ref().map(|completion| completion.completed),
            total: overlaid.as_ref().map(|completion| completion.total),
        }
    }
}
