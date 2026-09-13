//! Backend failure type and blocking-task error mapping.
//!
//! [`BackendError`] converges typed daemon and engine errors without merging
//! their wire codes (M1) — [`BackendError::code`] returns the raw string
//! from either side.

use zg_core::error::EngineError;

use crate::errors::DaemonError;
use crate::read_session_cache::SessionError;

/// Backend failure: typed daemon and engine errors converge here without
/// merging their wire codes (M1) — [`BackendError::code`] returns the raw
/// string from either side.
#[derive(Debug)]
pub enum BackendError {
    /// Daemon-layer failure (bare wire code).
    Daemon(DaemonError),
    /// Engine-layer failure (`ZVEC_GREP.ENGINE.*` code).
    Engine(EngineError),
}

impl BackendError {
    /// Raw wire code from either side.
    #[must_use]
    pub fn code(&self) -> String {
        match self {
            Self::Daemon(error) => error.code().to_owned(),
            Self::Engine(error) => error.code().to_string(),
        }
    }
}

impl std::fmt::Display for BackendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Daemon(error) => write!(f, "{error}"),
            Self::Engine(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for BackendError {}

impl From<DaemonError> for BackendError {
    fn from(error: DaemonError) -> Self {
        Self::Daemon(error)
    }
}

impl From<EngineError> for BackendError {
    fn from(error: EngineError) -> Self {
        Self::Engine(error)
    }
}

impl From<SessionError> for BackendError {
    fn from(error: SessionError) -> Self {
        match error {
            SessionError::Closed => Self::Daemon(DaemonError::ShuttingDown),
            SessionError::Open(error) => Self::Engine(error),
        }
    }
}
