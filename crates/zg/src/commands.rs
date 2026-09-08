//! Command handlers: direct engine calls or daemon tool calls per mode
//! (`cli/commands.ts` + `cli/auth.ts` behavior).
//!
//! Direct mode drives `ZvecGrepService` in-process with the same permit
//! guard as the daemon (authorization lives in `zg-core`, phase F);
//! server mode goes through [`DaemonClient`]. Server-mode index sends
//! only `{root, rebuild, wait, debug}`: per-request credentials and
//! index scoping are daemon configuration in this port and fail fast
//! here instead of tripping the server rejection (see
//! `docs/ts-divergence.md`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::{Value, json};
use zg_core::authorization::{PlanIndexInput, PlanSearchInput, RemoteEmbeddingPromptInput};
use zg_core::authorization::{
    RemoteEmbeddingAuthorizationManager, RemoteEmbeddingPermit, RemoteEmbeddingScope,
    create_remote_embedding_target, format_remote_embedding_authorization_prompt,
    plan_remote_index_authorization, plan_remote_search_authorization,
    remote_embedding_disclosure_data, with_remote_embedding_operation_permit,
};
use zg_core::config::{
    ClientMode, EmbeddingDevice, EmbeddingModelConfig, GlobalConfigUpdate, GlobalDefaults,
    ProviderConfig,
};
use zg_core::lexical::LexicalSearchOptions;
use zg_core::models::catalog::{EmbeddingCatalogEntry, ModelReference, list_embedding_models};
use zg_core::models::embeddings::{CreateEmbeddingModelOptions, DeviceKind};
use zg_core::models::factory::create_embedding_model;
use zg_core::service::facade::{CreateZvecGrepOptions, ZvecGrepService, create_zvec_grep};
use zg_core::service::types::{
    ContextDiagnostics, ContextSource, ZvecGrepContextOptions, ZvecGrepContextResult,
    ZvecGrepIndexOptions,
};
use zg_core::types::{
    CodeSymbolType, IndexProgress, RootPath, SearchPlanRoute, SearchPlanRouteMode,
};
use zg_server::backend::{DaemonBackend, DaemonBackendOptions, ServiceConfig};

use crate::cli::{
    AuthAction, Command, ConfigModelOp, ConfigProviderOp, ConfigTarget, DeviceArg, IndexArgs,
    QueryArgs, RefreshMode, ServerAction, StatusArgs, SymbolType, parse_byte_size,
    parse_environment_variable, parse_modified_time, split_targets,
};
use crate::client::{
    DaemonClient, ServerSearchBody, parse_server_search_response, resolve_client_mode,
    resolve_direct_search_policy, resolve_server_search_policy, resolve_server_url, route_by_mode,
};
use crate::error::CliError;
use crate::format::{
    ProgressReporter, print_context_result, print_context_warnings, print_control_status,
    print_index_result, print_no_indexable_files_tip, print_workspace_info, use_color,
};
use crate::install::{InstallOptions, InstallTarget, detect_targets, install, uninstall};

/// Runs the parsed CLI tree.
pub async fn run(cli: crate::cli::Cli) -> Result<(), CliError> {
    match cli.command {
        None => print_main_help(),
        Some(Command::Query(args)) => run_query(*args).await,
        Some(Command::Index(args)) => run_index(*args).await,
        Some(Command::Status(args)) => run_status(args).await,
        Some(Command::Install(args)) => run_install(args),
        Some(Command::Uninstall(args)) => run_uninstall(args),
        Some(Command::Config(args)) => run_config(args),
        Some(Command::Auth(args)) => run_auth(args),
        Some(Command::Server(args)) => run_server(args).await,
        Some(Command::Help(args)) => run_help(args),
        Some(Command::Version) => {
            println!("{}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some(Command::Completions(args)) => {
            let mut command = crate::cli::Cli::command();
            clap_complete::generate(args.shell, &mut command, "zg", &mut std::io::stdout());
            Ok(())
        }
        Some(Command::Serve) => Err(CliError::usage(
            "zg serve has been removed; use zg server on and Streamable HTTP MCP",
        )),
    }
}

use clap::CommandFactory;

fn print_main_help() -> Result<(), CliError> {
    crate::cli::Cli::command()
        .print_long_help()
        .map_err(|error| CliError::io(Path::new("<stdout>"), error))?;
    println!();
    Ok(())
}

fn run_help(args: crate::cli::HelpArgs) -> Result<(), CliError> {
    let Some(topic) = args.topic else {
        return print_main_help();
    };
    if topic == "models" {
        print_models_help();
        return Ok(());
    }
    let mut command = crate::cli::Cli::command();
    let known = [
        "query",
        "index",
        "status",
        "install",
        "uninstall",
        "config",
        "auth",
        "server",
        "help",
        "version",
        "completions",
    ];
    // Topics name top-level subcommands only.
    if known.contains(&topic.as_str()) {
        for sub in command.get_subcommands_mut() {
            if sub.get_name() == topic {
                sub.print_long_help()
                    .map_err(|error| CliError::io(Path::new("<stdout>"), error))?;
                println!();
                return Ok(());
            }
        }
    }
    print_main_help()
}

fn print_models_help() {
    println!("Embedding models:");
    for entry in list_embedding_models() {
        let (reference, provider, model, dimension) = catalog_identity(entry);
        println!("  {reference}  ({provider}/{model}, dim {dimension})");
    }
}

// ---------------------------------------------------------------------------
// query
// ---------------------------------------------------------------------------

async fn run_query(args: QueryArgs) -> Result<(), CliError> {
    let queries: Vec<String> = args
        .queries
        .iter()
        .chain(args.hybrid.iter())
        .map(|query| query.trim().to_owned())
        .filter(|query| !query.is_empty())
        .collect();
    if queries.is_empty()
        && args.fts.is_empty()
        && args.vector.is_empty()
        && args.regexp.is_empty()
        && args.pattern_files.is_empty()
    {
        return Err(CliError::usage(if args.rg {
            "zg query --rg requires a pattern. Use zg help query for examples."
        } else {
            "zg query requires text or --hybrid/--fts/--vector routes. Use zg help query for examples."
        }));
    }
    if args.rg {
        return run_rg_direct(args, queries).await;
    }
    let mode = resolve_client_mode(args.mode)?;
    route_by_mode(
        mode,
        run_query_direct(&args, &queries),
        async {
            let client = DaemonClient::from_env(None, args.home.as_deref())?;
            run_query_server(&args, &queries, client).await
        },
        async {
            match DaemonClient::from_env(None, args.home.as_deref()) {
                Ok(client) => client.server_available().await,
                Err(_) => false,
            }
        },
    )
    .await
}

/// Builds direct-mode context options; `auto_update` follows the direct
/// search policy (`wait` only).
fn direct_context_options<'a>(
    args: &'a QueryArgs,
    queries: &[String],
    fts_only: bool,
) -> Result<ZvecGrepContextOptions<'a>, CliError> {
    let policy = resolve_direct_search_policy(args.refresh);
    let mut primary = queries.to_vec();
    let mut fts = args.fts.clone();
    let mut vector = args.vector.clone();
    if fts_only {
        fts.extend(primary.iter().cloned());
        primary.clear();
        vector.clear();
    }
    let (query, rest) = match primary.split_first() {
        Some((first, rest)) => (Some(first.clone()), rest.to_vec()),
        None => (None, Vec::new()),
    };
    let routes: Vec<SearchPlanRoute> = fts
        .iter()
        .map(|term| SearchPlanRoute {
            mode: SearchPlanRouteMode::Fts,
            query: term.clone(),
        })
        .chain(vector.iter().map(|term| SearchPlanRoute {
            mode: SearchPlanRouteMode::Vector,
            query: term.clone(),
        }))
        .collect();
    Ok(ZvecGrepContextOptions {
        root: None,
        query,
        queries: rest,
        routes,
        fts: Vec::new(),
        vector: Vec::new(),
        fuse: args.fuse,
        limit: args.limit,
        trace: args.trace,
        track_entity_id: None,
        prefer_symbol: args.prefer_symbol,
        symbol_types: map_symbol_types(&args.symbol_type),
        include_paths: Vec::new(),
        exclude_paths: Vec::new(),
        globs: args.globs.clone(),
        insensitive_globs: args.iglobs.clone(),
        file_types: args.file_types.clone(),
        excluded_file_types: args.excluded_file_types.clone(),
        modified_after: map_modified_time(args.modified_after.as_deref(), "--modified-after")?,
        modified_before: map_modified_time(args.modified_before.as_deref(), "--modified-before")?,
        rg: None,
        auto_update: policy.auto_update,
        signal: None,
    })
}

#[allow(clippy::too_many_lines)]
async fn run_query_direct(args: &QueryArgs, queries: &[String]) -> Result<(), CliError> {
    if args.refresh == Some(RefreshMode::Background) {
        eprintln!(
            "warning: --refresh background requires Server mode; Direct mode uses --refresh off"
        );
    }
    let color = use_color(args.color, args.no_color);
    let service = create_zvec_grep(service_options(
        None,
        None,
        args.api_key.clone(),
        None,
        args.model_cache.clone(),
        args.device,
    ));
    let info = service.workspace_info(None)?;
    let uses_vector = !queries.is_empty() || !args.vector.is_empty();
    let permit = plan_search_permit(&info, args.api_key.clone(), uses_vector, args).await?;
    let request = direct_context_options(args, queries, false)?;
    let cancel = cancel_flag();
    let mut result = with_remote_embedding_operation_permit(permit.permit, || {
        let mut request = request;
        request.signal = Some(cancel.check());
        service.context(&request)
    })?;
    if permit.fts_fallback {
        let fallback = direct_context_options(args, queries, true)?;
        result = service.context(&fallback)?;
    }
    print_context_result(&result, args.human, color);
    print_context_warnings(&result);
    if result.source == ContextSource::Index && !result.items.is_empty() && args.debug {
        eprintln!(
            "debug: {} item(s), coverage {:?}",
            result.items.len(),
            result.coverage
        );
    }
    Ok(())
}

/// Search-side authorization: existing grant, `--allow-remote` once, TTY
/// prompt, or the verbatim non-TTY refusal. Returns the permit plus
/// whether the user chose FTS-only fallback.
struct SearchPermit {
    permit: Option<RemoteEmbeddingPermit>,
    fts_fallback: bool,
}

async fn plan_search_permit(
    info: &zg_core::service::types::ZvecGrepInfoResult,
    api_key: Option<String>,
    uses_vector: bool,
    args: &QueryArgs,
) -> Result<SearchPermit, CliError> {
    let schema = info
        .workspace_index
        .as_ref()
        .and_then(|index| index.embedding.clone())
        .flatten();
    let Some(schema) = schema else {
        return Ok(SearchPermit {
            permit: None,
            fts_fallback: false,
        });
    };
    if schema.provider != "qwen" {
        if schema.provider != "local" {
            return Err(CliError::usage(format!(
                "Unsupported remote embedding provider: {}",
                schema.provider
            )));
        }
        return Ok(SearchPermit {
            permit: None,
            fts_fallback: false,
        });
    }
    let reference = catalog_reference_for(&schema.provider, &schema.model).ok_or_else(|| {
        CliError::usage(format!(
            "Unsupported remote embedding provider: {}",
            schema.provider
        ))
    })?;
    let model = create_embedding_model(
        &reference,
        &CreateEmbeddingModelOptions {
            api_key,
            endpoint: None,
            model_cache_dir: args.model_cache.clone(),
            device: DeviceKind::Auto,
        },
    )?;
    let plan = plan_remote_search_authorization(&PlanSearchInput {
        info,
        model: model.info(),
        uses_vector,
        auto_update: false,
        freshness_wait: args.refresh == Some(RefreshMode::Wait),
        runtime_needs_reconciliation: false,
    })?;
    let Some(plan) = plan else {
        return Ok(SearchPermit {
            permit: None,
            fts_fallback: false,
        });
    };
    authorize_plan(&plan, args.allow_remote.is_some(), true).await
}

/// Resolves one authorization plan to a permit, mirroring
/// `authorizeCliPlan` including the verbatim texts.
async fn authorize_plan(
    plan: &zg_core::authorization::RemoteEmbeddingPlan,
    allow_remote: bool,
    offer_fts: bool,
) -> Result<SearchPermit, CliError> {
    let manager = RemoteEmbeddingAuthorizationManager::new();
    if let Some(permit) = manager.existing_workspace_permit(&plan.target)? {
        return Ok(SearchPermit {
            permit: Some(permit),
            fts_fallback: false,
        });
    }
    if allow_remote {
        let permit = manager.grant(&plan.target, RemoteEmbeddingScope::Once)?;
        return Ok(SearchPermit {
            permit: Some(permit),
            fts_fallback: false,
        });
    }
    if !std::io::IsTerminal::is_terminal(&std::io::stdin())
        || !std::io::IsTerminal::is_terminal(&std::io::stderr())
    {
        return Err(CliError::AuthorizationRequired {
            message: [
                "Remote Embedding authorization is required.",
                "Re-run with --allow-remote, or grant Workspace authorization:",
                "  zg auth grant --capability embedding --scope workspace",
            ]
            .join("\n"),
        });
    }
    let data = remote_embedding_disclosure_data(plan.disclosure);
    eprintln!(
        "{}",
        format_remote_embedding_authorization_prompt(&RemoteEmbeddingPromptInput {
            workspace_roots: &plan.target.workspace_roots,
            provider: &plan.target.provider,
            model: &plan.target.model,
            endpoint: Some(plan.target.endpoint.as_str()),
            data: &data,
            note: None,
        })
    );
    eprintln!();
    eprintln!("1. Allow once");
    eprintln!("2. Allow for this workspace");
    if offer_fts {
        eprintln!("3. Use FTS only");
    }
    let cancel = if offer_fts { 4 } else { 3 };
    eprintln!("{cancel}. Cancel");
    let answer = read_choice(&format!("Choose [1-{cancel}]: "))?;
    match answer.trim() {
        "1" => Ok(SearchPermit {
            permit: Some(manager.grant(&plan.target, RemoteEmbeddingScope::Once)?),
            fts_fallback: false,
        }),
        "2" => Ok(SearchPermit {
            permit: Some(manager.grant(&plan.target, RemoteEmbeddingScope::Workspace)?),
            fts_fallback: false,
        }),
        "3" if offer_fts => Ok(SearchPermit {
            permit: None,
            fts_fallback: true,
        }),
        _ => Err(CliError::AuthorizationDeclined {
            message: "Remote Embedding authorization was declined. No remote data was sent."
                .to_owned(),
        }),
    }
}

fn read_choice(prompt: &str) -> Result<String, CliError> {
    use std::io::Write;
    let mut stderr = std::io::stderr().lock();
    stderr
        .write_all(prompt.as_bytes())
        .and_then(|()| stderr.flush())
        .map_err(|error| CliError::io(Path::new("<stderr>"), error))?;
    drop(stderr);
    let mut answer = String::new();
    std::io::stdin()
        .read_line(&mut answer)
        .map_err(|error| CliError::io(Path::new("<stdin>"), error))?;
    Ok(answer)
}

async fn run_query_server(
    args: &QueryArgs,
    queries: &[String],
    client: DaemonClient,
) -> Result<(), CliError> {
    let color = use_color(args.color, args.no_color);
    let policy = resolve_server_search_policy(args.refresh);
    let mut arguments = json!({
        "root": absolute_cwd()?,
        "queries": queries,
        "limit": args.limit,
        "fuse": args.fuse,
        "trace": args.trace,
        "preferSymbol": args.prefer_symbol,
        "freshness": policy.freshness.as_wire(),
        "autoUpdate": policy.auto_update,
    });
    if !args.fts.is_empty() {
        arguments["fts"] = json!(args.fts);
    }
    if !args.vector.is_empty() {
        arguments["vector"] = json!(args.vector);
    }
    if !args.globs.is_empty() {
        arguments["globs"] = json!(args.globs);
    }
    if !args.iglobs.is_empty() {
        arguments["insensitiveGlobs"] = json!(args.iglobs);
    }
    if !args.file_types.is_empty() {
        arguments["fileTypes"] = json!(args.file_types);
    }
    if !args.excluded_file_types.is_empty() {
        arguments["excludedFileTypes"] = json!(args.excluded_file_types);
    }
    let result = client.call_tool("zvec_grep_search", arguments).await?;
    match parse_server_search_response(&result)? {
        ServerSearchBody::Text(text) => println!("{text}"),
        ServerSearchBody::Groups(structured) => {
            println!(
                "{}",
                serde_json::to_string_pretty(&structured).unwrap_or_default()
            );
        }
    }
    if args.debug {
        eprintln!("debug: server search via {}", resolve_server_url());
    }
    let _ = color;
    Ok(())
}

// ---------------------------------------------------------------------------
// rg
// ---------------------------------------------------------------------------

/// Direct `--rg`: builds in-process lexical options from CLI flags.
async fn run_rg_direct(args: QueryArgs, queries: Vec<String>) -> Result<(), CliError> {
    let color = use_color(args.color, args.no_color);
    let mut patterns = queries;
    patterns.extend(args.regexp.clone());
    if args.line_regexp {
        patterns = patterns
            .iter()
            .map(|pattern| format!("^(?:{pattern})$"))
            .collect();
    }
    let (before, after) = match (args.context, args.before_context, args.after_context) {
        (Some(both), None, None) => (both as usize, both as usize),
        _ => (
            args.before_context.map_or(0, |value| value as usize),
            args.after_context.map_or(0, |value| value as usize),
        ),
    };
    if args.context.is_some() && (args.before_context.is_some() || args.after_context.is_some()) {
        return Err(CliError::usage(
            "--context cannot be combined with --before-context or --after-context",
        ));
    }
    let root = std::env::current_dir().map_err(|error| CliError::io(Path::new("."), error))?;
    let options = LexicalSearchOptions {
        root: root.clone(),
        patterns,
        pattern_files: args.pattern_files.clone(),
        paths: args.rg_paths.clone(),
        limit: args.limit,
        include_paths: Vec::new(),
        exclude_paths: Vec::new(),
        globs: args.globs.clone(),
        insensitive_globs: args.iglobs.clone(),
        file_types: args.file_types.clone(),
        excluded_file_types: args.excluded_file_types.clone(),
        hidden: args.hidden,
        no_ignore: args.no_ignore,
        ignore_files: args.ignore_files.iter().map(PathBuf::from).collect(),
        max_depth: args
            .max_depth
            .map(usize::try_from)
            .transpose()
            .map_err(|_| CliError::usage("--max-depth is too large"))?,
        max_file_size_bytes: args
            .max_filesize
            .as_deref()
            .map(parse_byte_size)
            .transpose()?,
        follow: args.follow,
        modified_after: args
            .modified_after
            .as_deref()
            .map(|value| parse_modified_time(value, "--modified-after"))
            .transpose()?,
        modified_before: args
            .modified_before
            .as_deref()
            .map(|value| parse_modified_time(value, "--modified-before"))
            .transpose()?,
        fixed_strings: args.fixed_strings,
        ignore_case: args.ignore_case && !args.case_sensitive,
        smart_case: args.smart_case,
        word_regexp: args.word_regexp,
        before_context: before,
        after_context: after,
        max_count: args.max_count,
    };
    let searched = ZvecGrepService::new(service_options(
        None,
        None,
        args.api_key.clone(),
        None,
        args.model_cache.clone(),
        args.device,
    ))
    .rg_search(&options)?;
    let query = options.patterns.join(" | ");
    let result = ZvecGrepContextResult {
        query,
        root: root.to_string_lossy().into_owned(),
        source: ContextSource::Rg,
        coverage: if searched.diagnostics.truncated {
            zg_core::service::types::ContextCoverage::RgTruncated
        } else {
            zg_core::service::types::ContextCoverage::RgExhaustive
        },
        workspace_index: None,
        items: searched.items,
        group_results: None,
        diagnostics: ContextDiagnostics {
            rg: serde_json::to_value(&searched.diagnostics).ok(),
            ..ContextDiagnostics::default()
        },
    };
    print_context_result(&result, args.human, color);
    if let Some(missing) = searched.diagnostics.missing_paths {
        for path in missing {
            eprintln!("warning: path not found: {path}");
        }
    }
    Ok(())
}

// Note: `--rg` is direct-only like the TypeScript CLI (`runDirectRgQuery`
// ignores the transport mode), so no `rg` command string is ever built
// here. Managed `rg` over the daemon is the `zvec_grep_rg` MCP tool.

// ---------------------------------------------------------------------------
// index
// ---------------------------------------------------------------------------

async fn run_index(args: IndexArgs) -> Result<(), CliError> {
    if args.roots.len() > 1 {
        return Err(CliError::usage("zg index accepts at most one root path"));
    }
    let root = args
        .roots
        .first()
        .cloned()
        .unwrap_or_else(|| PathBuf::from("."));
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
        async {
            match DaemonClient::from_env(None, args.home.as_deref()) {
                Ok(client) => client.server_available().await,
                Err(_) => false,
            }
        },
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
        vec![zg_core::service::types::RootPathSpec::Full(Box::new(
            root_path,
        ))]
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
        async {
            match DaemonClient::from_env(None, args.home.as_deref()) {
                Ok(client) => client.server_available().await,
                Err(_) => false,
            }
        },
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

// ---------------------------------------------------------------------------
// status
// ---------------------------------------------------------------------------

async fn run_status(args: StatusArgs) -> Result<(), CliError> {
    if args.roots.len() > 1 {
        return Err(CliError::usage("zg status accepts at most one root path"));
    }
    let root = args
        .roots
        .first()
        .cloned()
        .unwrap_or_else(|| PathBuf::from("."));
    let absolute = absolute_path(&root)?;
    let mode = resolve_client_mode(args.mode)?;
    let state = route_by_mode(
        mode,
        run_status_direct(&args, &absolute),
        async {
            let client = DaemonClient::from_env(None, args.home.as_deref())?;
            run_status_server(&client, &absolute).await
        },
        async {
            match DaemonClient::from_env(None, args.home.as_deref()) {
                Ok(client) => client.server_available().await,
                Err(_) => false,
            }
        },
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
    let state = print_workspace_info(&info, color);
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

// ---------------------------------------------------------------------------
// config
// ---------------------------------------------------------------------------

fn run_config(args: crate::cli::ConfigArgs) -> Result<(), CliError> {
    match &args.target {
        Some(ConfigTarget::Model(cmd)) => match &cmd.op {
            Some(ConfigModelOp::Set(set)) => run_config_model_set(set),
            None => Err(CliError::usage(
                "zg config requires provider set or model set",
            )),
        },
        Some(ConfigTarget::Provider(cmd)) => match &cmd.op {
            Some(ConfigProviderOp::Set(set)) => run_config_provider_set(set),
            None => Err(CliError::usage(
                "zg config requires provider set or model set",
            )),
        },
        None => Err(CliError::usage(
            "zg config requires provider set or model set",
        )),
    }
}

fn run_config_model_set(set: &crate::cli::ConfigModelSetArgs) -> Result<(), CliError> {
    if set.reference.len() != 1 {
        return Err(CliError::usage(
            "zg config model set requires exactly one reference",
        ));
    }
    let reference = set.reference[0].clone();
    if set.endpoint.is_none() && set.device.is_none() && !set.default {
        return Err(CliError::usage(
            "zg config model set requires --endpoint, --device, or --default",
        ));
    }
    let entry = catalog_reference(&reference)?;
    let catalog = catalog_entry(entry.as_str()).ok_or_else(|| {
        CliError::config_invalid(format!("Unknown embedding model \"{reference}\""))
    })?;
    let (_, provider, _, _) = catalog_identity(catalog);
    if provider == "local" && set.endpoint.is_some() {
        return Err(CliError::usage(
            "--endpoint is only supported for remote embedding models",
        ));
    }
    if provider != "local" && set.device.is_some() {
        return Err(CliError::usage(
            "--device is only supported for local embedding models",
        ));
    }
    if let Some(endpoint) = &set.endpoint {
        assert_http_endpoint(endpoint)?;
    }
    let path = zg_core::config::global_config_path();
    let models = (set.endpoint.is_some() || set.device.is_some()).then(|| {
        BTreeMap::from([(
            reference.clone(),
            EmbeddingModelConfig {
                endpoint: set.endpoint.clone(),
                device: set.device.map(map_device),
            },
        )])
    });
    let defaults = set.default.then(|| GlobalDefaults {
        embedding: Some(reference.clone()),
        model_cache_dir: None,
    });
    zg_core::config::update_global_config(
        &path,
        GlobalConfigUpdate {
            defaults,
            providers: None,
            models,
            client: None,
            server: None,
        },
    )?;
    println!("Model config: {reference}");
    println!("Global config: {}", path.display());
    Ok(())
}

fn run_config_provider_set(set: &crate::cli::ConfigProviderSetArgs) -> Result<(), CliError> {
    if set.reference.len() != 1 {
        return Err(CliError::usage(
            "zg config provider set requires exactly one reference",
        ));
    }
    let reference = set.reference[0].clone();
    if !reference.chars().all(|marker| {
        marker.is_ascii_lowercase() || marker.is_ascii_digit() || marker == '_' || marker == '-'
    }) || !reference
        .chars()
        .next()
        .is_some_and(|marker| marker.is_ascii_lowercase())
    {
        return Err(CliError::config_invalid(format!(
            "Invalid embedding provider \"{reference}\""
        )));
    }
    if reference == "local"
        || !list_embedding_models()
            .iter()
            .any(|entry| catalog_identity(entry).1 == reference)
    {
        return Err(CliError::config_invalid(format!(
            "Unsupported remote embedding provider: {reference}"
        )));
    }
    let Some(api_key) = set.api_key.clone() else {
        return Err(CliError::usage("zg config provider set requires --api-key"));
    };
    let path = zg_core::config::global_config_path();
    zg_core::config::update_global_config(
        &path,
        GlobalConfigUpdate {
            defaults: None,
            providers: Some(BTreeMap::from([(
                reference.clone(),
                ProviderConfig {
                    api_key: Some(api_key),
                },
            )])),
            models: None,
            client: None,
            server: None,
        },
    )?;
    println!("Provider config: {reference}");
    println!("Global config: {}", path.display());
    Ok(())
}

fn assert_http_endpoint(endpoint: &str) -> Result<(), CliError> {
    let valid = endpoint.starts_with("http://") || endpoint.starts_with("https://");
    if valid && endpoint.len() > "https://".len() {
        return Ok(());
    }
    Err(CliError::usage("--endpoint must be a valid HTTP(S) URL"))
}

// ---------------------------------------------------------------------------
// auth
// ---------------------------------------------------------------------------

fn run_auth(args: crate::cli::AuthArgs) -> Result<(), CliError> {
    match &args.action {
        Some(AuthAction::Grant(grant)) => run_auth_grant(&args, grant),
        Some(AuthAction::Status(status)) => run_auth_status(&args, status),
        Some(AuthAction::Revoke(revoke)) => run_auth_revoke(&args, revoke),
        None => Err(CliError::usage("zg auth requires grant, status, or revoke")),
    }
}

fn auth_root(requested: &[PathBuf]) -> Result<String, CliError> {
    if requested.len() > 1 {
        return Err(CliError::usage("zg auth grant accepts at most one root"));
    }
    let start = requested
        .first()
        .cloned()
        .unwrap_or_else(|| PathBuf::from("."));
    Ok(absolute_path(&start)?.to_string_lossy().into_owned())
}

fn run_auth_status(
    _args: &crate::cli::AuthArgs,
    status: &crate::cli::AuthRootArgs,
) -> Result<(), CliError> {
    let root = auth_root(&status.roots)?;
    let store = zg_core::authorization::RemoteEmbeddingAuthorizationStore::new();
    let status = store.status(&root)?;
    print_auth_status(&root, &status);
    Ok(())
}

fn run_auth_revoke(
    args: &crate::cli::AuthArgs,
    revoke: &crate::cli::AuthRootArgs,
) -> Result<(), CliError> {
    let _ = args;
    let root = auth_root(&revoke.roots)?;
    let store = zg_core::authorization::RemoteEmbeddingAuthorizationStore::new();
    let revoked = store.revoke_all(&root)?;
    if revoked > 0 {
        println!("Revoked {revoked} Remote Embedding Workspace grant(s).");
    } else {
        println!("No Remote Embedding Workspace grants found.");
    }
    Ok(())
}

fn run_auth_grant(
    args: &crate::cli::AuthArgs,
    grant: &crate::cli::AuthGrantArgs,
) -> Result<(), CliError> {
    let root = auth_root(&grant.roots)?;
    let service = create_zvec_grep(service_options(
        Some(PathBuf::from(&root)),
        args.embedding.clone(),
        args.api_key.clone(),
        args.endpoint.clone(),
        args.model_cache.clone(),
        args.device,
    ));
    let info = service.workspace_info(Some(Path::new(&root)))?;
    let configured = args.embedding.clone().or_else(|| {
        info.workspace_index
            .as_ref()
            .and_then(|index| index.embedding.clone())
            .flatten()
            .and_then(|schema| catalog_reference_for(&schema.provider, &schema.model))
            .map(|reference| reference.as_str().to_owned())
    });
    let Some(configured) = configured else {
        return Err(CliError::usage(
            "No embedding model is available. Pass --embedding <remote/model> or build an index first.",
        ));
    };
    let reference = catalog_reference(&configured)?;
    let entry = catalog_entry(reference.as_str()).ok_or_else(|| {
        CliError::config_invalid(format!("Unknown embedding model \"{configured}\""))
    })?;
    let (_, provider, model, _) = catalog_identity(entry);
    if provider == "local" {
        return Err(CliError::usage(
            "Local embedding models do not require authorization.",
        ));
    }
    if provider != "qwen" {
        return Err(CliError::usage(format!(
            "Unsupported remote embedding provider: {provider}"
        )));
    }
    let model_info = create_embedding_model(
        &reference,
        &CreateEmbeddingModelOptions {
            api_key: args.api_key.clone(),
            endpoint: args.endpoint.clone(),
            model_cache_dir: args.model_cache.clone(),
            device: DeviceKind::Auto,
        },
    )?;
    let endpoint = model_info.info().endpoint.clone().ok_or_else(|| {
        CliError::usage(format!(
            "Embedding model {} did not provide a remote endpoint.",
            model_info.info().reference
        ))
    })?;
    let roots: Vec<String> = info
        .workspace_index
        .as_ref()
        .map(|index| {
            index
                .root_paths
                .iter()
                .map(|path| path.absolute_path.clone())
                .collect()
        })
        .unwrap_or_else(|| vec![root.clone()]);
    let target = create_remote_embedding_target(&roots, provider, model, &endpoint)?;
    let manager = RemoteEmbeddingAuthorizationManager::new();
    manager.grant(&target, RemoteEmbeddingScope::Workspace)?;
    let store = zg_core::authorization::RemoteEmbeddingAuthorizationStore::new();
    print_auth_status(&root, &store.status(&root)?);
    Ok(())
}

fn print_auth_status(root: &str, status: &zg_core::authorization::AuthorizationStatus) {
    println!("root: {root}");
    println!("grants: {}", status.grants.len());
    for grant in &status.grants {
        println!(
            "  {} {}/{} scope={:?} valid={}",
            grant.id, grant.provider, grant.model, grant.scope, grant.valid
        );
    }
}

// ---------------------------------------------------------------------------
// server
// ---------------------------------------------------------------------------

async fn run_server(args: crate::cli::ServerArgs) -> Result<(), CliError> {
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

fn daemon_home(args: &crate::cli::ServerArgs) -> Option<PathBuf> {
    args.home.clone()
}

async fn run_server_on(args: crate::cli::ServerArgs) -> Result<(), CliError> {
    let home = daemon_home(&args);
    let status = zg_server::server_controller::server_status(home.as_deref()).await;
    if status.ready {
        print_control_status(true, true, status.pid, status.server_url.as_deref());
        return Ok(());
    }
    let program =
        std::env::current_exe().map_err(|error| CliError::io(Path::new("<exe>"), error))?;
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
                crate::cli::McpToolsetArg::Agent => "agent",
                crate::cli::McpToolsetArg::Full => "full",
            }
            .to_owned(),
        );
    }
    if let Some(home) = &args.home {
        spawn.push("--home".to_owned());
        spawn.push(home.to_string_lossy().into_owned());
    }
    let status = zg_server::server_controller::start_server(
        &program.to_string_lossy(),
        &spawn,
        home.as_deref(),
        std::time::Duration::from_secs(30),
    )
    .await?;
    print_control_status(
        status.running,
        status.ready,
        status.pid,
        status.server_url.as_deref(),
    );
    Ok(())
}

async fn run_server_off(args: crate::cli::ServerArgs) -> Result<(), CliError> {
    let home = daemon_home(&args);
    let status = zg_server::server_controller::stop_server(
        home.as_deref(),
        std::time::Duration::from_secs(30),
        args.token_file.clone(),
    )
    .await?;
    print_control_status(
        status.running,
        status.ready,
        status.pid,
        status.server_url.as_deref(),
    );
    Ok(())
}

async fn run_server_status(
    args: &crate::cli::ServerArgs,
    status_args: &crate::cli::ServerStatusArgs,
) -> Result<(), CliError> {
    let home = daemon_home(args);
    let status = zg_server::server_controller::server_status(home.as_deref()).await;
    print_control_status(
        status.running,
        status.ready,
        status.pid,
        status.server_url.as_deref(),
    );
    if status_args.check_ready && !status.ready {
        return Err(CliError::NotReady {
            message: "zvec-grep server is not ready".to_owned(),
        });
    }
    Ok(())
}

async fn run_server_run(args: crate::cli::ServerArgs) -> Result<(), CliError> {
    let listen = zg_server::config::configured_listen_address(args.listen.as_deref())?;
    let token = zg_server::config::resolve_server_token(None, args.token_file.clone())?;
    let toolset = zg_server::mcp::toolset::McpToolset::resolve(
        args.mcp_toolset.map(|toolset| match toolset {
            crate::cli::McpToolsetArg::Agent => "agent",
            crate::cli::McpToolsetArg::Full => "full",
        }),
        std::env::var(zg_server::mcp::toolset::MCP_TOOLSET_ENV)
            .ok()
            .as_deref(),
    )
    .map_err(|error| CliError::usage(error.to_string()))?;
    let home = daemon_home(&args);
    let server_url = format!("http://{}", listen.display());
    let lock =
        zg_server::server_controller::DaemonInstanceLock::acquire(home.as_deref(), &server_url)
            .await?;
    let backend = DaemonBackend::new(DaemonBackendOptions {
        service: ServiceConfig {
            embedding: args.embedding.map(ModelReference::new),
            api_key: args.api_key.clone(),
            endpoint: args.endpoint.clone(),
            model_cache_dir: args.model_cache.clone(),
            model_override: None,
        },
        ..DaemonBackendOptions::default()
    });
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
    tokio::signal::ctrl_c().await.map_err(|error| {
        CliError::daemon_unavailable(format!("failed to wait for shutdown signal: {error}"))
    })?;
    eprintln!("shutting down");
    server.close().await;
    backend.close().await;
    lock.release().await;
    Ok(())
}

async fn run_server_stdio(args: crate::cli::ServerArgs) -> Result<(), CliError> {
    let toolset = zg_server::mcp::toolset::McpToolset::resolve(
        args.mcp_toolset.map(|toolset| match toolset {
            crate::cli::McpToolsetArg::Agent => "agent",
            crate::cli::McpToolsetArg::Full => "full",
        }),
        std::env::var(zg_server::mcp::toolset::MCP_TOOLSET_ENV)
            .ok()
            .as_deref(),
    )
    .map_err(|error| CliError::usage(error.to_string()))?;
    let backend = DaemonBackend::new(DaemonBackendOptions {
        service: ServiceConfig {
            embedding: args.embedding.map(ModelReference::new),
            api_key: args.api_key.clone(),
            endpoint: args.endpoint.clone(),
            model_cache_dir: args.model_cache.clone(),
            model_override: None,
        },
        ..DaemonBackendOptions::default()
    });
    zg_server::mcp::stdio_bridge::run_stdio_server(
        backend,
        env!("CARGO_PKG_VERSION").to_owned(),
        toolset,
    )
    .await
    .map_err(|error| CliError::daemon_unavailable(error.to_string()))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// install
// ---------------------------------------------------------------------------

fn run_install(args: crate::cli::InstallArgs) -> Result<(), CliError> {
    let home = user_home()?;
    let tokens = split_targets(&args.target);
    let targets = if tokens.is_empty() {
        let detected = detect_targets(&home);
        if detected.is_empty() {
            return Err(CliError::usage(
                "No agent integrations detected. Pass --target claude|codex|opencode|cursor|qwen|qoder.",
            ));
        }
        detected
    } else {
        tokens
            .iter()
            .map(|token| InstallTarget::parse(token))
            .collect::<Result<Vec<_>, _>>()?
    };
    let token_env = args
        .mcp_token_env
        .clone()
        .map(|value| parse_environment_variable(&value, "--mcp-token-env"))
        .transpose()?;
    let options = InstallOptions::new(
        args.mcp_transport,
        args.mcp_toolset,
        args.mcp_tool_timeout,
        token_env,
        args.yes || args.force,
    );
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

fn run_uninstall(args: crate::cli::UninstallArgs) -> Result<(), CliError> {
    let home = user_home()?;
    let tokens = split_targets(&args.target);
    let targets = if tokens.is_empty() {
        let detected = detect_targets(&home);
        if detected.is_empty() {
            return Err(CliError::usage(
                "No agent integrations detected. Pass --target claude|codex|opencode|cursor|qwen|qoder.",
            ));
        }
        detected
    } else {
        tokens
            .iter()
            .map(|token| InstallTarget::parse(token))
            .collect::<Result<Vec<_>, _>>()?
    };
    let _ = args.yes;
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

// ---------------------------------------------------------------------------
// shared helpers
// ---------------------------------------------------------------------------

/// Mirrors `createServiceOptions` for the direct engine path.
fn service_options(
    root: Option<PathBuf>,
    embedding: Option<String>,
    api_key: Option<String>,
    endpoint: Option<String>,
    model_cache: Option<PathBuf>,
    device: Option<DeviceArg>,
) -> CreateZvecGrepOptions {
    let _ = device;
    CreateZvecGrepOptions {
        root,
        embedding: embedding.map(ModelReference::new),
        embedding_model: None,
        api_key,
        endpoint,
        model_cache_dir: model_cache,
    }
}

/// Maps a CLI device flag onto the model options.
fn map_device(device: DeviceArg) -> EmbeddingDevice {
    match device {
        DeviceArg::Auto => EmbeddingDevice::Auto,
        DeviceArg::Cpu => EmbeddingDevice::Cpu,
        DeviceArg::Metal => EmbeddingDevice::Metal,
        DeviceArg::Vulkan => EmbeddingDevice::Vulkan,
        DeviceArg::Cuda => EmbeddingDevice::Cuda,
    }
}

/// Maps CLI symbol types onto indexed symbol types.
fn map_symbol_types(types: &[SymbolType]) -> Vec<CodeSymbolType> {
    types
        .iter()
        .map(|symbol| match symbol {
            SymbolType::Module => CodeSymbolType::Module,
            SymbolType::Class => CodeSymbolType::Class,
            SymbolType::Interface => CodeSymbolType::Interface,
            SymbolType::Function => CodeSymbolType::Function,
            SymbolType::Value => CodeSymbolType::Value,
            SymbolType::Alias => CodeSymbolType::Alias,
        })
        .collect()
}

fn map_modified_time(
    value: Option<&str>,
    option: &str,
) -> Result<Option<zg_core::types::UnixMillis>, CliError> {
    value
        .map(|value| {
            parse_modified_time(value, option).map(zg_core::types::UnixMillis::from_millis)
        })
        .transpose()
}

/// `false` flags become `None` (unset), mirroring optional TS booleans.
fn bool_flag(set: bool) -> Option<bool> {
    set.then_some(true)
}

/// Validates that a reference names a catalog entry, mirroring
/// `requireEmbeddingModelCatalogEntry`.
fn catalog_reference(reference: &str) -> Result<ModelReference, CliError> {
    if catalog_entry(reference).is_some() {
        return Ok(ModelReference::new(reference.to_owned()));
    }
    Err(CliError::config_invalid(format!(
        "Unknown embedding model \"{reference}\". Use zg help models for the catalog."
    )))
}

/// Finds a catalog entry by reference.
fn catalog_entry(reference: &str) -> Option<&'static EmbeddingCatalogEntry> {
    list_embedding_models()
        .iter()
        .find(|entry| catalog_identity(entry).0 == reference)
}

/// `(reference, provider, model, dimension)` for one catalog entry.
fn catalog_identity(
    entry: &EmbeddingCatalogEntry,
) -> (&'static str, &'static str, &'static str, usize) {
    match entry {
        EmbeddingCatalogEntry::LlamaCpp(model) => (
            model.reference,
            model.provider,
            model.model,
            model.dimension,
        ),
        EmbeddingCatalogEntry::QwenText(model) => (
            model.reference,
            model.provider,
            model.model,
            model.dimension,
        ),
        EmbeddingCatalogEntry::QwenMultimodal(model) => (
            model.reference,
            model.provider,
            model.model,
            model.dimension,
        ),
        EmbeddingCatalogEntry::TransformersJs(model) => (
            model.reference,
            model.provider,
            model.model,
            model.dimension,
        ),
        EmbeddingCatalogEntry::Model2Vec(model) => (
            model.reference,
            model.provider,
            model.model,
            model.dimension,
        ),
    }
}

/// Maps a recorded `(provider, model)` schema back to its catalog
/// reference.
fn catalog_reference_for(provider: &str, model: &str) -> Option<ModelReference> {
    list_embedding_models()
        .iter()
        .find(|entry| {
            let identity = catalog_identity(entry);
            identity.1 == provider && identity.2 == model
        })
        .map(|entry| ModelReference::new(catalog_identity(entry).0.to_owned()))
}

fn absolute_cwd() -> Result<String, CliError> {
    Ok(absolute_path(PathBuf::from("."))?
        .to_string_lossy()
        .into_owned())
}

fn absolute_path(path: impl AsRef<Path>) -> Result<PathBuf, CliError> {
    let path = path.as_ref();
    if path.as_os_str() == "." {
        return std::env::current_dir().map_err(|error| CliError::io(Path::new("."), error));
    }
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()
            .map_err(|error| CliError::io(Path::new("."), error))?
            .join(path)
    };
    Ok(absolute)
}

/// Cooperative cancellation: flips on Ctrl-C, polled by blocking runs.
struct CancelLatch {
    flag: Arc<AtomicBool>,
}

impl CancelLatch {
    fn check(&self) -> zg_core::service::types::AbortCheck {
        let flag = Arc::clone(&self.flag);
        Arc::new(move || flag.load(Ordering::Relaxed))
    }
}

fn cancel_flag() -> CancelLatch {
    let latch = CancelLatch {
        flag: Arc::new(AtomicBool::new(false)),
    };
    let flag = Arc::clone(&latch.flag);
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            flag.store(true, Ordering::Relaxed);
        }
    });
    latch
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn direct_rg_finds_fixture_hits() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn alpha() {}\n").unwrap();
        std::fs::write(dir.path().join("b.txt"), "nothing here\n").unwrap();
        let root = dir.path().to_owned();
        let options = LexicalSearchOptions {
            root,
            patterns: vec!["alpha".to_owned()],
            ..LexicalSearchOptions::default()
        };
        let service = ZvecGrepService::new(service_options(None, None, None, None, None, None));
        let searched = service.rg_search(&options).unwrap();
        assert_eq!(searched.items.len(), 1);
        assert_eq!(searched.items[0].file.relative_path, "a.rs");
        assert!(!searched.diagnostics.truncated);
    }

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
