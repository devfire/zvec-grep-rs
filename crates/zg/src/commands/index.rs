//! `zg index`: build, refresh, inspect failures of, or drop the workspace index.
//!
//! Server-mode index sends only `{root, rebuild, wait, debug}`:
//! per-request credentials and index scoping are daemon configuration
//! in this port and fail fast here instead of tripping the server
//! rejection (see `docs/ts-divergence.md`).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Value, json};

use zg_core::authorization::{
    PlanIndexInput, RemoteEmbeddingPermit, plan_remote_index_authorization,
    with_remote_embedding_operation_permit,
};
use zg_core::config::ClientMode;
use zg_core::models::embeddings::{CreateEmbeddingModelOptions, DeviceKind};
use zg_core::models::factory::create_embedding_model;
use zg_core::service::facade::create_zvec_grep;
use zg_core::service::types::{RootPathSpec, ZvecGrepIndexOptions};
use zg_core::types::{IndexProgress, RootPath};

use super::authz::{authorize_plan, read_choice};
use super::catalog::{catalog_entry, catalog_identity, catalog_reference, catalog_reference_for};
use super::support::{
    absolute_path, bool_flag, cancel_flag, server_available, service_options, single_root_or_cwd,
};
use crate::cli::{IndexArgs, parse_byte_size};
use crate::client::{DaemonClient, resolve_client_mode, route_by_mode};
use crate::error::CliError;
use crate::format::{ProgressReporter, print_index_result, print_no_indexable_files_tip};

pub(crate) async fn run_index(args: IndexArgs) -> Result<(), CliError> {
    let root = single_root_or_cwd(&args.roots, "zg index accepts at most one root path")?;
    if args.drop {
        return run_index_drop(&args, &root).await;
    }
    if let Some(reference) = &args.embedding {
        catalog_reference(reference)?;
    }
    if server_flags_set(&args) {
        // Fail fast: the daemon owns index configuration and would
        // reject these per-request overrides.
        let mode = resolve_client_mode(args.mode)?;
        if mode == ClientMode::Server {
            return Err(CliError::usage(
                "index credentials and file-scope options cannot be used with server mode; configure the daemon instead",
            ));
        }
    }
    let mode = resolve_client_mode(args.mode)?;
    route_by_mode(
        mode,
        run_index_direct(&args, &root),
        async {
            let client = DaemonClient::from_env(None, args.home.as_deref())?;
            run_index_server(&args, &root, client).await
        },
        server_available(args.home.as_deref()),
    )
    .await
}

/// True when any credential or file-scope flag is set.
fn server_flags_set(args: &IndexArgs) -> bool {
    args.embedding.is_some()
        || args.api_key.is_some()
        || args.endpoint.is_some()
        || args.device.is_some()
        || !args.globs.is_empty()
        || !args.iglobs.is_empty()
        || !args.file_types.is_empty()
        || !args.excluded_file_types.is_empty()
        || args.hidden
        || args.no_ignore
        || !args.ignore_files.is_empty()
        || args.max_depth.is_some()
        || args.max_filesize.is_some()
        || args.follow
        || args.embedding_concurrency.is_some()
        || args.reset_paths
}

async fn run_index_direct(args: &IndexArgs, root: &PathBuf) -> Result<(), CliError> {
    let absolute = absolute_path(root)?;
    let explicit = !args.roots.is_empty();
    let reporter = Arc::new(std::sync::Mutex::new(ProgressReporter::new(false)));
    let sink: zg_core::pipeline::indexing::IndexProgressSink = {
        let reporter = Arc::clone(&reporter);
        Arc::new(move |progress: IndexProgress| {
            if let Ok(mut reporter) = reporter.lock() {
                reporter.report(&progress);
            }
        })
    };
    let service = create_zvec_grep(service_options(
        Some(absolute.clone()),
        args.embedding.clone(),
        args.api_key.clone(),
        args.endpoint.clone(),
        args.model_cache.clone(),
        args.device,
    ));
    let info_before = service.workspace_info(Some(absolute.as_path()))?;
    assert_model_compatible(&info_before, args.embedding.as_deref(), args.rebuild)?;
    let permit = plan_index_permit(&info_before, args).await?;
    let root_path = RootPath {
        absolute_path: absolute.to_string_lossy().into_owned(),
        recursive: true,
        include: Vec::new(),
        exclude: Vec::new(),
        globs: args.globs.clone(),
        insensitive_globs: args.iglobs.clone(),
        file_types: args.file_types.clone(),
        excluded_file_types: args.excluded_file_types.clone(),
        hidden: bool_flag(args.hidden),
        no_ignore: bool_flag(args.no_ignore),
        ignore_files: args.ignore_files.clone(),
        max_depth: args.max_depth,
        max_file_size_bytes: args
            .max_filesize
            .as_deref()
            .map(parse_byte_size)
            .transpose()?,
        follow: bool_flag(args.follow),
    };
    let root_spec = if explicit {
        vec![RootPathSpec::Full(Box::new(root_path))]
    } else {
        Vec::new()
    };
    let cancel = cancel_flag();
    let options = ZvecGrepIndexOptions {
        root: Some(absolute.as_path()),
        root_paths: root_spec,
        rebuild: args.rebuild,
        reset_paths: args.reset_paths,
        include_paths: Vec::new(),
        exclude_paths: Vec::new(),
        globs: args.globs.clone(),
        insensitive_globs: args.iglobs.clone(),
        file_types: args.file_types.clone(),
        excluded_file_types: args.excluded_file_types.clone(),
        hidden: bool_flag(args.hidden),
        no_ignore: bool_flag(args.no_ignore),
        ignore_files: args.ignore_files.clone(),
        max_depth: args.max_depth,
        max_file_size_bytes: args
            .max_filesize
            .as_deref()
            .map(parse_byte_size)
            .transpose()?,
        follow: bool_flag(args.follow),
        embedding_concurrency: args.embedding_concurrency,
        on_progress: Some(sink),
        changed_paths: Vec::new(),
        signal: Some(cancel.check()),
    };
    let result = with_remote_embedding_operation_permit(permit, || service.ensure_index(&options));
    if let Ok(reporter) = reporter.lock() {
        reporter.finish();
    }
    let result: zg_core::types::IndexResult = result?;
    if args.debug {
        if let Some(diagnostics) = &result.scan_diagnostics {
            eprintln!("debug: scan diagnostics: {diagnostics:?}");
        }
    }
    if result.files_scanned == 0 {
        print_no_indexable_files_tip();
    }
    print_index_result("Workspace index", &result);
    Ok(())
}

/// Mirrors `assertEmbeddingModelCompatible`: a requested reference that
/// disagrees with the recorded schema needs `--rebuild`.
fn assert_model_compatible(
    info: &zg_core::service::types::ZvecGrepInfoResult,
    requested: Option<&str>,
    rebuild: bool,
) -> Result<(), CliError> {
    let Some(requested) = requested else {
        return Ok(());
    };
    let existing = info
        .workspace_index
        .as_ref()
        .and_then(|index| index.embedding.clone())
        .flatten();
    let Some(existing) = existing else {
        return Ok(());
    };
    let reference = catalog_reference(requested)?;
    let entry = catalog_entry(reference.as_str()).ok_or_else(|| {
        CliError::config_invalid(format!("Unknown embedding model \"{requested}\""))
    })?;
    let (_, provider, model, _) = catalog_identity(entry);
    if (provider != existing.provider || model != existing.model) && !rebuild {
        return Err(CliError::usage(format!(
            "Requested embedding model \"{requested}\" does not match the indexed model \"{}/{}\". Re-run with --rebuild to reindex.",
            existing.provider, existing.model
        )));
    }
    Ok(())
}

async fn plan_index_permit(
    info: &zg_core::service::types::ZvecGrepInfoResult,
    args: &IndexArgs,
) -> Result<Option<RemoteEmbeddingPermit>, CliError> {
    let schema = info
        .workspace_index
        .as_ref()
        .and_then(|index| index.embedding.clone())
        .flatten();
    let requested = args.embedding.clone();
    let reference = match (requested, schema) {
        (Some(reference), _) => {
            let resolved = catalog_reference(&reference)?;
            let entry = catalog_entry(resolved.as_str()).ok_or_else(|| {
                CliError::config_invalid(format!("Unknown embedding model \"{reference}\""))
            })?;
            let (_, provider, _, _) = catalog_identity(entry);
            if provider != "qwen" {
                return Ok(None);
            }
            Some(resolved)
        }
        (None, Some(existing)) if existing.provider == "qwen" => {
            let resolved =
                catalog_reference_for(&existing.provider, &existing.model).ok_or_else(|| {
                    CliError::usage(format!(
                        "Unsupported remote embedding provider: {}",
                        existing.provider
                    ))
                })?;
            Some(resolved)
        }
        _ => None,
    };
    let Some(reference) = reference else {
        return Ok(None);
    };
    let model = create_embedding_model(
        &reference,
        &CreateEmbeddingModelOptions {
            api_key: args.api_key.clone(),
            endpoint: args.endpoint.clone(),
            model_cache_dir: args.model_cache.clone(),
            device: DeviceKind::Auto,
        },
    )?;
    let plan = plan_remote_index_authorization(&PlanIndexInput {
        info,
        model: model.info(),
        rebuild: args.rebuild,
        needs_update: true,
    })?;
    let Some(plan) = plan else {
        return Ok(None);
    };
    Ok(authorize_plan(&plan, args.allow_remote.is_some(), false)
        .await?
        .permit)
}

async fn run_index_server(
    args: &IndexArgs,
    root: &PathBuf,
    client: DaemonClient,
) -> Result<(), CliError> {
    let absolute = absolute_path(root)?;
    let result = client
        .call_tool(
            "zvec_grep_index",
            json!({
                "root": absolute.to_string_lossy(),
                "rebuild": args.rebuild,
                "wait": true,
                "debug": args.debug,
            }),
        )
        .await?;
    let structured = result.structured.clone();
    let state = structured
        .get("state")
        .and_then(Value::as_str)
        .unwrap_or("submitted");
    let root_out = structured
        .get("root")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| absolute.to_string_lossy().into_owned());
    let job = structured
        .get("job_id")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    println!("Workspace index: {state}");
    println!("Root: {root_out}");
    println!("Job: {job}");
    if state == "failed" {
        let detail = structured
            .get("error")
            .and_then(|error| error.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("index job failed");
        return Err(CliError::usage(format!("Workspace index failed: {detail}")));
    }
    Ok(())
}

async fn run_index_drop(args: &IndexArgs, root: &Path) -> Result<(), CliError> {
    let absolute = absolute_path(root)?;
    if !confirm_index_drop(&absolute, args.yes)? {
        println!("Index drop cancelled.");
        return Ok(());
    }
    let display = absolute.to_string_lossy().into_owned();
    let mode = resolve_client_mode(args.mode)?;
    let dropped = route_by_mode(
        mode,
        async {
            let service = create_zvec_grep(service_options(
                Some(absolute.clone()),
                None,
                None,
                None,
                None,
                None,
            ));
            service
                .drop_index(Some(absolute.as_path()))
                .map_err(CliError::from)
        },
        async {
            let client = DaemonClient::from_env(None, args.home.as_deref())?;
            drop_via_server(&client, &display).await
        },
        server_available(args.home.as_deref()),
    )
    .await?;
    if dropped {
        println!("Dropped index for {display}");
    } else {
        println!("No index found for {display}");
    }
    Ok(())
}

/// Drops one index through the daemon.
async fn drop_via_server(client: &DaemonClient, root: &str) -> Result<bool, CliError> {
    let result = client
        .call_tool("zvec_grep_index_drop", json!({"root": root}))
        .await?;
    Ok(result
        .structured
        .get("removed")
        .and_then(Value::as_bool)
        .unwrap_or(false))
}

/// Mirrors `confirmIndexDrop`: `--yes` wins, non-TTY without it errors,
/// otherwise a `[y/N]` prompt on stdout.
fn confirm_index_drop(root: &Path, yes: bool) -> Result<bool, CliError> {
    if yes {
        return Ok(true);
    }
    if !std::io::IsTerminal::is_terminal(&std::io::stdin())
        || !std::io::IsTerminal::is_terminal(&std::io::stdout())
    {
        return Err(CliError::usage(
            "zg index --drop requires --yes in a non-interactive shell",
        ));
    }
    let answer = read_choice(&format!("Drop the index for {}? [y/N] ", root.display()))?;
    Ok(matches!(answer.trim().to_lowercase().as_str(), "y" | "yes"))
}
