//! Checkout failures: closed pool vs. load failure with the engine error
//! preserved, plus the dehydrated failure shared with waiting acquirers.

use zg_core::error::{EngineError, EngineErrorCode};

use crate::errors::DaemonError;

/// Failure to check out a model: either the pool is closed or the load
/// itself failed (with the engine error preserved, not stringified).
#[derive(Debug)]
pub enum AcquireError {
    /// The pool is closed; the daemon is shutting down.
    Closed,
    /// Model construction failed.
    Load {
        /// Catalog reference that failed to load.
        reference: String,
        /// Engine failure, preserved for diagnostics and retry decisions.
        error: EngineError,
    },
}

impl AcquireError {
    /// Daemon wire code for this failure.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Closed => DaemonError::ShuttingDown.code(),
            Self::Load { .. } => DaemonError::ModelLoadFailed {
                reference: String::new(),
            }
            .code(),
        }
    }

    /// Inner engine error for load failures.
    #[must_use]
    pub fn engine_error(&self) -> Option<&EngineError> {
        match self {
            Self::Load { error, .. } => Some(error),
            Self::Closed => None,
        }
    }
}

impl std::fmt::Display for AcquireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Closed => write!(f, "embedding model pool is closed"),
            Self::Load { reference, error } => write!(f, "failed to load {reference}: {error}"),
        }
    }
}

impl std::error::Error for AcquireError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Closed => None,
            Self::Load { error, .. } => Some(error),
        }
    }
}

/// Dehydrated load failure shared with waiting acquirers. The code is the
/// `Copy` [`EngineErrorCode`], so it round-trips exactly; message and
/// context are owned strings.
#[derive(Debug, Clone)]
pub(crate) struct LoadError {
    code: EngineErrorCode,
    message: String,
    context: Option<String>,
}

impl LoadError {
    pub(crate) fn dehydrate(error: &EngineError) -> Self {
        Self {
            code: *error.code(),
            message: error.message().to_owned(),
            context: error.context().map(str::to_owned),
        }
    }

    pub(crate) fn rehydrate(&self) -> EngineError {
        let mut error = EngineError::new(self.code, self.message.clone());
        if let Some(context) = &self.context {
            error = error.with_context(context.clone());
        }
        error
    }
}
