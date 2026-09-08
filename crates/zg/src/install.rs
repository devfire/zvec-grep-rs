//! IDE MCP config installation (`cli/install.ts` behavior).
//!
//! Read-modify-write behind a `--yes`/`--force` gate: managed `zvec_grep`
//! entries and marked blocks merge freely; foreign entries are never
//! overwritten without explicit consent. JSON targets merge the
//! `zvec_grep` server object; Codex uses a marked TOML block. Guidance
//! markdown blocks (`AGENTS.md`) are out of scope by design (see
//! `docs/ts-divergence.md`).

use std::path::{Path, PathBuf};

use crate::cli::McpTransportArg;
use crate::client::resolve_server_url;
use crate::error::CliError;
use zg_server::mcp::toolset::McpToolset;

/// Managed TOML block markers for Codex, byte-identical to TS.
pub const CONFIG_START: &str = "# ZVEC_GREP_START";
/// Managed TOML block end marker, byte-identical to TS.
pub const CONFIG_END: &str = "# ZVEC_GREP_END";

/// Default MCP tool timeout in seconds, mirroring TS.
pub const DEFAULT_TOOL_TIMEOUT_SECS: u32 = 600;

/// Installable integration target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallTarget {
    /// Claude Code (`~/.claude.json`).
    Claude,
    /// Codex (`~/.codex/config.toml`).
    Codex,
    /// OpenCode (`~/.config/opencode/opencode.json`).
    OpenCode,
    /// Cursor (`~/.cursor/mcp.json`).
    Cursor,
    /// Qwen Code (`~/.qwen/settings.json`).
    Qwen,
    /// Qoder (`~/.qoder/mcp.json`).
    Qoder,
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
            _ => Err(CliError::usage(format!(
                "Unknown install target \"{token}\". Expected claude, codex, opencode, cursor, qwen, or qoder."
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
        }
    }
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

/// Config file path for a target under `home`.
pub fn config_path(target: InstallTarget, home: &Path) -> PathBuf {
    match target {
        InstallTarget::Claude => home.join(".claude.json"),
        InstallTarget::Codex => home.join(".codex").join("config.toml"),
        InstallTarget::OpenCode => home.join(".config").join("opencode").join("opencode.json"),
        InstallTarget::Cursor => home.join(".cursor").join("mcp.json"),
        InstallTarget::Qwen => home.join(".qwen").join("settings.json"),
        InstallTarget::Qoder => home.join(".qoder").join("mcp.json"),
    }
}

/// Installs one target; returns files written.
pub fn install(
    target: InstallTarget,
    options: &InstallOptions,
    home: &Path,
) -> Result<Vec<PathBuf>, CliError> {
    let path = config_path(target, home);
    match target {
        InstallTarget::Codex => install_codex(&path, options),
        _ => install_json(target, &path, options),
    }
}

/// Uninstalls one target; returns files written (missing files are
/// skipped silently).
pub fn uninstall(target: InstallTarget, home: &Path) -> Result<Vec<PathBuf>, CliError> {
    let path = config_path(target, home);
    match target {
        InstallTarget::Codex => uninstall_codex(&path),
        _ => uninstall_json(&path),
    }
}

/// Detects installed targets: config files (or their parent dirs) that
/// already exist under `home`.
pub fn detect_targets(home: &Path) -> Vec<InstallTarget> {
    const ALL: [InstallTarget; 6] = [
        InstallTarget::Claude,
        InstallTarget::Codex,
        InstallTarget::OpenCode,
        InstallTarget::Cursor,
        InstallTarget::Qwen,
        InstallTarget::Qoder,
    ];
    ALL.iter()
        .copied()
        .filter(|target| {
            let path = config_path(*target, home);
            path.exists() || path.parent().is_some_and(Path::exists)
        })
        .collect()
}

/// Container key holding MCP servers in a JSON target.
fn container_key(target: InstallTarget) -> &'static str {
    match target {
        InstallTarget::OpenCode => "mcp",
        _ => "mcpServers",
    }
}

/// Managed server entry for a JSON target.
fn server_entry(target: InstallTarget, options: &InstallOptions) -> serde_json::Value {
    let timeout_ms = u64::from(options.timeout_secs) * 1000;
    match options.transport {
        McpTransportArg::Stdio => {
            let mut entry = serde_json::json!({
                "command": "zg",
                "args": stdio_args(options),
                "timeout": timeout_ms,
            });
            if target == InstallTarget::Qwen {
                entry["alwaysLoadTools"] = true.into();
                entry["trust"] = true.into();
            }
            if target == InstallTarget::Qoder {
                entry["trust"] = true.into();
            }
            if target == InstallTarget::OpenCode {
                entry = serde_json::json!({
                    "type": "local",
                    "command": "zg",
                    "args": stdio_args(options),
                });
            }
            entry
        }
        McpTransportArg::Http => {
            let mut entry = match target {
                InstallTarget::Qwen => serde_json::json!({
                    "httpUrl": options.server_url,
                    "timeout": timeout_ms,
                    "alwaysLoadTools": true,
                    "trust": true,
                }),
                InstallTarget::Qoder | InstallTarget::OpenCode => serde_json::json!({
                    "type": "http",
                    "url": options.server_url,
                    "timeout": timeout_ms,
                }),
                _ => serde_json::json!({"url": options.server_url}),
            };
            if let Some(env) = &options.token_env {
                entry["headers"] = serde_json::json!({
                    "Authorization": format!("Bearer ${{{env}}}"),
                });
            }
            entry
        }
    }
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

/// True for entries this installer owns: a `zg` stdio command or an
/// http(s) URL entry.
fn is_managed(entry: &serde_json::Value) -> bool {
    if entry.get("command").and_then(|command| command.as_str()) == Some("zg") {
        return true;
    }
    for key in ["url", "httpUrl"] {
        if entry
            .get(key)
            .and_then(|url| url.as_str())
            .is_some_and(|url| url.starts_with("http://") || url.starts_with("https://"))
        {
            return true;
        }
    }
    false
}

fn install_json(
    target: InstallTarget,
    path: &Path,
    options: &InstallOptions,
) -> Result<Vec<PathBuf>, CliError> {
    let key = container_key(target);
    let mut root = read_json_object(path)?;
    if let Some(container) = root.get(key)
        && !container.is_object()
    {
        return Err(CliError::install_refused(format!(
            "Expected {key} in {} to be a JSON object",
            path.display()
        )));
    }
    if has_jsonc_comments(path)? && !options.force {
        return Err(CliError::install_refused(format!(
            "{} contains comments that a JSON rewrite would drop. Re-run with --force to replace it for {}.",
            path.display(),
            target.label()
        )));
    }
    let mut container = root
        .get(key)
        .cloned()
        .unwrap_or_else(|| serde_json::Value::Object(Default::default()));
    let existing = container.get("zvec_grep");
    if existing.is_some_and(|entry| !is_managed(entry)) && !options.force {
        return Err(CliError::install_refused(format!(
            "Existing unmanaged zvec_grep MCP server found in {}. Re-run with --force to replace it for {}.",
            path.display(),
            target.label()
        )));
    }
    container["zvec_grep"] = server_entry(target, options);
    root[key] = container;
    write_json_file(path, &root)?;
    Ok(vec![path.to_owned()])
}

fn uninstall_json(path: &Path) -> Result<Vec<PathBuf>, CliError> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let mut root = read_json_object(path)?;
    let mut changed = false;
    for key in ["mcp", "mcpServers"] {
        if let Some(container) = root.get_mut(key).and_then(|value| value.as_object_mut()) {
            if container.remove("zvec_grep").is_some() {
                changed = true;
            }
        }
    }
    if changed {
        write_json_file(path, &root)?;
        return Ok(vec![path.to_owned()]);
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
            block.push_str(&format!("url = \"{}\"", options.server_url));
        }
    }
    if let Some(env) = &options.token_env {
        block.push_str(&format!("\nbearer_token_env_var = \"{env}\""));
    }
    block.push('\n');
    block.push_str(CONFIG_END);
    block
}

fn toml_string_array(values: &[String]) -> String {
    let quoted: Vec<String> = values.iter().map(|value| format!("\"{value}\"")).collect();
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

fn read_json_object(path: &Path) -> Result<serde_json::Value, CliError> {
    if !path.exists() {
        return Ok(serde_json::Value::Object(Default::default()));
    }
    let text = std::fs::read_to_string(path).map_err(|error| CliError::io(path, error))?;
    serde_json::from_str(&text)
        .map_err(|_| {
            CliError::install_refused(format!(
                "{} is not valid JSON. Fix it or re-run with --force to replace it.",
                path.display()
            ))
        })
        .and_then(|value: serde_json::Value| {
            if value.is_object() {
                Ok(value)
            } else {
                Err(CliError::install_refused(format!(
                    "Expected {} to hold a JSON object",
                    path.display()
                )))
            }
        })
}

/// Naive comment scan outside string literals: JSONC comments would not
/// survive a serde rewrite, so they need explicit consent.
fn has_jsonc_comments(path: &Path) -> Result<bool, CliError> {
    if !path.exists() {
        return Ok(false);
    }
    let text = std::fs::read_to_string(path).map_err(|error| CliError::io(path, error))?;
    Ok(contains_comment(&text))
}

fn contains_comment(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut index = 0;
    let mut in_string = false;
    let mut escaped = false;
    while index < bytes.len() {
        let byte = bytes[index];
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
        } else if byte == b'"' {
            in_string = true;
        } else if byte == b'/' && index + 1 < bytes.len() {
            // `https://` inside a string is skipped above; a bare `//` or
            // `/*` outside strings is a JSONC comment.
            if bytes[index + 1] == b'/' || bytes[index + 1] == b'*' {
                return true;
            }
        }
        index += 1;
    }
    false
}

fn read_text_file(path: &Path) -> Result<String, CliError> {
    if !path.exists() {
        return Ok(String::new());
    }
    std::fs::read_to_string(path).map_err(|error| CliError::io(path, error))
}

fn write_json_file(path: &Path, value: &serde_json::Value) -> Result<(), CliError> {
    let text = format!(
        "{}\n",
        serde_json::to_string_pretty(value).unwrap_or_else(|_| "{}".to_owned())
    );
    write_text_file(path, &text)
}

/// Atomic write via tmp file + rename, `0600` on Unix.
fn write_text_file(path: &Path, text: &str) -> Result<(), CliError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| CliError::io(path, error))?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, text).map_err(|error| CliError::io(path, error))?;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
        std::fs::rename(&tmp, path).map_err(|error| CliError::io(path, error))?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, text).map_err(|error| CliError::io(path, error))?;
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn options() -> InstallOptions {
        InstallOptions {
            transport: McpTransportArg::Stdio,
            toolset: McpToolset::Agent,
            timeout_secs: 600,
            token_env: None,
            force: false,
            server_url: "http://127.0.0.1:7999".to_owned(),
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
    fn jsonc_comments_need_force() {
        let home = home();
        let dir = home.path();
        let path = config_path(InstallTarget::Qwen, dir);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{\n// keep me\n}").unwrap();
        let error = install(InstallTarget::Qwen, &options(), dir).expect_err("must refuse");
        assert!(error.to_string().contains("--force"), "{error}");
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
        assert!(!contains_comment(r#"{"url": "http://x/y"}"#));
        assert!(contains_comment("{\n// c\n}"));
        assert!(contains_comment("{\n/* c */\n}"));
    }
}
