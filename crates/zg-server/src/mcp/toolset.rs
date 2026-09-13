//! MCP toolset selection: `agent` (search only) or `full`.
//!
//! Mirrors `../zvec-grep/src/mcp/toolset.ts` (`MCP_TOOLSET_ENV`,
//! `DEFAULT_MCP_TOOLSET`, `parseMcpToolset`, `resolveMcpToolset`).

use crate::mcp::error::McpError;

/// Environment variable selecting the MCP toolset.
pub const MCP_TOOLSET_ENV: &str = "ZVEC_GREP_MCP_TOOLSET";

/// Default toolset: search only.
pub const DEFAULT_MCP_TOOLSET: McpToolset = McpToolset::Agent;

/// Tools exposed over MCP.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum McpToolset {
    /// Search only (`zvec_grep_search`).
    #[default]
    Agent,
    /// Search plus index lifecycle, rg, and status tools.
    Full,
}

impl McpToolset {
    /// Parses an explicit value, mirroring TS `parseMcpToolset`.
    ///
    /// # Errors
    ///
    /// Returns [`McpError::InvalidParams`] when the value is not `"agent"` or `"full"`.
    pub fn parse(value: &str) -> Result<Self, McpError> {
        match value {
            "agent" => Ok(Self::Agent),
            "full" => Ok(Self::Full),
            _ => Err(McpError::invalid_params(format!(
                "Unsupported MCP toolset \"{value}\". Expected \"agent\" or \"full\"."
            ))),
        }
    }

    /// Resolves explicit flag → environment → default, mirroring TS
    /// `resolveMcpToolset`.
    ///
    /// # Errors
    ///
    /// Returns [`McpError::InvalidParams`] when the selected value is not `"agent"` or `"full"`.
    pub fn resolve(explicit: Option<&str>, environment: Option<&str>) -> Result<Self, McpError> {
        match explicit.or(environment) {
            None => Ok(DEFAULT_MCP_TOOLSET),
            Some(value) => Self::parse(value),
        }
    }

    /// Reads the environment variable, falling back to the default.
    ///
    /// # Errors
    ///
    /// Returns [`McpError::InvalidParams`] when the environment value is not `"agent"` or `"full"`.
    pub fn from_environment() -> Result<Self, McpError> {
        let environment = std::env::var(MCP_TOOLSET_ENV).ok();
        Self::resolve(None, environment.as_deref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_flag_over_environment_over_default() {
        assert_eq!(McpToolset::resolve(None, None).unwrap(), McpToolset::Agent);
        assert_eq!(
            McpToolset::resolve(None, Some("full")).unwrap(),
            McpToolset::Full
        );
        assert_eq!(
            McpToolset::resolve(Some("agent"), Some("full")).unwrap(),
            McpToolset::Agent
        );
    }

    #[test]
    fn rejects_unknown_toolsets() {
        assert!(McpToolset::parse("everything").is_err());
    }
}
