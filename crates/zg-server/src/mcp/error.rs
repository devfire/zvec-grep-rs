//! MCP endpoint errors as a typed enum converging into rmcp payloads.
//!
//! Mirrors the failure surface of `../zvec-grep/src/mcp/tools.ts`
//! (`ProtocolError(-32602)` for invalid input, plain `Error` otherwise).
//! No new wire codes are invented (M1): variants map onto JSON-RPC
//! `-32602` (invalid params) and `-32603` (internal error).

use rmcp::ErrorData;

use crate::backend::BackendError;

/// Typed MCP-layer failure.
#[derive(Debug)]
pub enum McpError {
    /// Caller input failed schema or bound validation.
    InvalidParams {
        /// Human-readable reason (echoes the TS zod messages where set).
        message: String,
    },
    /// Remote-embedding authorization is required but no grant flows
    /// through MCP: the caller must run `zg auth grant`. The TS server
    /// elicits interactively here; without an elicitation channel the
    /// port fails closed instead (see `docs/ts-divergence.md`).
    AuthorizationRequired {
        /// Workspace-scoped hint for the grant command.
        message: String,
    },
    /// Backend command failed (daemon or engine error).
    Backend(BackendError),
    /// Transport failure (stdio/HTTP serving).
    Transport {
        /// Human-readable cause.
        message: String,
    },
}

impl McpError {
    /// Caller-input failure with an owned message.
    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self::InvalidParams {
            message: message.into(),
        }
    }

    /// Maps onto an rmcp JSON-RPC error payload.
    #[must_use]
    pub fn into_error_data(self) -> ErrorData {
        match self {
            Self::InvalidParams { message } => ErrorData::invalid_params(message, None),
            Self::AuthorizationRequired { message } => ErrorData::invalid_params(message, None),
            Self::Backend(error) => ErrorData::internal_error(error.to_string(), None),
            Self::Transport { message } => ErrorData::internal_error(message, None),
        }
    }
}

impl std::fmt::Display for McpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidParams { message }
            | Self::AuthorizationRequired { message }
            | Self::Transport { message } => write!(f, "{message}"),
            Self::Backend(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for McpError {}

impl From<BackendError> for McpError {
    fn from(error: BackendError) -> Self {
        Self::Backend(error)
    }
}

impl From<crate::errors::DaemonError> for McpError {
    fn from(error: crate::errors::DaemonError) -> Self {
        Self::Backend(BackendError::Daemon(error))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_params_maps_to_json_rpc_code() {
        let data = McpError::invalid_params("root is required.").into_error_data();
        assert_eq!(data.code.0, -32602);
    }

    #[test]
    fn authorization_is_fail_closed_invalid_params() {
        let data = McpError::AuthorizationRequired {
            message: "Remote Embedding authorization is required.".to_owned(),
        }
        .into_error_data();
        assert_eq!(data.code.0, -32602);
    }

    #[test]
    fn backend_failure_maps_to_internal_error() {
        let data = McpError::Backend(BackendError::Daemon(
            crate::errors::DaemonError::ShuttingDown,
        ))
        .into_error_data();
        assert_eq!(data.code.0, -32603);
    }
}
