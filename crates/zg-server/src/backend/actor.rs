//! Root-actor protocol: handles, commands, and the cached read session.
//!
//! [`RootHandle`] is the only cross-boundary address of a root actor (its
//! sender stays crate-visible: external layers command actors through
//! [`DaemonBackend`](super::DaemonBackend), never directly).
//! [`RootCommand`] enumerates everything one actor processes sequentially;
//! [`FinishedIndex`] reports a scheduler run back into the actor.
//! [`CachedSession`] is the read-session cache entry: facade guard plus the
//! model identity and pool lease that keep it valid.

use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::oneshot;
use zg_core::lexical::LexicalSearchResult;
use zg_core::models::EmbeddingModelInfo;
use zg_core::service::facade::ReadSession;
use zg_core::types::{IndexResult, WorkspaceIndexStatus};

use crate::root_runtime::Generation;

use super::error::BackendError;
use super::request_types::{DaemonIndexStatus, IndexInput, RgQuery};
use super::search_types::{DaemonSearchResult, SearchQuery};
use crate::change_set::ChangeSetSnapshot;
use crate::errors::DaemonError;
use crate::job_scheduler::SubmitIndexJobResult;
use crate::model_pool::ModelLease;
use crate::read_session_cache::ClosableHandle;
use crate::root_runtime::RootKey;
use crate::runtime_manager::send_command;
use crate::watch_manager::WatchReason;

/// Handle to a live root actor: its key plus its command sender. The
/// sender stays crate-visible: external layers command actors through
/// [`DaemonBackend`](super::DaemonBackend), never directly.
#[derive(Clone)]
pub struct RootHandle {
    /// Canonical root the actor owns.
    pub key: RootKey,
    /// Actor command sender.
    pub(crate) tx: UnboundedSender<RootCommand>,
}

/// Commands one root actor processes sequentially.
pub(crate) enum RootCommand {
    /// Hybrid search through the read-session cache.
    Search {
        /// Owned query.
        query: SearchQuery,
        /// Search result with daemon-computed freshness.
        reply: oneshot::Sender<Result<DaemonSearchResult, BackendError>>,
    },
    /// Index (or reindex) through the scheduler.
    Index {
        /// Owned index input.
        input: IndexInput,
        /// Submitted job snapshot plus whether a live job was reused.
        reply: oneshot::Sender<Result<SubmitIndexJobResult, BackendError>>,
    },
    /// Cached-or-read status with live job overlay.
    Status {
        /// Status plus overlay.
        reply: oneshot::Sender<Result<DaemonIndexStatus, BackendError>>,
    },
    /// In-process lexical search.
    Rg {
        /// Owned lexical query.
        query: RgQuery,
        /// Lexical result.
        reply: oneshot::Sender<Result<LexicalSearchResult, BackendError>>,
    },
    /// Cancels jobs, closes handles, deletes storage.
    Drop {
        /// True when storage existed.
        reply: oneshot::Sender<Result<bool, BackendError>>,
    },
    /// Watcher batch from the watch manager.
    WatchBatch {
        /// Flushed changes.
        changes: ChangeSetSnapshot,
        /// Flush reason.
        reason: WatchReason,
    },
    /// A scheduler index run finished (sent by the run closure).
    IndexFinished {
        /// Outcome with its target revision, boxed: the status payload
        /// dwarfs every other variant (M3).
        finished: Box<FinishedIndex>,
    },
    /// Stop the actor and unregister.
    Shutdown,
}

/// Outcome of one index run, stamped with its target revision.
#[derive(Debug)]
pub(crate) struct FinishedIndex {
    /// Revision the run reconciled toward.
    pub(crate) revision: Generation,
    /// Whether the run reconciled fully.
    pub(crate) force_full: bool,
    /// Fresh status on success; `None` keeps the previous cache.
    pub(crate) ok: Option<FinishedOk>,
}

/// Successful run payload.
#[derive(Debug)]
pub(crate) struct FinishedOk {
    /// Raw index counters (refreshes scan diagnostics).
    pub(crate) index_result: IndexResult,
    /// Freshly read status.
    pub(crate) status: WorkspaceIndexStatus,
}

/// Cached read session: the facade guard, the model identity behind it
/// (for permit planning), and the pool lease that keeps the model
/// resident. The session closes before the lease releases (declaration
/// order).
pub(crate) struct CachedSession {
    /// Open read guard.
    session: ReadSession,
    /// Static identity of the backing model.
    model_info: EmbeddingModelInfo,
    /// Model lease backing the session (unused beyond ownership).
    _lease: Option<ModelLease>,
}

impl CachedSession {
    /// Wraps an open guard with its model identity and optional lease.
    pub(crate) fn new(
        session: ReadSession,
        model_info: EmbeddingModelInfo,
        lease: Option<ModelLease>,
    ) -> Self {
        Self {
            session,
            model_info,
            _lease: lease,
        }
    }

    /// Static identity of the backing model.
    pub(crate) fn model_info(&self) -> &EmbeddingModelInfo {
        &self.model_info
    }

    /// Open read guard.
    pub(crate) fn session(&self) -> &ReadSession {
        &self.session
    }
}

#[async_trait::async_trait]
impl ClosableHandle for CachedSession {
    async fn close(self) {
        self.session.close();
    }
}

/// Sends one command and awaits its reply. A dead actor (dropped reply
/// half) means the root is gone: surfaces as a shutdown error.
pub(crate) async fn send_recv<T>(
    handle: &RootHandle,
    make: impl FnOnce(oneshot::Sender<T>) -> RootCommand,
) -> Result<T, BackendError> {
    let (tx, rx) = oneshot::channel();
    send_command(&handle.tx, make(tx))?;
    rx.await
        .map_err(|_| BackendError::Daemon(DaemonError::ShuttingDown))
}
