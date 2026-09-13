//! Daemon error taxonomy: `DaemonError` enum plus golden code registry.
//!
//! Mirrors `../zvec-grep/src/daemon/errors.ts` (`DaemonError` with a plain
//! string code and a `retryable` flag). Per M1 the Rust shape is an enum —
//! every code is a `&'static str` literal returned by [`DaemonError::code`],
//! exhaustively matched, with no runtime-assembled variant. Codes stay bare
//! (`INDEX_MISSING`, not `ZVEC_GREP.ENGINE.*`-prefixed): the TS wire carries
//! them raw inside job snapshots and MCP payloads, so prefixing would break
//! the frozen contract.
//!
//! Divergence notes (see `docs/ts-divergence.md`): `ADDRESS_IN_USE`,
//! `UNKNOWN_JOB`, `ALREADY_RUNNING`, and `SHUTDOWN_FAILED` have no TS code
//! — TS throws plain `Error`s on those paths. They are Rust additions so
//! every daemon failure stays inside the enum.

use std::fmt;

/// Typed daemon failure. Every variant maps to one frozen wire code via
/// [`DaemonError::code`]; see `tests/golden/daemon-error-codes.txt`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DaemonError {
    /// The daemon is shutting down. Retryable — the TS constructor passes
    /// `retryable = true` on this path only.
    ShuttingDown,
    /// Indexed search needs a built index that is absent.
    IndexMissing {
        /// Canonical root the caller asked about.
        root: String,
    },
    /// A second writer arrived while an index or drop job owns the root.
    IndexBusy {
        /// Canonical root under contention.
        root: String,
    },
    /// An index job failed for a non-retryable reason.
    IndexFailed {
        /// Redacted one-line summary.
        message: String,
    },
    /// An index job was cancelled cooperatively.
    IndexCancelled,
    /// A drop arrived while another drop owns the root.
    IndexDropInProgress {
        /// Canonical root under contention.
        root: String,
    },
    /// The model pool could not load the requested embedding model.
    ModelLoadFailed {
        /// Catalog reference that failed to load.
        reference: String,
    },
    /// The loaded model does not match the manifest's recorded embedding.
    EmbeddingModelMismatch {
        /// Human-readable expected-vs-actual detail.
        detail: String,
    },
    /// A root argument was not an absolute path.
    RootNotAbsolute {
        /// The offending root argument.
        root: String,
    },
    /// A root argument names nothing usable on disk.
    RootNotFound {
        /// The offending root argument.
        root: String,
    },
    /// A root argument is not readable (or not writable for indexing).
    RootPermissionDenied {
        /// The offending root argument.
        root: String,
    },
    /// A `--listen` value is not `host:port` or its port is out of range.
    InvalidListenAddress {
        /// The offending listen value.
        value: String,
    },
    /// A listen host or request Host/Origin is not loopback.
    LoopbackRequired {
        /// The offending host value.
        host: String,
    },
    /// A server token is missing, too short, or does not match.
    InvalidToken,
    /// Remote embedding was requested without a grant.
    RemoteEmbeddingAuthRequired,
    /// The listen address is already bound. Rust addition: TS throws a
    /// plain `Error` here.
    AddressInUse {
        /// The contested `host:port`.
        address: String,
    },
    /// A scheduler `wait` named an unknown job. Rust addition: TS throws a
    /// plain `Error` here.
    UnknownJob {
        /// The unknown job id.
        id: String,
    },
    /// `server on` found a live instance lock. Rust addition: TS throws a
    /// plain `Error` here.
    AlreadyRunning {
        /// PID recorded in the live lock.
        pid: u32,
    },
    /// `POST /control/shutdown` answered non-2xx. Rust addition: TS throws
    /// a plain `ShutdownResponseError` here.
    ShutdownFailed {
        /// HTTP status of the refused shutdown.
        status: u16,
    },
}

impl DaemonError {
    /// Frozen wire code for this variant. `const` so call sites and the
    /// golden registry stay literal-only.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::ShuttingDown => "DAEMON_SHUTTING_DOWN",
            Self::IndexMissing { .. } => "INDEX_MISSING",
            Self::IndexBusy { .. } => "INDEX_BUSY",
            Self::IndexFailed { .. } => "INDEX_FAILED",
            Self::IndexCancelled => "INDEX_CANCELLED",
            Self::IndexDropInProgress { .. } => "INDEX_DROP_IN_PROGRESS",
            Self::ModelLoadFailed { .. } => "MODEL_LOAD_FAILED",
            Self::EmbeddingModelMismatch { .. } => "EMBEDDING_MODEL_MISMATCH",
            Self::RootNotAbsolute { .. } => "ROOT_NOT_ABSOLUTE",
            Self::RootNotFound { .. } => "ROOT_NOT_FOUND",
            Self::RootPermissionDenied { .. } => "ROOT_PERMISSION_DENIED",
            Self::InvalidListenAddress { .. } => "INVALID_LISTEN_ADDRESS",
            Self::LoopbackRequired { .. } => "LOOPBACK_REQUIRED",
            Self::InvalidToken => "INVALID_TOKEN",
            Self::RemoteEmbeddingAuthRequired => "REMOTE_EMBEDDING_AUTH_REQUIRED",
            Self::AddressInUse { .. } => "ADDRESS_IN_USE",
            Self::UnknownJob { .. } => "UNKNOWN_JOB",
            Self::AlreadyRunning { .. } => "ALREADY_RUNNING",
            Self::ShutdownFailed { .. } => "SHUTDOWN_FAILED",
        }
    }

    /// True only for [`DaemonError::ShuttingDown`], mirroring the TS
    /// `retryable` constructor flag. The scheduler retries only these plus
    /// `ZVEC_GREP.ENGINE.LOCK.BUSY` engine errors.
    #[must_use]
    pub const fn retryable(&self) -> bool {
        matches!(self, Self::ShuttingDown)
    }
}

impl fmt::Display for DaemonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ShuttingDown => write!(f, "the daemon is shutting down"),
            Self::IndexMissing { root } => write!(
                f,
                "indexed search requires a built zvec-grep index for {root}"
            ),
            Self::IndexBusy { root } => {
                write!(f, "an index operation is already running for {root}")
            }
            Self::IndexFailed { message } => write!(f, "indexing failed: {message}"),
            Self::IndexCancelled => write!(f, "indexing was cancelled"),
            Self::IndexDropInProgress { root } => {
                write!(f, "an index drop is already in progress for {root}")
            }
            Self::ModelLoadFailed { reference } => {
                write!(f, "failed to load embedding model {reference}")
            }
            Self::EmbeddingModelMismatch { detail } => {
                write!(f, "embedding model mismatch: {detail}")
            }
            Self::RootNotAbsolute { root } => {
                write!(f, "root must be an absolute path: {root}")
            }
            Self::RootNotFound { root } => write!(f, "root does not exist: {root}"),
            Self::RootPermissionDenied { root } => {
                write!(f, "root is not accessible: {root}")
            }
            Self::InvalidListenAddress { value } => {
                write!(f, "listen must use host:port format: {value}")
            }
            Self::LoopbackRequired { host } => {
                write!(f, "server only supports loopback listen addresses: {host}")
            }
            Self::InvalidToken => write!(f, "invalid or missing server token"),
            Self::RemoteEmbeddingAuthRequired => {
                write!(f, "remote embedding requires an authorization grant")
            }
            Self::AddressInUse { address } => {
                write!(f, "server address {address} is already in use")
            }
            Self::UnknownJob { id } => write!(f, "unknown job: {id}"),
            Self::AlreadyRunning { pid } => {
                write!(f, "zvec-grep server is already running with PID {pid}")
            }
            Self::ShutdownFailed { status } => {
                write!(f, "server shutdown request failed with HTTP {status}")
            }
        }
    }
}

impl std::error::Error for DaemonError {}

/// Every daemon wire code, one per enum variant, for the golden registry
/// test (`tests/golden/daemon-error-codes.txt`). Adding a variant without
/// extending this list fails that test by construction.
#[must_use]
pub fn all_codes() -> Vec<&'static str> {
    vec![
        DaemonError::ShuttingDown.code(),
        DaemonError::IndexMissing {
            root: String::new(),
        }
        .code(),
        DaemonError::IndexBusy {
            root: String::new(),
        }
        .code(),
        DaemonError::IndexFailed {
            message: String::new(),
        }
        .code(),
        DaemonError::IndexCancelled.code(),
        DaemonError::IndexDropInProgress {
            root: String::new(),
        }
        .code(),
        DaemonError::ModelLoadFailed {
            reference: String::new(),
        }
        .code(),
        DaemonError::EmbeddingModelMismatch {
            detail: String::new(),
        }
        .code(),
        DaemonError::RootNotAbsolute {
            root: String::new(),
        }
        .code(),
        DaemonError::RootNotFound {
            root: String::new(),
        }
        .code(),
        DaemonError::RootPermissionDenied {
            root: String::new(),
        }
        .code(),
        DaemonError::InvalidListenAddress {
            value: String::new(),
        }
        .code(),
        DaemonError::LoopbackRequired {
            host: String::new(),
        }
        .code(),
        DaemonError::InvalidToken.code(),
        DaemonError::RemoteEmbeddingAuthRequired.code(),
        DaemonError::AddressInUse {
            address: String::new(),
        }
        .code(),
        DaemonError::UnknownJob { id: String::new() }.code(),
        DaemonError::AlreadyRunning { pid: 0 }.code(),
        DaemonError::ShutdownFailed { status: 0 }.code(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shutdown_is_the_only_retryable_code() {
        assert!(DaemonError::ShuttingDown.retryable());
        assert!(!DaemonError::IndexCancelled.retryable());
        assert!(
            !DaemonError::IndexBusy {
                root: String::new()
            }
            .retryable()
        );
    }
}
