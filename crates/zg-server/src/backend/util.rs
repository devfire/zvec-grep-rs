//! Small backend helpers with no home elsewhere.
//!
//! [`log_event`] emits a two-field daemon event; [`join_backend_error`]
//! maps a `spawn_blocking` join failure without inventing new wire codes.
//! Wall-clock stamps come from the shared [`UnixMillis`](zg_core::types::UnixMillis)
//! authority (see [`DaemonServerStatus`](super::DaemonServerStatus)).

use std::collections::BTreeMap;

use super::error::BackendError;
use crate::errors::DaemonError;
use crate::logger::{DaemonLogger, LogField};

pub(crate) fn log_event(logger: &Option<DaemonLogger>, name: &str, fields: [(&str, LogField); 2]) {
    if let Some(logger) = logger {
        logger.event(
            name,
            fields
                .into_iter()
                .map(|(key, value)| (key.to_owned(), value))
                .collect::<BTreeMap<_, _>>(),
        );
    }
}

/// Maps a `spawn_blocking` join failure: cancellation races the daemon
/// shutdown path, anything else is a failed index-side task. No new wire
/// code is invented — both variants already exist in the registries.
pub(crate) fn join_backend_error(error: tokio::task::JoinError) -> BackendError {
    if error.is_cancelled() {
        BackendError::Daemon(DaemonError::ShuttingDown)
    } else {
        BackendError::Daemon(DaemonError::IndexFailed {
            message: format!("blocking task panicked: {error}"),
        })
    }
}
