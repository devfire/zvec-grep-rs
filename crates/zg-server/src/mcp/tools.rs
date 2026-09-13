//! MCP tool router: the six `zvec_grep_*` tools over rmcp 0.6.
//!
//! Mirrors `../zvec-grep/src/mcp/tools.ts` (`createZvecGrepMcpServer`,
//! `registerZvecGrepTools`, tool descriptions/annotations, text layouts).
//! Every handler delegates to [`DaemonBackend`] commands and reimplements
//! no search/index logic. Tool instructions are byte-identical to the TS
//! rule lists. Remote-embedding authorization fails closed: without an
//! existing grant the backend errors and the handler surfaces the
//! `zg auth grant` hint instead of eliciting (see
//! `docs/ts-divergence.md`).

use std::sync::Arc;
use std::time::Duration;

use futures::FutureExt;
use rmcp::handler::server::router::tool::ToolRoute;
use rmcp::handler::server::tool::ToolCallContext;
use rmcp::model::{
    CallToolResult, Implementation, InitializeResult, JsonObject, ProgressToken, ProtocolVersion,
    ServerCapabilities, ServerInfo, Tool, ToolAnnotations, ToolsCapability,
};
use rmcp::service::Peer;
use rmcp::{ErrorData, RoleServer, ServerHandler};
use serde::de::DeserializeOwned;

use crate::backend::SearchIndexing;
use crate::backend::{
    BackendError, DaemonBackend, DaemonSearchResult, IndexInput, ResultFreshness,
};
use crate::job_scheduler::IndexJobSnapshot;
use crate::mcp::error::McpError;
use crate::mcp::input_normalization::{normalize_search_input, rg_query_from_input};
use crate::mcp::progress_heartbeat::{ProgressHeartbeat, progress_token_from_meta};
use crate::mcp::result_format::{format_agent_context_result, text_tool_result, tool_result};
use crate::mcp::schemas::{
    IndexActionOutput, IndexDropInput, IndexDropOutput, IndexInput as WireIndexInput, IndexOutput,
    IndexStatusInput, IndexStatusOutput, JobErrorOutput, JobStateOutput, RgInput,
    ScanDiagnosticsOutput, SearchInput, ServerModelsOutput, ServerStatusInput, ServerStatusOutput,
    format_index_status, input_schema_for, output_schema_for, parse_root,
};
use crate::mcp::toolset::McpToolset;

/// Server instructions for the `agent` toolset, byte-identical to TS
/// `ZVEC_GREP_AGENT_MCP_INSTRUCTIONS`.
pub const AGENT_MCP_INSTRUCTIONS: &str = "Use zvec-grep with these workspace retrieval rules:\n- Use the current workspace as the evidence source when the user asks about local material, prior context establishes it as relevant, or the question concerns how the current project works—even if the workspace is not mentioned explicitly.\n- A workspace may contain any mix of code, documents, configuration, and data.\n- Do not use workspace retrieval for unrelated open-world questions, current external facts, or web content that does not depend on local evidence.\n- Use native Grep or rg first only when exact lookup alone is sufficient, such as locating one definition, literal, filename, configuration key, error message, regex match, or exhaustive occurrence list.\n- Use zvec_grep_search first when wording or location is unknown, or when the answer requires architecture, lifecycle, call relationships, dependencies, data or control flow, design rationale, comparison, or synthesis across files or components.\n- When user-provided or verified exact symbols are present but the answer spans multiple files, components, stages, implementations, or relationships, treat the task as mixed: call zvec_grep_search with the semantic intent and those anchors, then use Read, Grep, or rg for focused verification.\n- For a semantic or mixed workspace task, start discovery with focused zvec_grep_search before broad file discovery.\n- Preserve the question's concepts, relationships, and constraints from the user request and established context in semantic queries. Treat inferred names as supplemental hypotheses, not replacements for or constraints on the stated intent.\n- `query` creates one primary hybrid FTS-plus-vector group; `queries` creates one or more primary hybrid groups; `fts` and `vector` add supplemental lexical-only or semantic-only route groups. These are retrieval routes, not hard constraints. Without `fuse`, the response is one deduplicated and reranked list with query-group metadata; set `fuse: true` to collapse every group into one ranked search plan.\n- For a fused mixed search, use arguments such as {\"root\":\"/absolute/workspace\",\"query\":\"how are results ranked and fused\",\"fts\":[\"RRF\",\"score\"],\"fuse\":true}.\n- Search results include bounded source snippets. Treat a sufficient snippet as already-read evidence, and open only the cited file or range when a required detail falls outside it.\n- If semantic retrieval remains irrelevant, fall back to native Grep or rg.\n- Stop searching once the available evidence is sufficient for the requested task. Continue only to resolve a material gap or ambiguity; do not repeat similar searches or broaden the investigation merely to reconfirm what is already established.\n- Do not launch a sub-agent solely to locate workspace material.\n- Every workspace operation requires an absolute root path visible to the daemon.\n- Read freshness and background_refresh directly from zvec_grep_search responses without a status preflight.\n- When results are served_from_current_index, use them immediately when they are sufficient; do not perform extra diagnostics merely because a background refresh is active.\n- When an index is missing and literal or regex search can answer the task, use native Grep or rg. Creating or rebuilding a persistent index requires explicit user authorization.";

/// Server instructions for the `full` toolset, byte-identical to TS
/// `ZVEC_GREP_FULL_MCP_INSTRUCTIONS`.
pub const FULL_MCP_INSTRUCTIONS: &str = "Use zvec-grep with these workspace retrieval and lifecycle rules:\n- Use the current workspace as the evidence source when the user asks about local material, prior context establishes it as relevant, or the question concerns how the current project works—even if the workspace is not mentioned explicitly.\n- A workspace may contain any mix of code, documents, configuration, and data.\n- Do not use workspace retrieval for unrelated open-world questions, current external facts, or web content that does not depend on local evidence.\n- Use zvec_grep_rg first only when exact lookup alone is sufficient, such as locating one definition, literal, filename, configuration key, error message, regex match, or exhaustive occurrence list.\n- Use zvec_grep_search first when wording or location is unknown, or when the answer requires architecture, lifecycle, call relationships, dependencies, data or control flow, design rationale, comparison, or synthesis across files or components.\n- When user-provided or verified exact symbols are present but the answer spans multiple files, components, stages, implementations, or relationships, treat the task as mixed: call zvec_grep_search with the semantic intent and those anchors, then use Read or zvec_grep_rg for focused verification.\n- For a semantic or mixed workspace task, start discovery with focused zvec_grep_search before broad file discovery.\n- Preserve the question's concepts, relationships, and constraints from the user request and established context in semantic queries. Treat inferred names as supplemental hypotheses, not replacements for or constraints on the stated intent.\n- `query` creates one primary hybrid FTS-plus-vector group; `queries` creates one or more primary hybrid groups; `fts` and `vector` add supplemental lexical-only or semantic-only route groups. These are retrieval routes, not hard constraints. Without `fuse`, the response is one deduplicated and reranked list with query-group metadata; set `fuse: true` to collapse every group into one ranked search plan.\n- For a fused mixed search, use arguments such as {\"root\":\"/absolute/workspace\",\"query\":\"how are results ranked and fused\",\"fts\":[\"RRF\",\"score\"],\"fuse\":true}.\n- Search results include bounded source snippets. Treat a sufficient snippet as already-read evidence, and open only the cited file or range when a required detail falls outside it.\n- If semantic retrieval remains irrelevant, fall back to zvec_grep_rg.\n- Stop searching once the available evidence is sufficient for the requested task. Continue only to resolve a material gap or ambiguity; do not repeat similar searches or broaden the investigation merely to reconfirm what is already established.\n- Do not launch a sub-agent solely to locate workspace material.\n- Every workspace operation requires an absolute root path visible to the daemon.\n- Use the zvec_grep_* tools directly for workspace search, status, indexing, deletion, and exhaustive lexical search.\n- Use freshness and background_refresh from zvec_grep_search without a status preflight; call zvec_grep_index_status only for a missing index, failed or cancelled indexing, diagnostics, or explicit progress monitoring.\n- When results are served_from_current_index, use them immediately when they are sufficient; do not call status merely because a background refresh is active.\n- Call zvec_grep_index only when persistent indexing or index deletion is explicitly requested. Never silently create, rebuild, or drop an index.\n- For a new index, use a user-selected embedding or omit it only when a server default model is known; never guess a model.\n- zvec_grep_index wait defaults to false; poll zvec_grep_index_status for background progress and set wait to true only when completion is required before continuing.\n- Use zvec_grep_index with drop: true, or zvec_grep_index_drop, only when index deletion is explicitly requested.\n- Call zvec_grep_server_status only for daemon diagnostics, not before ordinary searches.";

/// Search tool description for the `agent` toolset.
const AGENT_SEARCH_DESCRIPTION: &str = "Search an existing workspace index for semantic, relational, cross-file, or multi-hop evidence such as architecture, call chains, dependencies, lifecycle, data or control flow, design rationale, and comparisons. Use it when exact lookup alone cannot answer a workspace-grounded question. Results include bounded source snippets and query-group metadata; treat sufficient snippets as already-read evidence. Use native Grep or rg instead when exact lookup alone is sufficient. Read freshness and background_refresh from the response without a status preflight; when results are served_from_current_index, use them if sufficient.";

/// Search tool description for the `full` toolset.
const FULL_SEARCH_DESCRIPTION: &str = "Search an existing workspace index for semantic, relational, cross-file, or multi-hop evidence such as architecture, call chains, dependencies, lifecycle, data or control flow, design rationale, and comparisons. Use it when exact lookup alone cannot answer a workspace-grounded question. Results include bounded source snippets and query-group metadata; treat sufficient snippets as already-read evidence. Use zvec_grep_rg instead when exact lookup alone is sufficient. Read freshness and background_refresh from the response; when results are served_from_current_index, use them if sufficient.";

/// MCP server over [`DaemonBackend`]: tool implementations plus server
/// identity. `Clone` so rmcp route closures can own a handle.
#[derive(Clone)]
pub struct ZvecGrepMcpServer {
    backend: DaemonBackend,
    version: String,
    toolset: McpToolset,
}

impl ZvecGrepMcpServer {
    /// Builds the server around a backend.
    #[must_use]
    pub fn new(backend: DaemonBackend, version: String, toolset: McpToolset) -> Self {
        Self {
            backend,
            version,
            toolset,
        }
    }

    /// Underlying backend (test inspection, transport close).
    #[must_use]
    pub fn backend(&self) -> &DaemonBackend {
        &self.backend
    }

    /// Active toolset.
    #[must_use]
    pub fn toolset(&self) -> McpToolset {
        self.toolset
    }

    /// Builds the rmcp router with toolset-gated tools: `agent` exposes
    /// search only, `full` adds index lifecycle, rg, and status tools.
    #[must_use]
    pub fn router(self) -> rmcp::handler::server::router::Router<Self> {
        let full = self.toolset == McpToolset::Full;
        let router = rmcp::handler::server::router::Router::new(self);
        let router = if full {
            router
                .with_tool(index_route())
                .with_tool(index_drop_route())
                .with_tool(rg_route())
                .with_tool(index_status_route())
                .with_tool(server_status_route())
        } else {
            router
        };
        router.with_tool(search_route(full))
    }
}

impl ServerHandler for ZvecGrepMcpServer {
    fn get_info(&self) -> ServerInfo {
        InitializeResult {
            protocol_version: ProtocolVersion::default(),
            capabilities: ServerCapabilities {
                tools: Some(ToolsCapability { list_changed: None }),
                ..ServerCapabilities::default()
            },
            server_info: Implementation {
                name: "zvec-grep".to_owned(),
                title: None,
                version: self.version.clone(),
                icons: None,
                website_url: None,
            },
            instructions: Some(
                match self.toolset {
                    McpToolset::Agent => AGENT_MCP_INSTRUCTIONS,
                    McpToolset::Full => FULL_MCP_INSTRUCTIONS,
                }
                .to_owned(),
            ),
        }
    }
}

/// Tool-call envelope: owned arguments, peer, and progress token.
struct CallEnvelope {
    arguments: Option<JsonObject>,
    peer: Peer<RoleServer>,
    progress_token: Option<ProgressToken>,
}

fn envelope(context: &ToolCallContext<'_, ZvecGrepMcpServer>) -> CallEnvelope {
    CallEnvelope {
        arguments: context.arguments.clone(),
        peer: context.request_context.peer.clone(),
        progress_token: progress_token_from_meta(&context.request_context.meta),
    }
}

fn parse_arguments<T: DeserializeOwned>(arguments: &Option<JsonObject>) -> Result<T, McpError> {
    let value = serde_json::Value::Object(arguments.clone().unwrap_or_default());
    serde_json::from_value(value)
        .map_err(|error| McpError::invalid_params(format!("Invalid tool arguments: {error}")))
}

fn search_route(full: bool) -> ToolRoute<ZvecGrepMcpServer> {
    let mut tool = Tool::new(
        "zvec_grep_search",
        if full {
            FULL_SEARCH_DESCRIPTION
        } else {
            AGENT_SEARCH_DESCRIPTION
        },
        input_schema_for::<SearchInput>(),
    );
    tool.title = Some("Search with zvec-grep".to_owned());
    tool.annotations = Some(
        ToolAnnotations::new()
            .read_only(false)
            .destructive(false)
            .idempotent(false)
            .open_world(true),
    );
    ToolRoute::new_dyn(tool, |context: ToolCallContext<'_, ZvecGrepMcpServer>| {
        let server = context.service.clone();
        let envelope = envelope(&context);
        async move { server.handle_search(envelope).await }.boxed()
    })
}

fn index_route() -> ToolRoute<ZvecGrepMcpServer> {
    let mut tool = Tool::new(
        "zvec_grep_index",
        "Activate an absolute workspace root to create, incrementally update, rebuild, or explicitly drop its index. Do not call this tool to create, rebuild, or drop an index unless the user requested persistent indexing or index deletion.",
        input_schema_for::<WireIndexInput>(),
    );
    tool.title = Some("Ensure or drop zvec-grep index".to_owned());
    tool.output_schema = Some(output_schema_for::<IndexOutput>());
    tool.annotations = Some(
        ToolAnnotations::new()
            .read_only(false)
            .destructive(true)
            .idempotent(false)
            .open_world(true),
    );
    ToolRoute::new_dyn(tool, |context: ToolCallContext<'_, ZvecGrepMcpServer>| {
        let server = context.service.clone();
        let envelope = envelope(&context);
        async move { server.handle_index(envelope).await }.boxed()
    })
}

fn index_drop_route() -> ToolRoute<ZvecGrepMcpServer> {
    let mut tool = Tool::new(
        "zvec_grep_index_drop",
        "Delete the persisted index for an absolute workspace root and release its daemon runtime.",
        input_schema_for::<IndexDropInput>(),
    );
    tool.title = Some("Drop zvec-grep workspace index".to_owned());
    tool.output_schema = Some(output_schema_for::<IndexDropOutput>());
    tool.annotations = Some(
        ToolAnnotations::new()
            .read_only(false)
            .destructive(true)
            .idempotent(true)
            .open_world(false),
    );
    ToolRoute::new_dyn(tool, |context: ToolCallContext<'_, ZvecGrepMcpServer>| {
        let server = context.service.clone();
        let envelope = envelope(&context);
        async move { server.handle_index_drop(envelope).await }.boxed()
    })
}

fn rg_route() -> ToolRoute<ZvecGrepMcpServer> {
    let mut tool = Tool::new(
        "zvec_grep_rg",
        "Run exhaustive, AST-enriched ripgrep across code or non-code workspace material without an index. Use it when a known word, symbol, filename, source fragment, or regex can answer the workspace-grounded question. Pass a command starting with `rg`; results are exhaustive unless a trailing `| head -N` explicitly bounds them.",
        input_schema_for::<RgInput>(),
    );
    tool.title = Some("Search with managed ripgrep".to_owned());
    tool.annotations = Some(
        ToolAnnotations::new()
            .read_only(true)
            .destructive(false)
            .idempotent(true)
            .open_world(false),
    );
    ToolRoute::new_dyn(tool, |context: ToolCallContext<'_, ZvecGrepMcpServer>| {
        let server = context.service.clone();
        let envelope = envelope(&context);
        async move { server.handle_rg(envelope).await }.boxed()
    })
}

fn index_status_route() -> ToolRoute<ZvecGrepMcpServer> {
    let mut tool = Tool::new(
        "zvec_grep_index_status",
        "Read persisted index status and, when active, daemon runtime and job status for an absolute root. Use only after a missing-index response, indexing failure or cancellation, explicit progress monitoring, or daemon diagnostics.",
        input_schema_for::<IndexStatusInput>(),
    );
    tool.title = Some("Inspect zvec-grep index status".to_owned());
    tool.output_schema = Some(output_schema_for::<IndexStatusOutput>());
    tool.annotations = Some(
        ToolAnnotations::new()
            .read_only(true)
            .destructive(false)
            .idempotent(true)
            .open_world(false),
    );
    ToolRoute::new_dyn(tool, |context: ToolCallContext<'_, ZvecGrepMcpServer>| {
        let server = context.service.clone();
        let envelope = envelope(&context);
        async move { server.handle_index_status(envelope).await }.boxed()
    })
}

fn server_status_route() -> ToolRoute<ZvecGrepMcpServer> {
    let mut tool = Tool::new(
        "zvec_grep_server_status",
        "Read daemon version, queue, runtime and model-pool summary without exposing repository paths.",
        input_schema_for::<ServerStatusInput>(),
    );
    tool.title = Some("Inspect zvec-grep server status".to_owned());
    tool.output_schema = Some(output_schema_for::<ServerStatusOutput>());
    tool.annotations = Some(
        ToolAnnotations::new()
            .read_only(true)
            .destructive(false)
            .idempotent(true)
            .open_world(false),
    );
    ToolRoute::new_dyn(tool, |context: ToolCallContext<'_, ZvecGrepMcpServer>| {
        let server = context.service.clone();
        let envelope = envelope(&context);
        async move { server.handle_server_status(envelope).await }.boxed()
    })
}

impl ZvecGrepMcpServer {
    async fn handle_search(&self, envelope: CallEnvelope) -> Result<CallToolResult, ErrorData> {
        self.search_inner(&envelope)
            .await
            .map_err(map_backend_error)
    }

    async fn search_inner(&self, envelope: &CallEnvelope) -> Result<CallToolResult, McpError> {
        let input: SearchInput = parse_arguments(&envelope.arguments)?;
        let normalized = normalize_search_input(&input)?;
        let trace = normalized.trace;
        let root = normalized.root.as_str().to_owned();
        let searched = self
            .backend
            .search(&root, normalized.into_backend_query())
            .await?;
        Ok(text_tool_result(search_text(&searched, trace)))
    }

    async fn handle_index(&self, envelope: CallEnvelope) -> Result<CallToolResult, ErrorData> {
        self.index_inner(envelope)
            .await
            .map_err(McpError::into_error_data)
    }

    async fn index_inner(&self, envelope: CallEnvelope) -> Result<CallToolResult, McpError> {
        let input: WireIndexInput = parse_arguments(&envelope.arguments)?;
        reject_index_overrides(&input)?;
        let root = parse_root(&input.root)?;
        if input.drop.unwrap_or(false) {
            if input.rebuild.unwrap_or(false) {
                return Err(McpError::invalid_params(
                    "drop must not be combined with indexing options.",
                ));
            }
            let removed = self.backend.index_drop(root.as_str()).await?;
            let output = IndexOutput {
                root: root.as_str().to_owned(),
                job_id: "drop".to_owned(),
                state: JobStateOutput::Succeeded,
                reused: false,
                action: Some(IndexActionOutput::Drop),
                dropped: Some(removed),
                error: None,
                scan_diagnostics: None,
            };
            return Ok(tool_result(
                index_text(&output),
                serde_json::to_value(&output).unwrap_or(serde_json::Value::Null),
            ));
        }
        let submitted = self
            .backend
            .index(
                root.as_str(),
                IndexInput {
                    rebuild: input.rebuild.unwrap_or(false),
                    changed_paths: Vec::new(),
                },
            )
            .await?;
        let (state, error, scan_diagnostics) = if input.wait.unwrap_or(false) {
            self.wait_for_index(
                root.as_str(),
                &submitted.job,
                &envelope,
                input.debug.unwrap_or(false),
            )
            .await?
        } else {
            (
                JobStateOutput::from(submitted.job.state),
                submitted.job.error.as_ref().map(JobErrorOutput::from),
                None,
            )
        };
        let output = IndexOutput {
            root: root.as_str().to_owned(),
            job_id: submitted.job.id.to_string(),
            state,
            reused: submitted.reused,
            action: Some(IndexActionOutput::Index),
            dropped: None,
            error,
            scan_diagnostics,
        };
        Ok(tool_result(
            index_text(&output),
            serde_json::to_value(&output).unwrap_or(serde_json::Value::Null),
        ))
    }

    /// Waits for a submitted index job with progress beats, then reads the
    /// terminal state, error, and (with `debug`) scan diagnostics. The
    /// status poll bridges the actor-apply race: the scheduler marks the
    /// job terminal before the actor stores the finished payload.
    async fn wait_for_index(
        &self,
        root: &str,
        job: &IndexJobSnapshot,
        envelope: &CallEnvelope,
        debug: bool,
    ) -> Result<
        (
            JobStateOutput,
            Option<JobErrorOutput>,
            Option<ScanDiagnosticsOutput>,
        ),
        McpError,
    > {
        let heartbeat = match envelope.progress_token.clone() {
            Some(token) => ProgressHeartbeat::start(
                envelope.peer.clone(),
                token,
                "Indexing workspace".to_owned(),
            ),
            None => ProgressHeartbeat::noop(),
        };
        let peer = envelope.peer.clone();
        let token = envelope.progress_token.clone();
        let terminal = self
            .backend
            .scheduler()
            .wait(
                &job.id,
                Some(Arc::new(move |progress: zg_core::types::IndexProgress| {
                    if let (Some(token), Some(message)) =
                        (token.as_ref(), progress_message(&progress))
                    {
                        let peer = peer.clone();
                        let token = token.clone();
                        tokio::spawn(async move {
                            use rmcp::model::ProgressNotificationParam;
                            let _ = peer
                                .notify_progress(ProgressNotificationParam {
                                    progress_token: token,
                                    progress: message.0,
                                    total: message.1,
                                    message: Some(message.2),
                                })
                                .await;
                        });
                    }
                })),
            )
            .await
            .map_err(BackendError::from)
            .map_err(McpError::from)?;
        heartbeat.stop().await;
        // The scheduler marks the job terminal before the actor applies
        // the finished payload (scan diagnostics, cached status), so poll
        // until the actor-side state settles — not just the scheduler side.
        let mut status = self.backend.index_status(root).await?;
        for _ in 0..40 {
            let settled = status
                .job
                .as_ref()
                .is_some_and(|live| live.id == terminal.id)
                && status.job.as_ref().is_some_and(|live| live.is_terminal())
                && (!debug || status.scan_diagnostics.is_some());
            if settled {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
            status = self.backend.index_status(root).await?;
        }
        let error = terminal.error.as_ref().map(JobErrorOutput::from);
        let scan_diagnostics = if debug {
            status
                .scan_diagnostics
                .as_ref()
                .map(ScanDiagnosticsOutput::from)
        } else {
            None
        };
        Ok((
            JobStateOutput::from(terminal.state),
            error,
            scan_diagnostics,
        ))
    }

    async fn handle_index_drop(&self, envelope: CallEnvelope) -> Result<CallToolResult, ErrorData> {
        let input: IndexDropInput =
            parse_arguments(&envelope.arguments).map_err(McpError::into_error_data)?;
        let root = parse_root(&input.root).map_err(McpError::into_error_data)?;
        let removed = self
            .backend
            .index_drop(root.as_str())
            .await
            .map_err(McpError::from)
            .map_err(McpError::into_error_data)?;
        let output = IndexDropOutput {
            root: root.as_str().to_owned(),
            removed,
        };
        Ok(tool_result(
            if removed {
                format!("Dropped workspace index for {}", output.root)
            } else {
                format!("No workspace index found for {}", output.root)
            },
            serde_json::to_value(&output).unwrap_or(serde_json::Value::Null),
        ))
    }

    async fn handle_rg(&self, envelope: CallEnvelope) -> Result<CallToolResult, ErrorData> {
        self.rg_inner(&envelope)
            .await
            .map_err(McpError::into_error_data)
    }

    async fn rg_inner(&self, envelope: &CallEnvelope) -> Result<CallToolResult, McpError> {
        let input: RgInput = parse_arguments(&envelope.arguments)?;
        let (root, query) = rg_query_from_input(&input)?;
        let searched = self.backend.rg_search(root.as_str(), query).await?;
        let mut text = format_agent_context_result(&rg_context_result(&searched), false);
        if searched.diagnostics.truncated {
            text.push_str("\n\nMore matches were omitted by the explicit output bound. Remove or increase the trailing `head` bound to see them.");
        }
        Ok(text_tool_result(text))
    }

    async fn handle_index_status(
        &self,
        envelope: CallEnvelope,
    ) -> Result<CallToolResult, ErrorData> {
        let input: IndexStatusInput =
            parse_arguments(&envelope.arguments).map_err(McpError::into_error_data)?;
        let root = parse_root(&input.root).map_err(McpError::into_error_data)?;
        let status = self
            .backend
            .index_status(root.as_str())
            .await
            .map_err(McpError::from)
            .map_err(McpError::into_error_data)?;
        let output = format_index_status(root.as_str(), &status);
        Ok(tool_result(
            serde_json::to_string_pretty(&output).unwrap_or_else(|_| "{}".to_owned()),
            serde_json::to_value(&output).unwrap_or(serde_json::Value::Null),
        ))
    }

    async fn handle_server_status(
        &self,
        envelope: CallEnvelope,
    ) -> Result<CallToolResult, ErrorData> {
        let _input: ServerStatusInput =
            parse_arguments(&envelope.arguments).map_err(McpError::into_error_data)?;
        let status = self.backend.server_status();
        let output = ServerStatusOutput {
            version: self.version.clone(),
            uptime_ms: now_ms().saturating_sub(status.started_at_ms),
            shutting_down: false,
            active_runtimes: status.runtimes,
            queued_jobs: status.queued_jobs,
            running_jobs: status.running_jobs,
            models: ServerModelsOutput {
                loaded: status.pool_loaded,
                active_leases: status.pool_leases,
            },
        };
        Ok(tool_result(
            serde_json::to_string_pretty(&output).unwrap_or_else(|_| "{}".to_owned()),
            serde_json::to_value(&output).unwrap_or(serde_json::Value::Null),
        ))
    }
}

/// Rejects per-request credential/device/embedding overrides and
/// index-scoping fields the daemon cannot apply per request. The TS tool
/// accepts these; the port owns index configuration at the daemon layer,
/// so accepting-and-ignoring them would silently build the wrong index.
fn reject_index_overrides(input: &WireIndexInput) -> Result<(), McpError> {
    if input.api_key.is_some() {
        return Err(McpError::invalid_params(
            "Per-request apiKey overrides are not supported; configure the daemon instead.",
        ));
    }
    if input.device.is_some() {
        return Err(McpError::invalid_params(
            "Per-request device overrides are not supported; configure the daemon instead.",
        ));
    }
    if input.endpoint.is_some() {
        return Err(McpError::invalid_params(
            "Per-request endpoint overrides are not supported; configure the daemon instead.",
        ));
    }
    if input
        .embedding
        .as_ref()
        .is_some_and(|value| !value.trim().is_empty())
    {
        return Err(McpError::invalid_params(
            "Per-request embedding selection is not supported; configure the daemon instead.",
        ));
    }
    for (name, present) in [
        ("resetPaths", input.reset_paths.is_some()),
        ("globs", input.globs.is_some()),
        ("insensitiveGlobs", input.insensitive_globs.is_some()),
        ("fileTypes", input.file_types.is_some()),
        ("excludedFileTypes", input.excluded_file_types.is_some()),
        ("hidden", input.hidden.is_some()),
        ("noIgnore", input.no_ignore.is_some()),
        ("ignoreFiles", input.ignore_files.is_some()),
        ("maxDepth", input.max_depth.is_some()),
        ("maxFileSizeBytes", input.max_file_size_bytes.is_some()),
        ("follow", input.follow.is_some()),
        (
            "embeddingConcurrency",
            input.embedding_concurrency.is_some(),
        ),
    ] {
        if present {
            return Err(McpError::invalid_params(format!(
                "Index option \"{name}\" is daemon configuration in this port and cannot be set per request."
            )));
        }
    }
    Ok(())
}

/// Backend failures surface as internal errors, except the fail-closed
/// remote-embedding refusal, which directs the caller to `zg auth grant`.
fn map_backend_error(error: McpError) -> ErrorData {
    if let McpError::Backend(BackendError::Engine(ref engine)) = error
        && is_authorization_refusal(engine)
    {
        return McpError::AuthorizationRequired {
            message: format!(
                "{engine} Run `zg auth grant` to allow remote embeddings for the workspace."
            ),
        }
        .into_error_data();
    }
    error.into_error_data()
}

/// Detects the fail-closed remote-embedding refusal by its frozen code.
fn is_authorization_refusal(engine: &zg_core::error::EngineError) -> bool {
    engine
        .code()
        .qualified()
        .contains("AUTH.REMOTE_EMBEDDING_REQUIRED")
}

/// Search response text: freshness lines plus the agent rendering.
fn search_text(searched: &DaemonSearchResult, trace: bool) -> String {
    let mut lines = vec![format!("freshness: {}", freshness_name(searched.freshness))];
    if let Some(indexing) = &searched.indexing {
        lines.push("results: served_from_current_index".to_owned());
        lines.push(format!(
            "background_refresh: {}",
            format_search_indexing(indexing)
        ));
    }
    lines.push(format_agent_context_result(&searched.result, trace));
    lines.join("\n")
}

fn freshness_name(freshness: ResultFreshness) -> &'static str {
    match freshness {
        ResultFreshness::Fresh => "fresh",
        ResultFreshness::PossiblyStale => "possibly_stale",
    }
}

/// `running (3/12)` or bare state (mirrors `formatSearchIndexing`).
fn format_search_indexing(indexing: &SearchIndexing) -> String {
    let state = match indexing.state {
        crate::backend::BackgroundIndexState::Idle => "idle",
        crate::backend::BackgroundIndexState::Queued => "queued",
        crate::backend::BackgroundIndexState::Running => "running",
        crate::backend::BackgroundIndexState::Failed => "failed",
        crate::backend::BackgroundIndexState::Cancelled => "cancelled",
    };
    match (indexing.completed, indexing.total) {
        (Some(completed), Some(total)) => format!("{state} ({completed}/{total})"),
        _ => state.to_owned(),
    }
}

/// Index response text (mirrors the TS tool text layout).
fn index_text(output: &IndexOutput) -> String {
    let state = match output.state {
        JobStateOutput::Queued => "queued",
        JobStateOutput::Running => "running",
        JobStateOutput::Succeeded => "succeeded",
        JobStateOutput::Failed => "failed",
        JobStateOutput::Cancelled => "cancelled",
    };
    let mut lines = vec![
        format!("root: {}", output.root),
        format!("job_id: {}", output.job_id),
        format!("state: {state}"),
        format!("reused: {}", output.reused),
    ];
    if let Some(action) = output.action {
        lines.push(format!(
            "action: {}",
            match action {
                IndexActionOutput::Index => "index",
                IndexActionOutput::Drop => "drop",
            }
        ));
    }
    if let Some(dropped) = output.dropped {
        lines.push(format!("dropped: {dropped}"));
    }
    if let Some(error) = &output.error {
        lines.push(format!("error_code: {}", error.code));
        lines.push(format!("error_message: {}", error.message));
        if let Some(context) = &error.context {
            lines.push(format!("error_context: {context}"));
        }
        if let Some(cause) = &error.cause {
            lines.push(format!("error_cause: {cause}"));
        }
    }
    if let Some(diagnostics) = &output.scan_diagnostics {
        lines.push(format!("skipped_files: {}", diagnostics.skipped_files));
    }
    lines.join("\n")
}

/// Adapts a lexical result into a context result for the agent renderer.
fn rg_context_result(
    searched: &zg_core::lexical::LexicalSearchResult,
) -> zg_core::service::types::ZvecGrepContextResult {
    use zg_core::service::types::{
        ContextCoverage, ContextDiagnostics, ContextSource, ZvecGrepContextResult,
    };
    ZvecGrepContextResult {
        query: searched.diagnostics.patterns.join(" | "),
        root: String::new(),
        source: ContextSource::Rg,
        coverage: if searched.diagnostics.truncated {
            ContextCoverage::RgTruncated
        } else {
            ContextCoverage::RgExhaustive
        },
        workspace_index: None,
        items: searched.items.clone(),
        group_results: None,
        diagnostics: ContextDiagnostics {
            empty_reason: None,
            index: None,
            rg: None,
            structure: None,
            timings: None,
        },
    }
}

/// `(progress, total, message)` for index progress reports.
fn progress_message(
    progress: &zg_core::types::IndexProgress,
) -> Option<(f64, Option<f64>, String)> {
    let completed = progress.files_indexed.unwrap_or(0) as f64;
    let total = progress.files_total.map(|total| total as f64);
    let message = match (progress.files_indexed, progress.files_total) {
        (Some(done), Some(total)) => format!("Indexing workspace: {done}/{total} files"),
        (Some(done), None) => format!("Indexing workspace: {done} files"),
        (None, Some(total)) => format!("Indexing workspace: 0/{total} files"),
        (None, None) => "Indexing workspace".to_owned(),
    };
    Some((completed, total, message))
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}
