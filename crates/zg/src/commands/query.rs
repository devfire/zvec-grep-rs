//! `zg query`: hybrid search in direct mode or via the daemon.

use serde_json::json;

use zg_core::authorization::{PlanSearchInput, plan_remote_search_authorization};
use zg_core::models::embeddings::{CreateEmbeddingModelOptions, DeviceKind};
use zg_core::models::factory::create_embedding_model;
use zg_core::service::facade::create_zvec_grep;
use zg_core::service::types::{ContextSource, ZvecGrepContextOptions};
use zg_core::types::{SearchPlanRoute, SearchPlanRouteMode};

use super::authz::{SearchPermit, authorize_plan};
use super::catalog::catalog_reference_for;
use super::support::{
    absolute_cwd, cancel_flag, map_modified_time, map_symbol_types, server_available,
    service_options,
};
use crate::cli::{QueryArgs, RefreshMode};
use crate::client::{
    DaemonClient, ServerSearchBody, parse_server_search_response, resolve_client_mode,
    resolve_direct_search_policy, resolve_server_search_policy, resolve_server_url, route_by_mode,
};
use crate::error::CliError;
use crate::format::{print_context_result, print_context_warnings, use_color};

pub(crate) async fn run_query(args: QueryArgs) -> Result<(), CliError> {
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
        return super::rg::run_rg_direct(args, queries).await;
    }
    let mode = resolve_client_mode(args.mode)?;
    route_by_mode(
        mode,
        run_query_direct(&args, &queries),
        async {
            let client = DaemonClient::from_env(None, args.home.as_deref())?;
            run_query_server(&args, &queries, client).await
        },
        server_available(args.home.as_deref()),
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
    let mut result =
        zg_core::authorization::with_remote_embedding_operation_permit(permit.permit, || {
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
        return Ok(SearchPermit::none());
    };
    // Local indexes need no remote permit; anything else remote must be
    // the supported provider.
    match schema.provider.as_str() {
        "local" => return Ok(SearchPermit::none()),
        "qwen" => {}
        provider => {
            return Err(CliError::usage(format!(
                "Unsupported remote embedding provider: {provider}"
            )));
        }
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
        return Ok(SearchPermit::none());
    };
    authorize_plan(&plan, args.allow_remote.is_some(), true).await
}

async fn run_query_server(
    args: &QueryArgs,
    queries: &[String],
    client: DaemonClient,
) -> Result<(), CliError> {
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
    Ok(())
}
