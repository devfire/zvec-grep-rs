//! Typed CLI errors (M1): one enum, const wire codes, golden registry.
//!
//! Mirrors no single TS module: the TypeScript CLI throws plain `Error`s
//! with hand-written messages. The codes here are Rust additions under
//! `ZVEC_GREP.ENGINE.CLI.*`; the *messages* stay byte-identical to TS
//! where the plan freezes them (drop confirmation, auth directives,
//! server-search incompatibility, removed flags). Engine and daemon
//! failures converge via `#[from]` and keep their own codes.

use zg_core::authorization::AuthError;
use zg_core::error::{EngineError, EngineErrorCode};
use zg_core::models::error::ModelError;
use zg_server::backend::BackendError;
use zg_server::errors::DaemonError;

/// CLI failure: usage, environment, or a wrapped engine/daemon error.
#[derive(Debug, thiserror::Error)]
pub enum CliError {
    /// Invalid invocation or rejected flag combination. The message is the
    /// user-facing text (many mirror `cli/args.ts` verbatim).
    #[error("{message}")]
    Usage {
        /// Exact text shown on stderr.
        message: String,
    },
    /// Invalid `config` values or references.
    #[error("{message}")]
    ConfigInvalid {
        /// Exact text shown on stderr.
        message: String,
    },
    /// Interactive authorization was declined; nothing remote was sent.
    #[error("{message}")]
    AuthorizationDeclined {
        /// Exact text shown on stderr.
        message: String,
    },
    /// Remote embedding needs a grant and no TTY/`--allow-remote` can
    /// provide one. Mirrors the `authorizeCliPlan` non-TTY text.
    #[error("{message}")]
    AuthorizationRequired {
        /// Exact text shown on stderr.
        message: String,
    },
    /// Install would clobber unmanaged config without `--yes`/`--force`.
    #[error("{message}")]
    InstallRefused {
        /// Exact text shown on stderr.
        message: String,
    },
    /// A running server predates grouped CLI output (`server-search.ts`).
    #[error("{message}")]
    ServerIncompatible {
        /// Exact text shown on stderr.
        message: String,
    },
    /// An rg flag has no in-process equivalent; directs to `zvec_grep_rg`.
    #[error("{message}")]
    RgIncompatible {
        /// Exact text shown on stderr.
        message: String,
    },
    /// The daemon is unreachable or not running.
    #[error("{message}")]
    DaemonUnavailable {
        /// Exact text shown on stderr.
        message: String,
    },
    /// `--check-ready` found a non-ready state.
    #[error("{message}")]
    NotReady {
        /// Exact text shown on stderr.
        message: String,
    },
    /// Filesystem failure with the path attached.
    #[error("{message}")]
    Io {
        /// Exact text shown on stderr.
        message: String,
    },
    /// Engine failure; keeps its own `ZVEC_GREP.ENGINE.*` code.
    #[error(transparent)]
    Engine(#[from] EngineError),
    /// Daemon failure; keeps its own bare code.
    #[error(transparent)]
    Daemon(#[from] DaemonError),
}

impl CliError {
    /// Builds [`CliError::Usage`].
    pub fn usage(message: impl Into<String>) -> Self {
        Self::Usage {
            message: message.into(),
        }
    }

    /// Builds [`CliError::ConfigInvalid`].
    pub fn config_invalid(message: impl Into<String>) -> Self {
        Self::ConfigInvalid {
            message: message.into(),
        }
    }

    /// Builds [`CliError::InstallRefused`].
    pub fn install_refused(message: impl Into<String>) -> Self {
        Self::InstallRefused {
            message: message.into(),
        }
    }
    /// Builds [`CliError::ServerIncompatible`].
    pub fn server_incompatible(message: impl Into<String>) -> Self {
        Self::ServerIncompatible {
            message: message.into(),
        }
    }

    /// Builds [`CliError::RgIncompatible`], directing to `zvec_grep_rg`.
    pub fn rg_incompatible(flag: &str) -> Self {
        Self::RgIncompatible {
            message: format!(
                "rg command option \"{flag}\" is not supported by zg query --rg; use the zvec_grep_rg MCP tool instead."
            ),
        }
    }

    /// Builds [`CliError::DaemonUnavailable`].
    pub fn daemon_unavailable(message: impl Into<String>) -> Self {
        Self::DaemonUnavailable {
            message: message.into(),
        }
    }

    /// Builds [`CliError::Io`].
    pub fn io(path: &std::path::Path, error: impl std::fmt::Display) -> Self {
        Self::Io {
            message: format!("failed to access {}: {error}", path.display()),
        }
    }

    /// Const wire code for CLI-owned variants; `None` for wrapped
    /// engine/daemon errors, which keep their own codes.
    pub const fn own_code(&self) -> Option<EngineErrorCode> {
        match self {
            Self::Usage { .. } => Some(EngineErrorCode::from_static("CLI.USAGE")),
            Self::ConfigInvalid { .. } => Some(EngineErrorCode::from_static("CLI.CONFIG_INVALID")),
            Self::AuthorizationDeclined { .. } => {
                Some(EngineErrorCode::from_static("CLI.AUTHORIZATION_DECLINED"))
            }
            Self::AuthorizationRequired { .. } => {
                Some(EngineErrorCode::from_static("CLI.AUTHORIZATION_REQUIRED"))
            }
            Self::InstallRefused { .. } => {
                Some(EngineErrorCode::from_static("CLI.INSTALL_REFUSED"))
            }
            Self::ServerIncompatible { .. } => {
                Some(EngineErrorCode::from_static("CLI.SERVER_INCOMPATIBLE"))
            }
            Self::RgIncompatible { .. } => {
                Some(EngineErrorCode::from_static("CLI.RG_INCOMPATIBLE"))
            }
            Self::DaemonUnavailable { .. } => {
                Some(EngineErrorCode::from_static("CLI.DAEMON_UNAVAILABLE"))
            }
            Self::NotReady { .. } => Some(EngineErrorCode::from_static("CLI.NOT_READY")),
            Self::Io { .. } => Some(EngineErrorCode::from_static("CLI.IO_FAILED")),
            Self::Engine(_) | Self::Daemon(_) => None,
        }
    }

    /// Rendered wire code: own codes qualified, wrapped codes passed
    /// through untouched (M1: no merging).
    pub fn code(&self) -> String {
        match self {
            Self::Engine(error) => error.code().to_string(),
            Self::Daemon(error) => error.code().to_owned(),
            owned => owned.own_code().map_or_else(
                || "ZVEC_GREP.ENGINE.CLI.UNKNOWN".to_owned(),
                |code| code.qualified(),
            ),
        }
    }
}

impl From<ModelError> for CliError {
    fn from(error: ModelError) -> Self {
        Self::Engine(EngineError::from(error))
    }
}

impl From<AuthError> for CliError {
    fn from(error: AuthError) -> Self {
        Self::Engine(EngineError::from(error))
    }
}

impl From<BackendError> for CliError {
    fn from(error: BackendError) -> Self {
        match error {
            BackendError::Engine(engine) => Self::Engine(engine),
            BackendError::Daemon(daemon) => Self::Daemon(daemon),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn all_codes() -> Vec<String> {
        let owned = [
            CliError::usage("x"),
            CliError::config_invalid("x"),
            CliError::AuthorizationDeclined {
                message: String::new(),
            },
            CliError::AuthorizationRequired {
                message: String::new(),
            },
            CliError::install_refused("x"),
            CliError::server_incompatible("x"),
            CliError::rg_incompatible("--threads"),
            CliError::daemon_unavailable("x"),
            CliError::NotReady {
                message: String::new(),
            },
            CliError::Io {
                message: String::new(),
            },
        ];
        let mut out: Vec<String> = owned
            .iter()
            .map(|error| {
                error
                    .own_code()
                    .expect("CLI-owned variant has a code")
                    .qualified()
            })
            .collect();
        out.sort();
        out.dedup();
        out
    }

    #[test]
    fn wire_codes_match_golden_registry() {
        let expected: Vec<String> = include_str!("../tests/golden/error-codes.txt")
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .map(str::to_owned)
            .collect();
        assert_eq!(all_codes(), expected);
    }

    #[test]
    fn wrapped_errors_keep_their_codes() {
        let engine = CliError::from(zg_core::service::types::empty_query_error());
        assert!(engine.code().contains("CONTEXT.EMPTY_QUERY"));
        assert!(engine.own_code().is_none());
    }

    #[test]
    fn rg_error_directs_to_the_tool() {
        let error = CliError::rg_incompatible("--threads");
        assert!(error.to_string().contains("zvec_grep_rg"));
        assert_eq!(error.code(), "ZVEC_GREP.ENGINE.CLI.RG_INCOMPATIBLE");
    }
}
