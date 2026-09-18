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
        || file_scope_flags_set(args)
        || args.embedding_concurrency.is_some()
        || args.reset_paths
}

/// True when any flag that lands in the constructed [`RootPath`] is set.
///
/// These flags only take effect when the root paths are (re)built from CLI
/// args: an explicit root path constructs a fresh `RootPath`, and
/// `--reset-paths` replaces the recorded ones. On a rootless update of an
/// existing index the facade instead reuses the manifest's recorded root
/// paths and silently ignores these flags (`ZvecGrepService::resolve_root_paths`),
/// which [`rootless_flag_conflict`] turns into a fail-fast usage error.
/// Excludes `--embedding-concurrency` (applies without a root rebuild),
/// credential flags, and `--reset-paths`.
fn file_scope_flags_set(args: &IndexArgs) -> bool {
    !args.globs.is_empty()
        || !args.iglobs.is_empty()
        || !args.file_types.is_empty()
        || !args.excluded_file_types.is_empty()
        || args.hidden
        || args.no_ignore
        || !args.ignore_files.is_empty()
        || args.max_depth.is_some()
        || args.max_filesize.is_some()
        || args.follow
        || args.include_nested_git
}

/// Usage error for a rootless update whose file-scope flags the existing
/// index would silently drop, or `None` when every flag still applies.
///
/// Fires only when all of the following hold: no explicit root path (the
/// run would reuse the recorded root paths), `--reset-paths` absent (the
/// reuse is not overridden), a file-scope flag set (something would be
/// dropped), and the index has recorded root paths to reuse (otherwise the
/// fallback root path is rebuilt from the flags and honors them).
fn rootless_flag_conflict(args: &IndexArgs, existing_root_paths: &[RootPath]) -> Option<CliError> {
    if !args.roots.is_empty()
        || args.reset_paths
        || !file_scope_flags_set(args)
        || existing_root_paths.is_empty()
    {
        return None;
    }
    Some(CliError::usage(
        "index file-scope options cannot be used on a rootless update while the existing index \
         keeps its configured root paths; re-run with --reset-paths to rebuild the root paths \
         from these flags, or pass an explicit root path",
    ))
}

async fn run_index_direct(args: &IndexArgs, root: &PathBuf) -> Result<(), CliError> {
    let absolute = absolute_path(root)?;
    let explicit = !args.roots.is_empty();
    let enabled = !(args.no_progress || args.quiet);
    let reporter = Arc::new(std::sync::Mutex::new(ProgressReporter::new(
        args.color,
        args.no_color,
        enabled,
    )));
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
    let existing_roots = match info_before.workspace_index.as_ref() {
        Some(index) => index.root_paths.as_slice(),
        None => &[],
    };
    if let Some(error) = rootless_flag_conflict(args, existing_roots) {
        return Err(error);
    }
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
        include_nested_git: bool_flag(args.include_nested_git),
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
        include_nested_git: bool_flag(args.include_nested_git),
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
    if args.debug
        && let Some(diagnostics) = &result.scan_diagnostics
    {
        eprintln!("debug: scan diagnostics: {diagnostics:?}");
    }
    if !args.quiet {
        if result.files_scanned == 0 {
            print_no_indexable_files_tip();
        }
        print_index_result("Workspace index", &result);
    }
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
    if server_flags_set(args) {
        return Err(CliError::usage(
            "index credentials and file-scope options cannot be used with server mode; configure the daemon instead",
        ));
    }
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
    if !args.quiet {
        println!("Workspace index: {state}");
        println!("Root: {root_out}");
        println!("Job: {job}");
    }
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
        if !args.quiet {
            println!("Index drop cancelled.");
        }
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
    if !args.quiet {
        if dropped {
            println!("Dropped index for {display}");
        } else {
            println!("No index found for {display}");
        }
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

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use clap::Parser;

    #[tokio::test]
    async fn auto_server_rejects_nested_git_flag_without_daemon() {
        let cli =
            crate::cli::Cli::try_parse_from(["zg", "index", "--include-nested-git", "."]).unwrap();
        let Some(crate::cli::Command::Index(args)) = cli.command else {
            panic!("index command");
        };
        let root = PathBuf::from(".");
        let result = route_by_mode(
            zg_core::config::ClientMode::Auto,
            run_index_direct(args.as_ref(), &root),
            run_index_server(
                args.as_ref(),
                &root,
                DaemonClient::new("http://127.0.0.1:0", None),
            ),
            async { true },
        )
        .await;
        let error = result.expect_err("auto server must reject the file-scope flag");
        assert!(matches!(error, CliError::Usage { .. }), "{error:?}");
        assert_eq!(
            error.to_string(),
            "index credentials and file-scope options cannot be used with server mode; configure the daemon instead"
        );
    }

    /// Parses `zg index <flags>` into args through the real clap surface.
    fn index_args(flags: &[&str]) -> IndexArgs {
        let mut argv = vec!["zg", "index"];
        argv.extend_from_slice(flags);
        let cli = crate::cli::Cli::try_parse_from(argv).unwrap();
        let Some(crate::cli::Command::Index(args)) = cli.command else {
            panic!("index command");
        };
        *args
    }

    #[test]
    fn file_scope_flags_set_covers_exactly_the_root_path_flags() {
        assert!(!file_scope_flags_set(&index_args(&[])));
        // Credentials, embedding concurrency, and --reset-paths apply
        // without a root-path rebuild, so they are not file-scope flags.
        let non_scope: [&[&str]; 6] = [
            &["--embedding", "qwen/text-embedding-v4"],
            &["--api-key", "secret"],
            &["--endpoint", "https://embed.example"],
            &["--device", "cpu"],
            &["--embedding-concurrency", "4"],
            &["--reset-paths"],
        ];
        for flags in non_scope {
            assert!(!file_scope_flags_set(&index_args(flags)), "{flags:?}");
        }
        // Every flag that lands in the constructed RootPath counts.
        let scope: [&[&str]; 11] = [
            &["--glob", "*.rs"],
            &["--iglob", "*.md"],
            &["--type", "rust"],
            &["--type-not", "javascript"],
            &["--hidden"],
            &["--no-ignore"],
            &["--ignore-file", ".custom-ignore"],
            &["--max-depth", "3"],
            &["--max-filesize", "10MB"],
            &["--follow"],
            &["--include-nested-git"],
        ];
        for flags in scope {
            assert!(file_scope_flags_set(&index_args(flags)), "{flags:?}");
        }
    }

    #[test]
    fn server_flags_set_still_covers_every_rejected_group() {
        assert!(!server_flags_set(&index_args(&[])));
        let rejected: [&[&str]; 7] = [
            &["--embedding", "qwen/text-embedding-v4"],
            &["--api-key", "secret"],
            &["--endpoint", "https://embed.example"],
            &["--device", "cpu"],
            &["--include-nested-git"],
            &["--embedding-concurrency", "4"],
            &["--reset-paths"],
        ];
        for flags in rejected {
            assert!(server_flags_set(&index_args(flags)), "{flags:?}");
        }
    }

    #[test]
    fn display_flags_never_count_as_scope_or_server_flags() {
        assert!(!file_scope_flags_set(&index_args(&["--color", "always"])));
        assert!(!file_scope_flags_set(&index_args(&["--no-color"])));
        assert!(!file_scope_flags_set(&index_args(&["--no-progress"])));
        assert!(!server_flags_set(&index_args(&["--color", "always"])));
        assert!(!server_flags_set(&index_args(&["--no-color"])));
        assert!(!server_flags_set(&index_args(&["--no-progress"])));
        assert!(index_args(&["--no-progress"]).no_progress);
        assert!(index_args(&["--quiet"]).quiet);
    }

    #[test]
    fn drop_with_quiet_or_no_progress_is_silent_not_rejected() {
        // `--quiet`/`--no-progress` only mute output; combining them with
        // `--drop` must validate so the run stays silent instead of
        // failing with a conflict error.
        for flags in [
            ["--drop", "--yes", "--quiet"],
            ["--drop", "--yes", "--no-progress"],
        ] {
            let mut argv = vec!["zg", "index"];
            argv.extend(flags);
            let cli = crate::cli::Cli::try_parse_from(argv).unwrap();
            crate::cli::validate(&cli).expect("--drop with silence flags must validate");
        }
    }

    #[test]
    fn rootless_flag_conflict_full_condition_matrix() {
        let configured = vec![RootPath::default()];
        // Rootless + file-scope flag + recorded root paths: the flags would
        // be silently dropped, so the guard fires.
        let error = rootless_flag_conflict(&index_args(&["--include-nested-git"]), &configured)
            .expect("rootless update over configured roots must fail fast");
        assert!(matches!(error, CliError::Usage { .. }), "{error:?}");
        assert!(error.to_string().contains("--reset-paths"), "{error}");
        // --reset-paths rebuilds the root paths from the flags.
        assert!(
            rootless_flag_conflict(
                &index_args(&["--include-nested-git", "--reset-paths"]),
                &configured
            )
            .is_none()
        );
        // An explicit root path rebuilds the RootPath from the flags.
        assert!(
            rootless_flag_conflict(&index_args(&["--include-nested-git", "."]), &configured)
                .is_none()
        );
        // No recorded root paths: the fallback root path honors the flags.
        assert!(rootless_flag_conflict(&index_args(&["--include-nested-git"]), &[]).is_none());
        // No file-scope flag set: nothing can be dropped.
        assert!(rootless_flag_conflict(&index_args(&[]), &configured).is_none());
    }

    #[tokio::test]
    async fn rootless_update_over_indexed_workspace_fails_fast_instead_of_dropping_flags() {
        use tempfile::TempDir;
        use zg_core::models::EmbeddingModel;
        use zg_core::models::stub::StubEmbeddingModel;
        use zg_core::service::facade::CreateZvecGrepOptions;

        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.txt"), "fixture content\n").unwrap();
        let stub: Arc<dyn EmbeddingModel> = Arc::new(StubEmbeddingModel::new(64));
        let setup = create_zvec_grep(CreateZvecGrepOptions {
            root: Some(dir.path().to_path_buf()),
            embedding: None,
            embedding_model: Some(stub),
            api_key: None,
            endpoint: None,
            model_cache_dir: None,
        });
        setup
            .ensure_index(&ZvecGrepIndexOptions {
                root: Some(dir.path()),
                root_paths: Vec::new(),
                rebuild: false,
                reset_paths: false,
                include_paths: Vec::new(),
                exclude_paths: Vec::new(),
                globs: Vec::new(),
                insensitive_globs: Vec::new(),
                file_types: Vec::new(),
                excluded_file_types: Vec::new(),
                hidden: None,
                no_ignore: None,
                ignore_files: Vec::new(),
                max_depth: None,
                max_file_size_bytes: None,
                follow: None,
                include_nested_git: None,
                embedding_concurrency: None,
                on_progress: None,
                changed_paths: Vec::new(),
                signal: None,
            })
            .unwrap();
        let info = setup.workspace_info(Some(dir.path())).unwrap();
        assert!(
            !info.workspace_index.unwrap().root_paths.is_empty(),
            "setup must record root paths"
        );

        // Rootless CLI run (no positional root) against the indexed
        // workspace: the guard must fire before any indexing work.
        let args = index_args(&["--include-nested-git"]);
        let error = run_index_direct(&args, &dir.path().to_path_buf())
            .await
            .expect_err("rootless update must fail fast instead of dropping the flag");
        assert!(matches!(error, CliError::Usage { .. }), "{error:?}");
        assert!(error.to_string().contains("--reset-paths"), "{error}");
    }
}
