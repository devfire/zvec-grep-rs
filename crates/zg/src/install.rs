//! IDE MCP config installation (`cli/install.ts` behavior).
//!
//! Read-modify-write behind a `--force` gate: managed `zvec_grep`
//! entries and marked blocks merge freely; foreign entries are never
//! overwritten without explicit consent. JSON targets merge the
//! `zvec_grep` server object; Codex uses a marked TOML block. Guidance
//! markdown blocks (`AGENTS.md`) are out of scope by design (see
//! `docs/ts-divergence.md`).

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::cli::McpTransportArg;
use crate::client::resolve_server_url;
use crate::error::CliError;
use zg_server::mcp::toolset::McpToolset;

mod jsonc;
mod paths;

/// Managed TOML block markers for Codex, byte-identical to TS.
pub const CONFIG_START: &str = "# ZVEC_GREP_START";
/// Managed TOML block end marker, byte-identical to TS.
pub const CONFIG_END: &str = "# ZVEC_GREP_END";

/// Grok permission block markers, byte-identical to TS.
pub const GROK_PERMISSION_START: &str = "# ZVEC_GREP_PERMISSION_START";
/// Grok permission block end marker, byte-identical to TS.
pub const GROK_PERMISSION_END: &str = "# ZVEC_GREP_PERMISSION_END";

/// Default MCP tool timeout in seconds, mirroring TS.
pub const DEFAULT_TOOL_TIMEOUT_SECS: u32 = 600;

/// Installable integration target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallTarget {
    /// Claude Code (`~/.claude.json`).
    Claude,
    /// Codex (`~/.codex/config.toml`).
    Codex,
    /// `OpenCode` (`~/.config/opencode/opencode.json`).
    OpenCode,
    /// Cursor (`~/.cursor/mcp.json`).
    Cursor,
    /// Qwen Code (`~/.qwen/settings.json`).
    Qwen,
    /// Qoder (`~/.qoder/settings.json` CLI + `~/.qoder/mcp.json` IDE).
    Qoder,
    /// GitHub Copilot CLI (`~/.copilot/mcp-config.json`).
    Copilot,
    /// VS Code (`<user-data>/Code/User/mcp.json` per channel).
    VsCode,
    /// Grok Build (`~/.grok/config.toml`).
    Grok,
}

impl InstallTarget {
    /// Parses a `--target` token, mirroring installer ids and aliases.
    pub fn parse(token: &str) -> Result<Self, CliError> {
        match token.to_lowercase().as_str() {
            "claude" | "cc" | "claude-code" => Ok(Self::Claude),
            "codex" => Ok(Self::Codex),
            "opencode" => Ok(Self::OpenCode),
            "cursor" => Ok(Self::Cursor),
            "qwen" | "qwen-code" | "qwencode" => Ok(Self::Qwen),
            "qoder" => Ok(Self::Qoder),
            "copilot" | "github-copilot" | "copilot-cli" => Ok(Self::Copilot),
            "vscode" | "vs-code" | "code" => Ok(Self::VsCode),
            "grok" | "grok-build" | "grok-cli" => Ok(Self::Grok),
            _ => Err(CliError::usage(format!(
                "Unknown install target \"{token}\". Expected claude, codex, opencode, cursor, qwen, qoder, copilot, vscode, grok, all, or auto."
            ))),
        }
    }

    /// Human label, mirroring installer labels.
    pub fn label(self) -> &'static str {
        match self {
            Self::Claude => "Claude Code",
            Self::Codex => "Codex",
            Self::OpenCode => "OpenCode",
            Self::Cursor => "Cursor",
            Self::Qwen => "Qwen Code",
            Self::Qoder => "Qoder",
            Self::Copilot => "GitHub Copilot",
            Self::VsCode => "VS Code",
            Self::Grok => "Grok Build",
        }
    }

    /// Every target, in upstream `AGENT_INSTALLERS` order.
    pub const ALL: [Self; 9] = [
        Self::Claude,
        Self::Codex,
        Self::OpenCode,
        Self::Cursor,
        Self::Qwen,
        Self::Qoder,
        Self::Copilot,
        Self::VsCode,
        Self::Grok,
    ];
}

/// Options for install/uninstall runs.
#[derive(Debug, Clone)]
pub struct InstallOptions {
    /// MCP transport.
    pub transport: McpTransportArg,
    /// MCP toolset.
    pub toolset: McpToolset,
    /// Tool timeout in seconds.
    pub timeout_secs: u32,
    /// Bearer-token env var (http only).
    pub token_env: Option<String>,
    /// Consent to replace unmanaged entries.
    pub force: bool,
    /// Daemon URL for http entries.
    pub server_url: String,
}

impl InstallOptions {
    /// Builds options from CLI flags with TS defaults.
    pub fn new(
        transport: Option<McpTransportArg>,
        toolset: Option<crate::cli::McpToolsetArg>,
        timeout: Option<u32>,
        token_env: Option<String>,
        force: bool,
    ) -> Self {
        Self {
            transport: transport.unwrap_or(McpTransportArg::Stdio),
            toolset: toolset.map_or(McpToolset::Agent, |toolset| match toolset {
                crate::cli::McpToolsetArg::Agent => McpToolset::Agent,
                crate::cli::McpToolsetArg::Full => McpToolset::Full,
            }),
            timeout_secs: timeout.unwrap_or(DEFAULT_TOOL_TIMEOUT_SECS),
            token_env,
            force,
            server_url: resolve_server_url(),
        }
    }
}

/// Primary config file path for a target under `home`, honoring
/// upstream env overrides (`CLAUDE_CONFIG_DIR`, `CODEX_HOME`,
/// `OPENCODE_CONFIG`, `XDG_CONFIG_HOME`, `CURSOR_CONFIG_DIR`,
/// `QODER_CONFIG_DIR`, and the new targets' homes).
#[must_use]
pub fn config_path(target: InstallTarget, home: &Path) -> PathBuf {
    paths::config_path(target, home)
}

/// Installs one target; returns files written.
pub fn install(
    target: InstallTarget,
    options: &InstallOptions,
    home: &Path,
) -> Result<Vec<PathBuf>, CliError> {
    match target {
        InstallTarget::Codex => install_codex(&config_path(target, home), options),
        InstallTarget::Grok => install_grok(&config_path(target, home), options),
        InstallTarget::Qoder => {
            let mut written = Vec::new();
            for path in paths::install_paths(target, home) {
                let entry = if path.file_name().is_some_and(|name| name == "settings.json") {
                    server_entry(target, options, None)
                } else {
                    qoder_ide_entry(options)
                };
                written.extend(install_json_entry(target, &path, options, entry)?);
            }
            Ok(written)
        }
        InstallTarget::Claude
        | InstallTarget::OpenCode
        | InstallTarget::Cursor
        | InstallTarget::Qwen
        | InstallTarget::Copilot
        | InstallTarget::VsCode => {
            let mut written = Vec::new();
            for path in paths::install_paths(target, home) {
                written.extend(install_json(target, &path, options)?);
            }
            Ok(written)
        }
    }
}

/// Uninstalls one target; returns files written (missing files are
/// skipped silently).
pub fn uninstall(target: InstallTarget, home: &Path) -> Result<Vec<PathBuf>, CliError> {
    match target {
        InstallTarget::Codex => uninstall_codex(&config_path(target, home)),
        InstallTarget::Grok => uninstall_grok(&config_path(target, home)),
        InstallTarget::Claude
        | InstallTarget::OpenCode
        | InstallTarget::Cursor
        | InstallTarget::Qwen
        | InstallTarget::Qoder
        | InstallTarget::Copilot
        | InstallTarget::VsCode => {
            let mut written = Vec::new();
            for path in paths::uninstall_paths(target, home) {
                written.extend(uninstall_json(&path)?);
            }
            Ok(written)
        }
    }
}

/// Detects installed targets: an agent executable on `PATH` (mirroring
/// upstream `executableIsAvailable`), or a pre-existing config file (or
/// its parent dir) under `home` as a fallback for agents whose binary
/// is not on `PATH` in this session.
#[must_use]
pub fn detect_targets(home: &Path) -> Vec<InstallTarget> {
    InstallTarget::ALL
        .iter()
        .copied()
        .filter(|target| target_executable_present(*target) || config_present(*target, home))
        .collect()
}

/// True when any of the target's agent executables is runnable from
/// `PATH` (mirroring upstream `AGENT_INSTALLERS[].executables`).
fn target_executable_present(target: InstallTarget) -> bool {
    target_executables(target)
        .iter()
        .any(|executable| executable_on_path(executable))
}

/// Agent executables per target, mirroring upstream `AGENT_INSTALLERS`.
fn target_executables(target: InstallTarget) -> &'static [&'static str] {
    match target {
        InstallTarget::Claude => &["claude"],
        InstallTarget::Codex => &["codex"],
        InstallTarget::OpenCode => &["opencode"],
        InstallTarget::Cursor => &["cursor"],
        InstallTarget::Qwen => &["qwen"],
        InstallTarget::Qoder => &["qoder", "qodercli", "qoder-ide"],
        InstallTarget::Copilot => &["copilot"],
        InstallTarget::VsCode => &["code", "code-insiders"],
        InstallTarget::Grok => &["grok"],
    }
}

/// True when `executable` resolves to an executable file in a `PATH`
/// directory (empty entries mean the current directory, as in shells).
pub(crate) fn executable_on_path(executable: &str) -> bool {
    let Some(path_var) = std::env::var_os("PATH") else {
        return false;
    };
    for entry in std::env::split_paths(&path_var) {
        let dir = if entry.as_os_str().is_empty() {
            PathBuf::from(".")
        } else {
            entry
        };
        let candidate = dir.join(executable);
        if is_executable_file(&candidate) {
            return true;
        }
        #[cfg(windows)]
        for extension in ["exe", "cmd", "bat", "com"] {
            if is_executable_file(&candidate.with_extension(extension)) {
                return true;
            }
        }
    }
    false
}

/// True when `path` is a regular file with any execute bit set (Unix);
/// on non-Unix any existing regular file counts.
fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// True when the target's config file (or its parent dir) already exists
/// under `home`.
fn config_present(target: InstallTarget, home: &Path) -> bool {
    paths::install_paths(target, home)
        .iter()
        .any(|path| path.exists() || path.parent().is_some_and(Path::exists))
}

/// Container key holding MCP servers in a JSON target.
fn container_key(target: InstallTarget) -> &'static str {
    match target {
        InstallTarget::OpenCode => "mcp",
        InstallTarget::VsCode => "servers",
        InstallTarget::Claude
        | InstallTarget::Codex
        | InstallTarget::Cursor
        | InstallTarget::Qwen
        | InstallTarget::Qoder
        | InstallTarget::Copilot
        | InstallTarget::Grok => "mcpServers",
    }
}

/// Owned Claude keys: everything else on an existing Claude entry
/// (e.g. a user-set `env`) is retained when merging (upstream
/// `updateClaudeMcpConfig`).
const CLAUDE_OWNED_KEYS: [&str; 5] = ["type", "url", "command", "args", "headers"];

/// Merges a fresh Claude entry into the existing one, preserving
/// unknown user fields and replacing only owned keys.
fn merge_claude_entry(
    existing: Option<&serde_json::Value>,
    owned: serde_json::Value,
) -> serde_json::Value {
    let mut merged = existing
        .and_then(|value| value.as_object())
        .cloned()
        .unwrap_or_default();
    for key in CLAUDE_OWNED_KEYS {
        merged.remove(key);
    }
    if let serde_json::Value::Object(owned_map) = owned {
        for (key, value) in owned_map {
            merged.insert(key, value);
        }
    }
    serde_json::Value::Object(merged)
}

/// `zg --server --stdio` argv for `OpenCode`'s array `command`, with an
/// explicit non-default toolset (upstream `stdioCommand`).
fn opencode_stdio_command(options: &InstallOptions) -> Vec<String> {
    let mut command = vec!["zg".to_owned(), "--server".to_owned(), "--stdio".to_owned()];
    if options.toolset == McpToolset::Full {
        command.push("--mcp-toolset".to_owned());
        command.push("full".to_owned());
    }
    command
}

/// Managed server entry for a JSON target. `existing` is the current
/// `zvec_grep` entry, used only by Claude's merge-into-existing write.
fn server_entry(
    target: InstallTarget,
    options: &InstallOptions,
    existing: Option<&serde_json::Value>,
) -> serde_json::Value {
    let timeout_ms = u64::from(options.timeout_secs) * 1000;
    let mut entry = match options.transport {
        McpTransportArg::Stdio => match target {
            // OpenCode requires `command` as a single argv array; it
            // has no separate `args` field.
            InstallTarget::OpenCode => serde_json::json!({
                "type": "local",
                "command": opencode_stdio_command(options),
                "enabled": true,
                "timeout": timeout_ms,
            }),
            // Claude Code only recognises the entry as stdio with an
            // explicit `type`; without it an http entry is likewise
            // not treated as HTTP.
            InstallTarget::Claude | InstallTarget::VsCode => serde_json::json!({
                "type": "stdio",
                "command": "zg",
                "args": stdio_args(options),
            }),
            InstallTarget::Copilot => serde_json::json!({
                "type": "local",
                "command": "zg",
                "args": stdio_args(options),
                "tools": ["*"],
                "timeout": timeout_ms,
            }),
            InstallTarget::Codex
            | InstallTarget::Cursor
            | InstallTarget::Qwen
            | InstallTarget::Qoder
            | InstallTarget::Grok => {
                let mut entry = serde_json::json!({
                    "command": "zg",
                    "args": stdio_args(options),
                    "timeout": timeout_ms,
                });
                if target == InstallTarget::Qwen
                    && let Some(map) = entry.as_object_mut()
                {
                    map.insert("alwaysLoadTools".to_owned(), true.into());
                    map.insert("trust".to_owned(), true.into());
                }
                if target == InstallTarget::Qoder
                    && let Some(map) = entry.as_object_mut()
                {
                    map.insert("trust".to_owned(), true.into());
                    map.insert("description".to_owned(), "Managed by zg --install".into());
                }
                entry
            }
        },
        McpTransportArg::Http => match target {
            InstallTarget::Qwen => serde_json::json!({
                "httpUrl": options.server_url,
                "timeout": timeout_ms,
                "alwaysLoadTools": true,
                "trust": true,
            }),
            InstallTarget::Qoder => serde_json::json!({
                "type": "http",
                "url": options.server_url,
                "timeout": timeout_ms,
                "trust": true,
                "description": "Managed by zg --install",
            }),
            // OpenCode calls HTTP servers `remote`, not `http`.
            InstallTarget::OpenCode => serde_json::json!({
                "type": "remote",
                "url": options.server_url,
                "enabled": true,
                "timeout": timeout_ms,
                "oauth": false,
            }),
            InstallTarget::Claude | InstallTarget::VsCode => serde_json::json!({
                "type": "http",
                "url": options.server_url,
            }),
            InstallTarget::Copilot => serde_json::json!({
                "type": "http",
                "url": options.server_url,
                "tools": ["*"],
                "timeout": timeout_ms,
            }),
            InstallTarget::Codex | InstallTarget::Cursor | InstallTarget::Grok => {
                serde_json::json!({"url": options.server_url})
            }
        },
    };
    apply_token_header(&mut entry, target, options);
    if target == InstallTarget::Claude {
        entry = merge_claude_entry(existing, entry);
    }
    entry
}

/// Adds the bearer-token `Authorization` header in the host's env
/// syntax: `OpenCode` expands `{env:VAR}`, VS Code `${env:VAR}`,
/// other hosts `${VAR}`.
fn apply_token_header(
    entry: &mut serde_json::Value,
    target: InstallTarget,
    options: &InstallOptions,
) {
    if let Some(env) = &options.token_env
        && let Some(map) = entry.as_object_mut()
    {
        let authorization = if target == InstallTarget::OpenCode {
            format!("Bearer {{env:{env}}}")
        } else if target == InstallTarget::VsCode {
            format!("Bearer ${{env:{env}}}")
        } else {
            format!("Bearer ${{{env}}}")
        };
        map.insert(
            "headers".to_owned(),
            serde_json::json!({ "Authorization": authorization }),
        );
    }
}

/// Qoder IDE (`mcp.json`) entry: upstream `qoderIdeMcpServer` — no
/// `trust`, HTTP rides `sse`, both shapes carry the managed marker
/// description so uninstall can tell them from foreign entries.
fn qoder_ide_entry(options: &InstallOptions) -> serde_json::Value {
    let timeout_ms = u64::from(options.timeout_secs) * 1000;
    let mut entry = match options.transport {
        McpTransportArg::Stdio => serde_json::json!({
            "command": "zg",
            "args": stdio_args(options),
            "timeout": timeout_ms,
            "description": "Managed by zg --install",
        }),
        McpTransportArg::Http => serde_json::json!({
            "type": "sse",
            "url": options.server_url,
            "timeout": timeout_ms,
            "description": "Managed by zg --install",
        }),
    };
    if let Some(env) = &options.token_env
        && let Some(map) = entry.as_object_mut()
    {
        map.insert(
            "headers".to_owned(),
            serde_json::json!({ "Authorization": format!("Bearer ${{{env}}}") }),
        );
    }
    entry
}

/// `zg server --stdio` argv, with an explicit non-default toolset.
fn stdio_args(options: &InstallOptions) -> Vec<String> {
    let mut args = vec!["server".to_owned(), "--stdio".to_owned()];
    if options.toolset == McpToolset::Full {
        args.push("--mcp-toolset".to_owned());
        args.push("full".to_owned());
    }
    args
}

/// True for entries this installer owns (mirroring upstream
/// `isManagedJsonMcpServer`): the daemon's server URL, a `zg` stdio
/// launch, or the `OpenCode` array-command form.
fn is_managed(entry: &serde_json::Value, server_url: &str) -> bool {
    if !entry.is_object() {
        return false;
    }
    if entry
        .get("url")
        .and_then(|url| url.as_str())
        .is_some_and(|url| url == server_url)
    {
        return true;
    }
    // Qwen's `httpUrl` twin of the daemon URL.
    if entry
        .get("httpUrl")
        .and_then(|url| url.as_str())
        .is_some_and(|url| url == server_url)
    {
        return true;
    }
    if entry.get("command").and_then(|command| command.as_str()) == Some("zg")
        && entry.get("args").is_some_and(is_stdio_args)
    {
        return true;
    }
    // OpenCode array form, mirroring upstream `isManagedJsonMcpServer`
    // (`["zg", "server", "--stdio", ..]`); this port writes the
    // `--server` flag spelling, so both count as managed.
    if let Some(command) = entry.get("command").and_then(|command| command.as_array())
        && let [program, action, stdio, rest @ ..] = command.as_slice()
        && program.as_str() == Some("zg")
        && matches!(action.as_str(), Some("server" | "--server"))
        && stdio.as_str() == Some("--stdio")
        && is_toolset_tail(rest)
    {
        return entry.get("type").and_then(|kind| kind.as_str()) == Some("local");
    }
    false
}

/// True for upstream `stdioArgs` shapes: `["server", "--stdio"]` plus an
/// optional `--mcp-toolset agent|full` tail.
fn is_stdio_args(args: &serde_json::Value) -> bool {
    let Some(args) = args.as_array() else {
        return false;
    };
    let [action, stdio, rest @ ..] = args.as_slice() else {
        return false;
    };
    action.as_str() == Some("server") && stdio.as_str() == Some("--stdio") && is_toolset_tail(rest)
}

/// True for the empty tail or `--mcp-toolset agent|full`.
fn is_toolset_tail(rest: &[serde_json::Value]) -> bool {
    match rest {
        [] => true,
        [flag, toolset] => {
            flag.as_str() == Some("--mcp-toolset")
                && matches!(toolset.as_str(), Some("agent" | "full"))
        }
        _ => false,
    }
}

fn install_json(
    target: InstallTarget,
    path: &Path,
    options: &InstallOptions,
) -> Result<Vec<PathBuf>, CliError> {
    // Existing entry is needed only for Claude's merge; reading it
    // here keeps `server_entry` free of I/O. A tolerant parse failure
    // is deferred to `install_json_entry`, which owns the force gate.
    let existing = read_text_file(path).ok().and_then(|raw| {
        jsonc::parse_jsonc_object(path, &raw)
            .ok()
            .flatten()
            .and_then(|root| {
                root.get(container_key(target))
                    .and_then(|container| container.get("zvec_grep"))
                    .cloned()
            })
    });
    let entry = server_entry(target, options, existing.as_ref());
    install_json_entry(target, path, options, entry)
}

fn install_json_entry(
    target: InstallTarget,
    path: &Path,
    options: &InstallOptions,
    entry: serde_json::Value,
) -> Result<Vec<PathBuf>, CliError> {
    let key = container_key(target);
    let raw = read_text_file(path)?;
    let mut root = match jsonc::parse_jsonc_object(path, &raw) {
        Ok(None) => serde_json::Value::Object(serde_json::Map::default()),
        Ok(Some(root)) => root,
        Err(error) => {
            if !options.force {
                return Err(error);
            }
            // `--force` replaces an unparseable file from scratch.
            let mut root = serde_json::Value::Object(serde_json::Map::default());
            if let Some(root_map) = root.as_object_mut() {
                root_map.insert(key.to_owned(), serde_json::json!({ "zvec_grep": entry }));
            }
            write_json_file(path, &root)?;
            return Ok(vec![path.to_owned()]);
        }
    };
    let existing = root
        .get(key)
        .and_then(|container| container.get("zvec_grep"))
        .cloned();
    if existing
        .as_ref()
        .is_some_and(|entry| !is_managed(entry, options.server_url.as_str()))
        && !options.force
    {
        return Err(CliError::install_refused(format!(
            "Existing unmanaged zvec_grep MCP server found in {}. Re-run with --force to replace it for {}.",
            path.display(),
            target.label()
        )));
    }
    // JSONC files keep their comments/trailing commas: patch the
    // entry textually instead of a serde rewrite that would drop them.
    if jsonc::needs_preservation(&raw) {
        let next = jsonc::upsert_entry(&raw, key, &entry);
        write_text_file(path, &next)?;
        return Ok(vec![path.to_owned()]);
    }
    let mut container = root
        .get(key)
        .cloned()
        .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::default()));
    if let Some(container_map) = container.as_object_mut() {
        container_map.insert("zvec_grep".to_owned(), entry);
    }
    if let Some(root_map) = root.as_object_mut() {
        root_map.insert(key.to_owned(), container);
    }
    write_json_file(path, &root)?;
    Ok(vec![path.to_owned()])
}

fn uninstall_json(path: &Path) -> Result<Vec<PathBuf>, CliError> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let raw = read_text_file(path)?;
    let Some(mut root) = jsonc::parse_jsonc_object(path, &raw)? else {
        return Ok(Vec::new());
    };
    let server_url = resolve_server_url();
    for key in ["mcp", "mcpServers", "servers"] {
        let managed = root
            .get(key)
            .and_then(|container| container.get("zvec_grep"))
            .is_some_and(|entry| is_managed(entry, server_url.as_str()));
        if !managed {
            continue;
        }
        if jsonc::needs_preservation(&raw) {
            if let Some(next) = jsonc::remove_entry(&raw, key) {
                write_text_file(path, &next)?;
                return Ok(vec![path.to_owned()]);
            }
            continue;
        }
        if let Some(container) = root.get_mut(key).and_then(|value| value.as_object_mut())
            && container.remove("zvec_grep").is_some()
        {
            write_json_file(path, &root)?;
            return Ok(vec![path.to_owned()]);
        }
    }
    Ok(Vec::new())
}

fn install_codex(path: &Path, options: &InstallOptions) -> Result<Vec<PathBuf>, CliError> {
    let existing = read_text_file(path)?;
    let block = codex_block(options);
    if let Some(replaced) = replace_marked_block(&existing, CONFIG_START, CONFIG_END, &block) {
        write_text_file(path, &replaced)?;
        return Ok(vec![path.to_owned()]);
    }
    if existing.contains("[mcp_servers.zvec_grep]") && !options.force {
        return Err(CliError::install_refused(format!(
            "Existing unmanaged zvec_grep MCP server found in {}. Re-run with --force to replace it for Codex.",
            path.display()
        )));
    }
    let mut next = existing;
    if !next.is_empty() && !next.ends_with('\n') {
        next.push('\n');
    }
    next.push_str(&block);
    next.push('\n');
    write_text_file(path, &next)?;
    Ok(vec![path.to_owned()])
}

fn uninstall_codex(path: &Path) -> Result<Vec<PathBuf>, CliError> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let existing = read_text_file(path)?;
    let stripped = remove_marked_block(&existing, CONFIG_START, CONFIG_END);
    if stripped == existing {
        return Ok(Vec::new());
    }
    write_text_file(path, &stripped)?;
    Ok(vec![path.to_owned()])
}

fn install_grok(path: &Path, options: &InstallOptions) -> Result<Vec<PathBuf>, CliError> {
    let existing = read_text_file(path)?;
    let block = grok_block(options);
    let mut next = if let Some(replaced) =
        replace_marked_block(&existing, CONFIG_START, CONFIG_END, &block)
    {
        replaced
    } else {
        if existing.contains("[mcp_servers.zvec_grep]") && !options.force {
            return Err(CliError::install_refused(format!(
                "Existing unmanaged zvec_grep MCP server found in {}. Re-run with --force to replace it for Grok Build.",
                path.display()
            )));
        }
        let mut next = existing.clone();
        if !next.is_empty() && !next.ends_with('\n') {
            next.push('\n');
        }
        next.push_str(&block);
        next.push('\n');
        next
    };
    // Permission block is skipped when the user already owns a
    // `[permission]` table (upstream `hasGrokPermissionTable`):
    // TOML forbids a second one.
    let permission = grok_permission_block();
    if let Some(replaced) = replace_marked_block(
        &next,
        GROK_PERMISSION_START,
        GROK_PERMISSION_END,
        &permission,
    ) {
        next = replaced;
    } else if !has_grok_permission_table(&next) {
        if !next.is_empty() && !next.ends_with('\n') {
            next.push('\n');
        }
        next.push_str(&permission);
        next.push('\n');
    }
    write_text_file(path, &next)?;
    Ok(vec![path.to_owned()])
}

fn uninstall_grok(path: &Path) -> Result<Vec<PathBuf>, CliError> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let existing = read_text_file(path)?;
    let stripped = remove_marked_block(
        &remove_marked_block(&existing, GROK_PERMISSION_START, GROK_PERMISSION_END),
        CONFIG_START,
        CONFIG_END,
    );
    if stripped == existing {
        return Ok(Vec::new());
    }
    write_text_file(path, &stripped)?;
    Ok(vec![path.to_owned()])
}

/// Grok TOML block, mirroring `grokConfigBlock`.
fn grok_block(options: &InstallOptions) -> String {
    let mut block = String::from(CONFIG_START);
    block.push_str("\n[mcp_servers.zvec_grep]\n");
    match options.transport {
        McpTransportArg::Stdio => {
            block.push_str("command = \"zg\"\nargs = ");
            block.push_str(&toml_string_array(&stdio_args(options)));
            block.push_str("\n# First-run daemon and local-model warmup can exceed Grok's 30s startup default.\nstartup_timeout_sec = 120");
        }
        McpTransportArg::Http => {
            let _ = write!(block, "url = {}", toml_basic_string(&options.server_url));
            if let Some(env) = &options.token_env {
                let _ = write!(
                    block,
                    "\nheaders = {{ Authorization = {} }}",
                    toml_basic_string(&format!("Bearer ${{{env}}}"))
                );
            }
        }
    }
    block.push('\n');
    block.push_str(CONFIG_END);
    block
}

fn grok_permission_block() -> String {
    format!(
        "{GROK_PERMISSION_START}\n[permission]\nallow = [\"MCPTool(zvec_grep__*)\"]\n{GROK_PERMISSION_END}"
    )
}

/// Line-level `[permission]` detection (upstream errs toward matching:
/// a false positive only skips the managed block).
fn has_grok_permission_table(existing: &str) -> bool {
    existing.lines().any(|line| {
        let line = line.trim();
        line.starts_with("[permission]")
            || line.starts_with("[permission.")
            || line.starts_with("permission =")
            || line.starts_with("permission.")
    })
}

/// Codex TOML block, mirroring `codexConfigBlock` (stdio shape).
fn codex_block(options: &InstallOptions) -> String {
    let mut block = String::from(CONFIG_START);
    block.push_str("\n[mcp_servers.zvec_grep]\n");
    match options.transport {
        McpTransportArg::Stdio => {
            block.push_str("command = \"zg\"\nargs = ");
            block.push_str(&toml_string_array(&stdio_args(options)));
        }
        McpTransportArg::Http => {
            let _ = write!(block, "url = {}", toml_basic_string(&options.server_url));
        }
    }
    if let Some(env) = &options.token_env {
        let _ = write!(block, "\nbearer_token_env_var = {}", toml_basic_string(env));
    }
    block.push('\n');
    block.push_str(CONFIG_END);
    block
}

/// TOML basic string: `"` and `\` (plus control chars) escaped so a
/// server URL or env-var name containing them cannot corrupt
/// `config.toml` (mirrors upstream `JSON.stringify` quoting).
fn toml_basic_string(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('"');
    for ch in value.chars() {
        match ch {
            '"' => quoted.push_str("\\\""),
            '\\' => quoted.push_str("\\\\"),
            '\u{8}' => quoted.push_str("\\b"),
            '\t' => quoted.push_str("\\t"),
            '\n' => quoted.push_str("\\n"),
            '\u{c}' => quoted.push_str("\\f"),
            '\r' => quoted.push_str("\\r"),
            ch if (ch as u32) < 0x20 => {
                let _ = write!(quoted, "\\u{:04X}", ch as u32);
            }
            ch => quoted.push(ch),
        }
    }
    quoted.push('"');
    quoted
}

fn toml_string_array(values: &[String]) -> String {
    let quoted: Vec<String> = values
        .iter()
        .map(|value| toml_basic_string(value))
        .collect();
    format!("[{}]", quoted.join(", "))
}

/// Replaces a marked block; `None` when no block is present.
fn replace_marked_block(existing: &str, start: &str, end: &str, block: &str) -> Option<String> {
    let start_at = existing.find(start)?;
    let end_at = existing[start_at..].find(end)? + start_at;
    let end_line = existing[end_at..]
        .find('\n')
        .map_or(existing.len(), |offset| end_at + offset + 1);
    let mut next = existing[..start_at].to_owned();
    next.push_str(block);
    next.push('\n');
    next.push_str(&existing[end_line..]);
    Some(next)
}

/// Removes a marked block; returns the input unchanged when absent.
fn remove_marked_block(existing: &str, start: &str, end: &str) -> String {
    let Some(start_at) = existing.find(start) else {
        return existing.to_owned();
    };
    let Some(end_rel) = existing[start_at..].find(end) else {
        return existing.to_owned();
    };
    let end_at = start_at + end_rel;
    let end_line = existing[end_at..]
        .find('\n')
        .map_or(existing.len(), |offset| end_at + offset + 1);
    format!("{}{}", &existing[..start_at], &existing[end_line..])
}

fn read_text_file(path: &Path) -> Result<String, CliError> {
    if !path.exists() {
        return Ok(String::new());
    }
    std::fs::read_to_string(path).map_err(|error| CliError::io(path, error))
}

fn write_json_file(path: &Path, value: &serde_json::Value) -> Result<(), CliError> {
    // Delegate to the hardened atomic writer: unique tmp + `create_new` +
    // fsync + rename + dir fsync. Parent-directory modes are left alone
    // (these are user config dirs, not owned `0700` roots); the file itself
    // stays `0600` on Unix.
    zg_core::utils::json_io::write_json_file(
        path,
        value,
        zg_core::utils::json_io::WriteModes {
            directory_mode: None,
            file_mode: Some(0o600),
        },
    )?;
    Ok(())
}

/// Process-wide counter disambiguating same-nanosecond tmp names.
static TMP_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Unique tmp sibling for atomic writes (pid + nanos + counter), so
/// concurrent installs never share a fixed `.tmp` name. This crate has no
/// `uuid` dependency; the counter disambiguates same-nanosecond writes
/// from threads of one process while the pid disambiguates processes.
fn unique_tmp_path(path: &Path) -> PathBuf {
    let count = TMP_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    path.with_extension(format!("{}.{}.{}.tmp", std::process::id(), nanos, count))
}

/// Persists the parent directory entry so a just-completed rename survives
/// a crash.
fn sync_parent_dir(path: &Path) -> Result<(), CliError> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let handle = std::fs::File::open(parent).map_err(|error| CliError::io(path, error))?;
    handle.sync_all().map_err(|error| CliError::io(path, error))
}

/// Atomic write via unique tmp file + `create_new` + fsync + rename on all
/// platforms, `0600` on Unix. A crash mid-write never leaves a truncated
/// destination.
fn write_text_file(path: &Path, text: &str) -> Result<(), CliError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| CliError::io(path, error))?;
    }
    let tmp = unique_tmp_path(path);
    let write_result = (|| -> Result<(), CliError> {
        // `create_new` never truncates a pre-existing file: a tmp-name
        // collision surfaces as an error instead of silent corruption.
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .map_err(|error| CliError::io(path, error))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))
                .map_err(|error| CliError::io(path, error))?;
        }
        std::io::Write::write_all(&mut file, text.as_bytes())
            .map_err(|error| CliError::io(path, error))?;
        std::io::Write::flush(&mut file).map_err(|error| CliError::io(path, error))?;
        file.sync_all().map_err(|error| CliError::io(path, error))?;
        drop(file);
        std::fs::rename(&tmp, path).map_err(|error| CliError::io(path, error))?;
        sync_parent_dir(path)?;
        Ok(())
    })();
    if let Err(error) = write_result {
        let _ = std::fs::remove_file(&tmp);
        return Err(error);
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod golden_tests;

#[cfg(test)]
// A panic in test code is just a test failure, so indexing in assertions needs no guard.
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn options() -> InstallOptions {
        InstallOptions {
            transport: McpTransportArg::Stdio,
            toolset: McpToolset::Agent,
            timeout_secs: 600,
            token_env: None,
            force: false,
            server_url: "http://127.0.0.1:18099".to_owned(),
        }
    }

    fn home() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn json_merge_preserves_unrelated_servers() {
        let home = home();
        let dir = home.path();
        let path = config_path(InstallTarget::Cursor, dir);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{"mcpServers": {"other": {"command": "x"}}}"#).unwrap();
        let written = install(InstallTarget::Cursor, &options(), dir).unwrap();
        assert_eq!(written, vec![path.clone()]);
        let root: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(root["mcpServers"]["other"]["command"], "x");
        assert_eq!(root["mcpServers"]["zvec_grep"]["command"], "zg");
        // Second run merges over the managed entry without consent.
        install(InstallTarget::Cursor, &options(), dir).unwrap();
    }

    #[test]
    fn unmanaged_entry_needs_force() {
        let home = home();
        let dir = home.path();
        let path = config_path(InstallTarget::Cursor, dir);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{"mcpServers": {"zvec_grep": {"command": "foreign"}}}"#,
        )
        .unwrap();
        let error = install(InstallTarget::Cursor, &options(), dir).expect_err("must refuse");
        assert!(error.to_string().contains("--force"), "{error}");
        let mut forced = options();
        forced.force = true;
        install(InstallTarget::Cursor, &forced, dir).unwrap();
        let root: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(root["mcpServers"]["zvec_grep"]["command"], "zg");
    }

    #[test]
    fn jsonc_comments_are_preserved() {
        let home = home();
        let dir = home.path();
        let path = config_path(InstallTarget::Qwen, dir);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{\n// keep me\n}").unwrap();
        install(InstallTarget::Qwen, &options(), dir).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("// keep me"), "{text}");
        let root = jsonc::parse_jsonc_object(&path, &text).unwrap().unwrap();
        assert_eq!(root["mcpServers"]["zvec_grep"]["command"], "zg");
    }

    #[test]
    fn opencode_prefers_existing_jsonc() {
        let home = home();
        let dir = home.path();
        let jsonc = dir.join(".config").join("opencode").join("opencode.jsonc");
        std::fs::create_dir_all(jsonc.parent().unwrap()).unwrap();
        std::fs::write(&jsonc, "{\n// oc\n\"mcp\": {},\n}").unwrap();
        let written = install(InstallTarget::OpenCode, &options(), dir).unwrap();
        assert_eq!(written, vec![jsonc.clone()]);
        let text = std::fs::read_to_string(&jsonc).unwrap();
        assert!(text.contains("// oc"), "{text}");
        let root = jsonc::parse_jsonc_object(&jsonc, &text).unwrap().unwrap();
        assert_eq!(root["mcp"]["zvec_grep"]["type"], "local");
        // Uninstall cleans the entry from the selected .jsonc too.
        assert_eq!(
            uninstall(InstallTarget::OpenCode, dir).unwrap(),
            vec![jsonc.clone()]
        );
        let after = std::fs::read_to_string(&jsonc).unwrap();
        assert!(after.contains("// oc"), "{after}");
    }

    #[test]
    fn qoder_writes_cli_and_ide_configs() {
        let home = home();
        let dir = home.path();
        let written = install(InstallTarget::Qoder, &options(), dir).unwrap();
        assert_eq!(written.len(), 2);
        let cli: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join(".qoder").join("settings.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(cli["mcpServers"]["zvec_grep"]["trust"], true);
        let ide: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join(".qoder").join("mcp.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            ide["mcpServers"]["zvec_grep"]["description"],
            "Managed by zg --install"
        );
        assert_eq!(uninstall(InstallTarget::Qoder, dir).unwrap().len(), 2);
    }

    #[test]
    fn copilot_and_grok_round_trip() {
        let home = home();
        let dir = home.path();
        install(InstallTarget::Copilot, &options(), dir).unwrap();
        let copilot: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join(".copilot").join("mcp-config.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            copilot["mcpServers"]["zvec_grep"]["tools"],
            serde_json::json!(["*"])
        );
        install(InstallTarget::Grok, &options(), dir).unwrap();
        let grok_path = dir.join(".grok").join("config.toml");
        let grok = std::fs::read_to_string(&grok_path).unwrap();
        assert!(grok.contains("[mcp_servers.zvec_grep]"), "{grok}");
        assert!(grok.contains("[permission]"), "{grok}");
        uninstall(InstallTarget::Grok, dir).unwrap();
        assert!(
            !std::fs::read_to_string(&grok_path)
                .unwrap()
                .contains("zvec_grep")
        );
        // Parse aliases.
        assert_eq!(
            InstallTarget::parse("vscode").unwrap(),
            InstallTarget::VsCode
        );
        assert_eq!(
            InstallTarget::parse("github-copilot").unwrap(),
            InstallTarget::Copilot
        );
        assert_eq!(
            InstallTarget::parse("grok-cli").unwrap(),
            InstallTarget::Grok
        );
    }

    #[test]
    fn vscode_writes_servers_container() {
        let home = home();
        let dir = home.path();
        let written = install(InstallTarget::VsCode, &options(), dir).unwrap();
        assert_eq!(written.len(), 1);
        let root: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&written[0]).unwrap()).unwrap();
        assert_eq!(root["servers"]["zvec_grep"]["type"], "stdio");
        assert_eq!(uninstall(InstallTarget::VsCode, dir).unwrap().len(), 1);
    }

    #[test]
    fn codex_block_round_trip() {
        let home = home();
        let dir = home.path();
        let path = config_path(InstallTarget::Codex, dir);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "[other]\nkey = 1\n").unwrap();
        install(InstallTarget::Codex, &options(), dir).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("[mcp_servers.zvec_grep]"), "{text}");
        assert!(
            text.contains(CONFIG_START) && text.contains(CONFIG_END),
            "{text}"
        );
        assert!(text.contains("[other]"), "{text}");
        // Reinstall replaces the block instead of duplicating it.
        install(InstallTarget::Codex, &options(), dir).unwrap();
        let again = std::fs::read_to_string(&path).unwrap();
        assert_eq!(again.matches(CONFIG_START).count(), 1);
        // Uninstall removes only the managed block.
        let removed = uninstall(InstallTarget::Codex, dir).unwrap();
        assert_eq!(removed, vec![path.clone()]);
        let stripped = std::fs::read_to_string(&path).unwrap();
        assert!(!stripped.contains("zvec_grep"), "{stripped}");
        assert!(stripped.contains("[other]"), "{stripped}");
    }

    #[test]
    fn uninstall_is_idempotent() {
        let home = home();
        let dir = home.path();
        assert!(uninstall(InstallTarget::Cursor, dir).unwrap().is_empty());
        install(InstallTarget::Cursor, &options(), dir).unwrap();
        assert_eq!(uninstall(InstallTarget::Cursor, dir).unwrap().len(), 1);
        assert!(uninstall(InstallTarget::Cursor, dir).unwrap().is_empty());
    }

    #[test]
    fn comment_scanner_skips_urls_in_strings() {
        assert!(!jsonc::needs_preservation(r#"{"url": "http://x/y"}"#));
        assert!(jsonc::needs_preservation("{\n// c\n}"));
        assert!(jsonc::needs_preservation("{\n/* c */\n}"));
    }

    /// Regression for #76.1: `executable_on_path` resolves an agent
    /// binary from a `PATH`-style value without any config file present.
    /// (`detect_targets` itself reads process `PATH`, which a test must
    /// not mutate; the lookup helper takes effect through it.)
    #[test]
    fn path_executable_counts_as_detected() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let exe = bin.join("qodercli");
        std::fs::write(&exe, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        assert!(is_executable_file(&exe));
        assert!(!is_executable_file(&bin.join("missing")));
        assert!(target_executables(InstallTarget::Qoder).contains(&"qodercli"));
        // Non-executable regular files never count on Unix.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let plain = bin.join("plain");
            std::fs::write(&plain, "x").unwrap();
            std::fs::set_permissions(&plain, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert!(!is_executable_file(&plain));
        }
    }

    /// Regression for #76.2a: a foreign `http(s)` URL is unmanaged and
    /// needs `--force`; the daemon's own URL reinstalls freely.
    #[test]
    fn foreign_http_url_needs_force() {
        let home = home();
        let dir = home.path();
        let path = config_path(InstallTarget::Cursor, dir);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{"mcpServers": {"zvec_grep": {"url": "https://foreign.example/mcp"}}}"#,
        )
        .unwrap();
        let error = install(InstallTarget::Cursor, &options(), dir).expect_err("must refuse");
        assert!(error.to_string().contains("--force"), "{error}");
        // The daemon URL itself is managed: reinstall needs no consent.
        std::fs::write(
            &path,
            r#"{"mcpServers": {"zvec_grep": {"url": "http://127.0.0.1:18099"}}}"#,
        )
        .unwrap();
        install(InstallTarget::Cursor, &options(), dir).unwrap();
    }

    /// Regression for #76.2b: uninstall never removes a foreign entry.
    #[test]
    fn uninstall_keeps_unmanaged_entry() {
        let home = home();
        let dir = home.path();
        let path = config_path(InstallTarget::Cursor, dir);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{"mcpServers": {"zvec_grep": {"command": "foreign"}}}"#,
        )
        .unwrap();
        assert!(uninstall(InstallTarget::Cursor, dir).unwrap().is_empty());
        let root: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(root["mcpServers"]["zvec_grep"]["command"], "foreign");
    }

    /// Regression for #76.3: `--force` replaces a commented file that
    /// `read_json_object` cannot parse.
    #[test]
    fn force_replaces_commented_file() {
        let home = home();
        let dir = home.path();
        let path = config_path(InstallTarget::Qwen, dir);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{\n// keep me\n}").unwrap();
        let mut forced = options();
        forced.force = true;
        install(InstallTarget::Qwen, &forced, dir).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("// keep me"), "{text}");
        let root = jsonc::parse_jsonc_object(&path, &text).unwrap().unwrap();
        assert_eq!(root["mcpServers"]["zvec_grep"]["command"], "zg");
    }

    /// Regression for #76.4: `"` and `\` in TOML strings are escaped.
    #[test]
    fn toml_basic_string_escapes() {
        assert_eq!(toml_basic_string("a\"b\\c"), "\"a\\\"b\\\\c\"");
        let mut opts = options();
        opts.transport = McpTransportArg::Http;
        opts.server_url = "https://x.example/\"q\"\\p".to_owned();
        opts.token_env = Some("A\"B\\C".to_owned());
        let block = codex_block(&opts);
        assert!(
            block.contains("url = \"https://x.example/\\\"q\\\"\\\\p\""),
            "{block}"
        );
        assert!(
            block.contains("bearer_token_env_var = \"A\\\"B\\\\C\""),
            "{block}"
        );
    }
}
