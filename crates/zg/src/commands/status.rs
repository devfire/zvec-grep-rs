//! `zg status`: report workspace index state, with `--check-ready` gating.

use std::path::Path;

use serde_json::{Value, json};

use zg_core::service::facade::create_zvec_grep;

use super::support::{absolute_path, server_available, service_options, single_root_or_cwd};
use crate::cli::StatusArgs;
use crate::client::{DaemonClient, resolve_client_mode, route_by_mode};
use crate::error::CliError;
use crate::format::{print_workspace_info, use_color};

pub(crate) async fn run_status(args: StatusArgs) -> Result<(), CliError> {
    let root = single_root_or_cwd(&args.roots, "zg status accepts at most one root path")?;
    let absolute = absolute_path(&root)?;
    let mode = resolve_client_mode(args.mode)?;
    let state = route_by_mode(
        mode,
        run_status_direct(&args, &absolute),
        async {
            let client = DaemonClient::from_env(None, args.home.as_deref())?;
            run_status_server(&client, &absolute).await
        },
        server_available(args.home.as_deref()),
    )
    .await?;
    if args.check_ready && state != "ready" {
        return Err(CliError::NotReady {
            message: format!("Workspace index is not ready (state: {state})"),
        });
    }
    Ok(())
}

async fn run_status_direct(args: &StatusArgs, absolute: &Path) -> Result<String, CliError> {
    let color = use_color(args.color, args.no_color);
    let service = create_zvec_grep(service_options(
        Some(absolute.to_owned()),
        args.embedding.clone(),
        args.api_key.clone(),
        args.endpoint.clone(),
        args.model_cache.clone(),
        args.device,
    ));
    let info = service.workspace_info(Some(absolute))?;
    let state = print_workspace_info(&info, color.enabled());
    Ok(state.as_str().to_owned())
}

async fn run_status_server(client: &DaemonClient, absolute: &Path) -> Result<String, CliError> {
    let display = absolute.to_string_lossy().into_owned();
    let result = client
        .call_tool("zvec_grep_index_status", json!({"root": display}))
        .await?;
    if !result.text.trim().is_empty() {
        println!("{}", result.text);
    }
    Ok(server_index_state(&result.structured))
}

/// Derives ready/stale/failed/unindexed from `IndexStatusOutput`.
fn server_index_state(structured: &Value) -> String {
    let indexed = structured
        .get("indexed")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !indexed {
        return "unindexed".to_owned();
    }
    let runtime = structured.get("runtime");
    if runtime.and_then(|runtime| runtime.get("error")).is_some() {
        return "failed".to_owned();
    }
    let live = runtime
        .and_then(|runtime| runtime.get("job_state"))
        .and_then(Value::as_str)
        .is_some_and(|state| state == "queued" || state == "running");
    if live { "stale" } else { "ready" }.to_owned()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn server_index_state_mapping() {
        assert_eq!(server_index_state(&json!({"indexed": false})), "unindexed");
        assert_eq!(server_index_state(&json!({"indexed": true})), "ready");
        assert_eq!(
            server_index_state(&json!({"indexed": true, "runtime": {"error": {}}})),
            "failed"
        );
        assert_eq!(
            server_index_state(&json!({"indexed": true, "runtime": {"job_state": "running"}})),
            "stale"
        );
    }
}
