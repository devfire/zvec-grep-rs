//! Cross-flag shape validation with `args.ts`-verbatim messages.
//!
//! Placement the clap tree already enforces (a flag that only exists on
//! one subcommand) needs no check here; everything below depends on
//! values or combinations clap cannot express.

use crate::error::CliError;

use super::auth::{AuthAction, AuthArgs};
use super::config::{ConfigArgs, ConfigModelOp, ConfigProviderOp, ConfigTarget};
use super::index::IndexArgs;
use super::integrate::{InstallArgs, UninstallArgs};
use super::query::QueryArgs;
use super::server::{ServerAction, ServerArgs};
use super::status::StatusArgs;
use super::values::{ClientModeArg, McpTransportArg};
use super::{Cli, Command};

/// Validates cross-flag shapes with `args.ts`-verbatim messages.
///
/// Placement the clap tree already enforces (a flag that only exists on
/// one subcommand) needs no check here; everything below depends on
/// values or combinations clap cannot express.
pub fn validate(cli: &Cli) -> Result<(), CliError> {
    let Some(command) = &cli.command else {
        return Ok(());
    };
    match command {
        Command::Query(args) => validate_query(args),
        Command::Index(args) => validate_index(args),
        Command::Status(args) => validate_status(args),
        Command::Install(args) => validate_install(args),
        Command::Uninstall(args) => validate_uninstall(args),
        Command::Config(args) => validate_config(args),
        Command::Auth(args) => validate_auth(args),
        Command::Server(args) => validate_server(args),
        Command::Help(_) | Command::Version | Command::Completions(_) | Command::Serve => Ok(()),
    }
}

fn validate_query(args: &QueryArgs) -> Result<(), CliError> {
    if args.json_removed {
        return Err(CliError::usage(
            "--json has been removed; use the default agent markdown output or --human",
        ));
    }
    if let Some(flag) = args.rg_output.first_set() {
        return Err(CliError::usage(format!(
            "{flag} changes rg output and cannot be used with managed --rg"
        )));
    }
    if let Some(flag) = args.rg_compat.first_set() {
        return Err(CliError::rg_incompatible(&flag));
    }
    if let Some(value) = &args.allow_remote {
        if !value.is_empty() {
            return Err(CliError::usage("--allow-remote does not take a value"));
        }
    }
    if args.embedding_rejected.is_some() {
        return Err(CliError::usage(
            "--embedding is not supported with zg query",
        ));
    }
    if args.endpoint_rejected.is_some() {
        return Err(CliError::usage("--endpoint is not supported with zg query"));
    }
    if args.embedding_concurrency_rejected.is_some() {
        return Err(CliError::usage(
            "--embedding-concurrency is not supported with zg query",
        ));
    }
    if args.rg && (args.has_explicit_routes() || !args.hybrid.is_empty()) {
        return Err(CliError::usage(
            "--rg cannot be combined with --hybrid, --fts, or --vector",
        ));
    }
    if args.rg && args.fuse {
        return Err(CliError::usage("--rg cannot be combined with --fuse"));
    }
    if args.force_direct && args.mode != Some(ClientModeArg::Direct) {
        return Err(CliError::usage("--force-direct requires --mode direct"));
    }
    if args.rg && args.preview.is_some() {
        return Err(CliError::usage(
            "--preview is not supported with --rg; use -A/-B/-C for rg context",
        ));
    }
    if args.rg && args.trace {
        return Err(CliError::usage("--rg cannot be combined with --trace"));
    }
    if args.rg && (args.prefer_symbol || !args.symbol_type.is_empty()) {
        return Err(CliError::usage(
            "--rg cannot be combined with indexed symbol options",
        ));
    }
    if args.rg && args.refresh.is_some() {
        return Err(CliError::usage(
            "--rg cannot be combined with indexed refresh options",
        ));
    }
    if !args.rg && args.has_compat_options() {
        let flag = args.first_compat_option();
        return Err(CliError::usage(format!(
            "{flag} can only be used with --rg"
        )));
    }
    if args.has_discovery_options() && !args.rg {
        let flag = args.first_discovery_option();
        return Err(CliError::usage(format!(
            "{flag} can only be used with index commands or zg query --rg"
        )));
    }
    Ok(())
}

fn validate_index(args: &IndexArgs) -> Result<(), CliError> {
    if args.roots.len() > 1 {
        return Err(CliError::usage("zg index accepts at most one root path"));
    }
    if let Some(value) = &args.allow_remote {
        if !value.is_empty() {
            return Err(CliError::usage("--allow-remote does not take a value"));
        }
    }
    if args.force_direct && args.mode != Some(ClientModeArg::Direct) {
        return Err(CliError::usage("--force-direct requires --mode direct"));
    }
    if args.drop
        && (args.rebuild
            || args.reset_paths
            || args.home.is_some()
            || args.embedding.is_some()
            || args.model_cache.is_some()
            || args.device.is_some()
            || args.api_key.is_some()
            || args.endpoint.is_some()
            || !args.globs.is_empty()
            || !args.iglobs.is_empty()
            || !args.file_types.is_empty()
            || !args.excluded_file_types.is_empty()
            || args.hidden
            || args.no_ignore
            || !args.ignore_files.is_empty()
            || args.max_depth.is_some()
            || args.max_filesize.is_some()
            || args.debug
            || args.follow
            || args.embedding_concurrency.is_some())
    {
        return Err(CliError::usage(
            "zg index --drop cannot be combined with indexing options",
        ));
    }
    Ok(())
}

fn validate_status(args: &StatusArgs) -> Result<(), CliError> {
    if args.roots.len() > 1 {
        return Err(CliError::usage("zg status accepts at most one root path"));
    }
    Ok(())
}

fn validate_install(args: &InstallArgs) -> Result<(), CliError> {
    if args.mcp_transport != Some(McpTransportArg::Http) && args.mcp_token_env.is_some() {
        return Err(CliError::usage(
            "--mcp-token-env requires --mcp-transport http",
        ));
    }
    Ok(())
}

fn validate_uninstall(_args: &UninstallArgs) -> Result<(), CliError> {
    Ok(())
}

fn validate_config(args: &ConfigArgs) -> Result<(), CliError> {
    const MISSING: &str = "zg config requires provider set or model set";
    match &args.target {
        None => Err(CliError::usage(MISSING)),
        Some(ConfigTarget::Model(cmd)) => match &cmd.op {
            None => Err(CliError::usage(MISSING)),
            Some(ConfigModelOp::Set(set)) => {
                if set.reference.len() != 1 {
                    return Err(CliError::usage(
                        "zg config model set requires exactly one reference",
                    ));
                }
                Ok(())
            }
        },
        Some(ConfigTarget::Provider(cmd)) => match &cmd.op {
            None => Err(CliError::usage(MISSING)),
            Some(ConfigProviderOp::Set(set)) => {
                if set.reference.len() != 1 {
                    return Err(CliError::usage(
                        "zg config provider set requires exactly one reference",
                    ));
                }
                if set.api_key.is_none() {
                    return Err(CliError::usage("zg config provider set requires --api-key"));
                }
                Ok(())
            }
        },
    }
}

fn validate_auth(args: &AuthArgs) -> Result<(), CliError> {
    let Some(action) = &args.action else {
        return Err(CliError::usage("zg auth requires grant, status, or revoke"));
    };
    match action {
        AuthAction::Grant(grant) => {
            if grant.roots.len() > 1 {
                return Err(CliError::usage("zg auth grant accepts at most one root"));
            }
            Ok(())
        }
        AuthAction::Status(status) => {
            if status.roots.len() > 1 {
                return Err(CliError::usage("zg auth status accepts at most one root"));
            }
            if status.capability_rejected.is_some() || status.scope_rejected.is_some() {
                return Err(CliError::usage(
                    "--capability and --scope can only be used with zg auth grant",
                ));
            }
            Ok(())
        }
        AuthAction::Revoke(revoke) => {
            if revoke.roots.len() > 1 {
                return Err(CliError::usage("zg auth revoke accepts at most one root"));
            }
            if revoke.capability_rejected.is_some() || revoke.scope_rejected.is_some() {
                return Err(CliError::usage(
                    "--capability and --scope can only be used with zg auth grant",
                ));
            }
            Ok(())
        }
    }
}

fn validate_server(args: &ServerArgs) -> Result<(), CliError> {
    let action_name = match &args.action {
        Some(ServerAction::On) => Some("on"),
        Some(ServerAction::Off) => Some("off"),
        Some(ServerAction::Status(_)) => Some("status"),
        Some(ServerAction::Run) => Some("run"),
        None => None,
    };
    if action_name.is_none() && !args.stdio {
        return Err(CliError::usage(
            "zg server requires on, off, status, run, or --stdio",
        ));
    }
    if action_name.is_some() && args.stdio {
        return Err(CliError::usage(
            "--stdio cannot be combined with a server action",
        ));
    }
    if args.listen.is_some()
        && action_name != Some("run")
        && action_name != Some("on")
        && !args.stdio
    {
        return Err(CliError::usage(
            "--listen can only be used with zg server on or run",
        ));
    }
    if args.token_file.is_some() && action_name == Some("status") {
        return Err(CliError::usage(
            "--token-file cannot be used with zg server status",
        ));
    }
    if args.mcp_toolset.is_some()
        && action_name != Some("on")
        && action_name != Some("run")
        && !args.stdio
    {
        return Err(CliError::usage(
            "--mcp-toolset can only be used with zg server on or run",
        ));
    }
    Ok(())
}
