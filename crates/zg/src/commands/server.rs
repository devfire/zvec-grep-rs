//! `zg server`: daemon lifecycle (`on`/`off`/`status`/`run`) and `--stdio`.

use std::path::{Path, PathBuf};

use zg_core::models::catalog::ModelReference;
use zg_server::backend::{DaemonBackend, DaemonBackendOptions, ServiceConfig};

use super::support::{device_wire_name, map_device_kind};
use crate::cli::{McpToolsetArg, ServerAction, ServerArgs, ServerStatusArgs};
use crate::error::CliError;
use crate::format::print_control_status;

pub(crate) async fn run_server(args: ServerArgs) -> Result<(), CliError> {
    if args.stdio {
        return run_server_stdio(args).await;
    }
    match &args.action {
        Some(ServerAction::On) => run_server_on(args).await,
        Some(ServerAction::Off) => run_server_off(args).await,
        Some(ServerAction::Status(status)) => run_server_status(&args, status).await,
        Some(ServerAction::Run) => run_server_run(args).await,
        None => Err(CliError::usage(
            "zg server requires on, off, status, run, or --stdio",
        )),
    }
}

fn daemon_home(args: &ServerArgs) -> Option<PathBuf> {
    args.home.clone()
}

/// Builds the daemon backend: every option is set explicitly by
/// destructuring the defaults first, so a new `DaemonBackendOptions`
/// field fails compilation here until its daemon value is decided.
// `service: _` below is that exhaustiveness check, not `..`: allowed against
// `unneeded_field_pattern` so newcomers cannot slip past this function.
#[allow(clippy::unneeded_field_pattern)]
fn daemon_backend(service: ServiceConfig) -> DaemonBackend {
    let DaemonBackendOptions {
        scheduler,
        pool,
        auth,
        logger,
        read_session_ttl,
        runtime_idle_ttl,
        service: _,
    } = DaemonBackendOptions::default();
    DaemonBackend::new(DaemonBackendOptions {
        service,
        scheduler,
        pool,
        auth,
        logger,
        read_session_ttl,
        runtime_idle_ttl,
    })
}

fn daemon_service(args: &ServerArgs) -> ServiceConfig {
    ServiceConfig {
        embedding: args.embedding.clone().map(ModelReference::new),
        api_key: args.api_key.clone(),
        endpoint: args.endpoint.clone(),
        model_cache_dir: args.model_cache.clone(),
        device: args.device.map(map_device_kind).unwrap_or_default(),
        model_override: None,
    }
}

fn resolve_toolset(args: &ServerArgs) -> Result<zg_server::mcp::toolset::McpToolset, CliError> {
    zg_server::mcp::toolset::McpToolset::resolve(
        args.mcp_toolset.map(|toolset| match toolset {
            McpToolsetArg::Agent => "agent",
            McpToolsetArg::Full => "full",
        }),
        std::env::var(zg_server::mcp::toolset::MCP_TOOLSET_ENV)
            .ok()
            .as_deref(),
    )
    .map_err(|error| CliError::usage(error.to_string()))
}

async fn run_server_on(args: ServerArgs) -> Result<(), CliError> {
    let home = daemon_home(&args);
    let status = zg_server::server_controller::server_status(home.as_deref()).await;
    if status.ready {
        if has_model_options(&args) {
            return Err(CliError::usage(
                "zvec-grep server is already running; --embedding/--api-key/--endpoint/--model-cache/--device cannot be applied without restarting it (run `zg server off` first)",
            ));
        }
        print_control_status(&status);
        return Ok(());
    }
    let program =
        std::env::current_exe().map_err(|error| CliError::io(Path::new("<exe>"), error))?;
    let spawn = server_on_spawn_args(&args);
    let status = zg_server::server_controller::start_server(
        &program.to_string_lossy(),
        &spawn,
        home.as_deref(),
        std::time::Duration::from_secs(30),
    )
    .await?;
    print_control_status(&status);
    Ok(())
}

/// True when any model-selection flag was passed to `server on`.
fn has_model_options(args: &ServerArgs) -> bool {
    args.embedding.is_some()
        || args.api_key.is_some()
        || args.endpoint.is_some()
        || args.model_cache.is_some()
        || args.device.is_some()
}

/// Child argv for `server run`: every `on` flag the daemon consumes is
/// forwarded, so the spawned daemon runs with the requested model setup.
fn server_on_spawn_args(args: &ServerArgs) -> Vec<String> {
    let mut spawn = vec!["server".to_owned(), "run".to_owned()];
    if let Some(listen) = &args.listen {
        spawn.push("--listen".to_owned());
        spawn.push(listen.clone());
    }
    if let Some(token_file) = &args.token_file {
        spawn.push("--token-file".to_owned());
        spawn.push(token_file.to_string_lossy().into_owned());
    }
    if let Some(toolset) = args.mcp_toolset {
        spawn.push("--mcp-toolset".to_owned());
        spawn.push(
            match toolset {
                McpToolsetArg::Agent => "agent",
                McpToolsetArg::Full => "full",
            }
            .to_owned(),
        );
    }
    if let Some(home) = &args.home {
        spawn.push("--home".to_owned());
        spawn.push(home.to_string_lossy().into_owned());
    }
    if let Some(embedding) = &args.embedding {
        spawn.push("--embedding".to_owned());
        spawn.push(embedding.clone());
    }
    if let Some(api_key) = &args.api_key {
        spawn.push("--api-key".to_owned());
        spawn.push(api_key.clone());
    }
    if let Some(endpoint) = &args.endpoint {
        spawn.push("--endpoint".to_owned());
        spawn.push(endpoint.clone());
    }
    if let Some(model_cache) = &args.model_cache {
        spawn.push("--model-cache".to_owned());
        spawn.push(model_cache.to_string_lossy().into_owned());
    }
    if let Some(device) = args.device {
        spawn.push("--device".to_owned());
        spawn.push(device_wire_name(device).to_owned());
    }
    spawn
}

async fn run_server_off(args: ServerArgs) -> Result<(), CliError> {
    let home = daemon_home(&args);
    let status = zg_server::server_controller::stop_server(
        home.as_deref(),
        std::time::Duration::from_secs(30),
        args.token_file.clone(),
    )
    .await?;
    print_control_status(&status);
    Ok(())
}

async fn run_server_status(
    args: &ServerArgs,
    status_args: &ServerStatusArgs,
) -> Result<(), CliError> {
    let home = daemon_home(args);
    let status = zg_server::server_controller::server_status(home.as_deref()).await;
    print_control_status(&status);
    if status_args.check_ready && !status.ready {
        return Err(CliError::NotReady {
            message: "zvec-grep server is not ready".to_owned(),
        });
    }
    Ok(())
}

async fn run_server_run(args: ServerArgs) -> Result<(), CliError> {
    let listen = zg_server::config::configured_listen_address(args.listen.as_deref())?;
    let token = zg_server::config::resolve_server_token(None, args.token_file.clone())?;
    let toolset = resolve_toolset(&args)?;
    let home = daemon_home(&args);
    let server_url = format!("http://{}", listen.display());
    let lock =
        zg_server::server_controller::DaemonInstanceLock::acquire(home.as_deref(), &server_url)
            .await?;
    let backend = daemon_backend(daemon_service(&args));
    let server = zg_server::http_server::DaemonHttpServer::new(
        zg_server::http_server::DaemonHttpServerOptions {
            host: listen.host.clone(),
            port: listen.port,
            token,
            backend: backend.clone(),
            mcp_toolset: toolset,
            mcp_endpoint: zg_server::mcp::http_transport::McpHttpEndpointOptions::default(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
        },
    )?;
    let address = server.start().await?;
    eprintln!("zvec-grep server listening on {address}");
    let mut lock = lock;
    lock.mark_ready().await;
    // ONE process-level shutdown signal: HTTP `POST /control/shutdown`
    // cancels the server token; the OS path is Ctrl-C. Either wakes this
    // task, which then runs the single cleanup sequence below (stop
    // listener, close backend, release instance lock).
    let shutdown = server.shutdown_token();
    tokio::select! {
        () = shutdown.cancelled() => {},
        result = tokio::signal::ctrl_c() => {
            result.map_err(|error| {
                CliError::daemon_unavailable(format!(
                    "failed to wait for shutdown signal: {error}"
                ))
            })?;
        }
    }
    eprintln!("shutting down");
    server.close().await;
    backend.close().await;
    lock.release().await;
    Ok(())
}

async fn run_server_stdio(args: ServerArgs) -> Result<(), CliError> {
    let toolset = resolve_toolset(&args)?;
    let backend = daemon_backend(daemon_service(&args));
    zg_server::mcp::stdio_bridge::run_stdio_server(
        backend,
        env!("CARGO_PKG_VERSION").to_owned(),
        toolset,
    )
    .await
    .map_err(|error| CliError::daemon_unavailable(error.to_string()))?;
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::cli::DeviceArg;

    fn args() -> ServerArgs {
        ServerArgs {
            action: Some(ServerAction::On),
            stdio: false,
            listen: None,
            token_file: None,
            mcp_toolset: None,
            home: None,
            embedding: Some("qwen/text-embedding-v4".to_owned()),
            model_cache: Some(PathBuf::from("/tmp/cache")),
            device: Some(DeviceArg::Cpu),
            api_key: Some("secret".to_owned()),
            endpoint: Some("https://example.test/v1".to_owned()),
        }
    }

    #[test]
    fn server_on_forwards_model_options() {
        let spawn = server_on_spawn_args(&args());
        for (flag, value) in [
            ("--embedding", "qwen/text-embedding-v4"),
            ("--api-key", "secret"),
            ("--endpoint", "https://example.test/v1"),
            ("--model-cache", "/tmp/cache"),
            ("--device", "cpu"),
        ] {
            let position = spawn.iter().position(|item| item == flag).unwrap();
            assert_eq!(spawn.get(position + 1).map(String::as_str), Some(value));
        }
        assert!(has_model_options(&args()));
    }
}
