//! `zg install` and `zg uninstall`: agent IDE integrations.

use std::path::{Path, PathBuf};

use crate::cli::{InstallArgs, UninstallArgs, parse_environment_variable, split_targets};
use crate::error::CliError;
use crate::install::{InstallOptions, InstallTarget, detect_targets, install, uninstall};

pub(crate) fn run_install(args: &InstallArgs) -> Result<(), CliError> {
    let home = user_home()?;
    let targets = resolve_targets(&args.target, &home)?;
    let options = install_options(args)?;
    let mut files = Vec::new();
    for target in targets {
        files.extend(install(target, &options, &home)?);
    }
    for file in &files {
        println!("Updated {}", file.display());
    }
    println!("Restart the selected agents or start a new session to load the integration.");
    Ok(())
}

/// Builds install options from CLI flags.
///
/// `--force` alone consents to replacing unmanaged entries; `--yes`
/// only skips confirmation prompts and must never imply `--force`
/// (#62), or a hand-written `zvec_grep` entry would be silently
/// clobbered by `zg install --yes`.
fn install_options(args: &InstallArgs) -> Result<InstallOptions, CliError> {
    let token_env = args
        .mcp_token_env
        .clone()
        .map(|value| parse_environment_variable(&value, "--mcp-token-env"))
        .transpose()?;
    Ok(InstallOptions::new(
        args.mcp_transport,
        args.mcp_toolset,
        args.mcp_tool_timeout,
        token_env,
        args.force,
    ))
}

pub(crate) fn run_uninstall(args: &UninstallArgs) -> Result<(), CliError> {
    let home = user_home()?;
    let targets = resolve_targets(&args.target, &home)?;
    let mut files = Vec::new();
    for target in targets {
        files.extend(uninstall(target, &home)?);
    }
    for file in &files {
        println!("Updated {}", file.display());
    }
    println!("Restart the selected agents or start a new session to apply the change.");
    Ok(())
}

/// Explicit targets win; otherwise the detected integrations apply.
/// No detection means the caller must name one.
fn resolve_targets(target: &[String], home: &Path) -> Result<Vec<InstallTarget>, CliError> {
    let tokens = split_targets(target);
    if tokens.is_empty() {
        let detected = detect_targets(home);
        if detected.is_empty() {
            return Err(CliError::usage(
                "No agent integrations detected. Pass --target claude|codex|opencode|cursor|qwen|qoder.",
            ));
        }
        return Ok(detected);
    }
    tokens
        .iter()
        .map(|token| InstallTarget::parse(token))
        .collect()
}

fn user_home() -> Result<PathBuf, CliError> {
    std::env::var("HOME")
        .ok()
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var("USERPROFILE")
                .ok()
                .filter(|home| !home.is_empty())
                .map(PathBuf::from)
        })
        .ok_or_else(|| CliError::usage("Cannot determine the home directory (set HOME)."))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use clap::Parser;

    fn force_for(argv: &[&str]) -> bool {
        let cli = crate::cli::Cli::try_parse_from(argv).unwrap();
        let Some(crate::cli::Command::Install(install_args)) = cli.command else {
            panic!("install command");
        };
        install_options(&install_args).unwrap().force
    }

    /// Regression for #62: `--yes` skips confirmation prompts only and
    /// must not consent to replacing an unmanaged `zvec_grep` entry;
    /// only `--force` does.
    #[test]
    fn yes_does_not_imply_force() {
        assert!(!force_for(&[
            "zg", "install", "--target", "claude", "--yes"
        ]));
        assert!(!force_for(&["zg", "install", "--target", "claude"]));
        assert!(force_for(&[
            "zg", "install", "--target", "claude", "--force"
        ]));
        assert!(force_for(&[
            "zg", "install", "--target", "claude", "--yes", "--force"
        ]));
    }
}
