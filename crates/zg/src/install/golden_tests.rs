//! Golden tests for OpenCode/Claude entry shapes (#61).

use super::*;

fn options(transport: McpTransportArg) -> InstallOptions {
    InstallOptions {
        transport,
        toolset: McpToolset::Agent,
        timeout_secs: 600,
        token_env: None,
        force: false,
        server_url: "http://127.0.0.1:7999".to_owned(),
    }
}

fn installed_entry(target: InstallTarget, opts: &InstallOptions, home: &Path) -> serde_json::Value {
    install(target, opts, home).unwrap();
    let path = config_path(target, home);
    let root: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    root[container_key(target)]["zvec_grep"].clone()
}

fn seed(target: InstallTarget, home: &Path, raw: &str) {
    let path = config_path(target, home);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, raw).unwrap();
}

#[test]
fn opencode_stdio_golden() {
    let home = tempfile::tempdir().unwrap();
    let entry = installed_entry(
        InstallTarget::OpenCode,
        &options(McpTransportArg::Stdio),
        home.path(),
    );
    assert_eq!(
        entry,
        serde_json::json!({
            "type": "local",
            "command": ["zg", "--server", "--stdio"],
            "enabled": true,
            "timeout": 600_000,
        })
    );
    assert!(entry.get("args").is_none());
    // Reinstall over the array-command entry needs no --force.
    installed_entry(
        InstallTarget::OpenCode,
        &options(McpTransportArg::Stdio),
        home.path(),
    );
}

#[test]
fn opencode_http_golden() {
    let home = tempfile::tempdir().unwrap();
    let mut opts = options(McpTransportArg::Http);
    opts.token_env = Some("ZG_TOKEN".to_owned());
    let entry = installed_entry(InstallTarget::OpenCode, &opts, home.path());
    assert_eq!(
        entry,
        serde_json::json!({
            "type": "remote",
            "url": "http://127.0.0.1:7999",
            "enabled": true,
            "timeout": 600_000,
            "oauth": false,
            "headers": {"Authorization": "Bearer {env:ZG_TOKEN}"},
        })
    );
}

#[test]
fn claude_stdio_golden() {
    let home = tempfile::tempdir().unwrap();
    let entry = installed_entry(
        InstallTarget::Claude,
        &options(McpTransportArg::Stdio),
        home.path(),
    );
    assert_eq!(
        entry,
        serde_json::json!({
            "type": "stdio",
            "command": "zg",
            "args": ["server", "--stdio"],
        })
    );
}

#[test]
fn claude_http_golden() {
    let home = tempfile::tempdir().unwrap();
    let entry = installed_entry(
        InstallTarget::Claude,
        &options(McpTransportArg::Http),
        home.path(),
    );
    assert_eq!(
        entry,
        serde_json::json!({
            "type": "http",
            "url": "http://127.0.0.1:7999",
        })
    );
}

#[test]
fn claude_http_merges_preserving_user_fields() {
    let home = tempfile::tempdir().unwrap();
    seed(
        InstallTarget::Claude,
        home.path(),
        r#"{"mcpServers": {"zvec_grep": {"command": "zg", "args": ["server", "--stdio"], "env": {"ZG_FOO": "bar"}, "custom": 7}}}"#,
    );
    let entry = installed_entry(
        InstallTarget::Claude,
        &options(McpTransportArg::Http),
        home.path(),
    );
    assert_eq!(
        entry,
        serde_json::json!({
            "type": "http",
            "url": "http://127.0.0.1:7999",
            "env": {"ZG_FOO": "bar"},
            "custom": 7,
        })
    );
}

#[test]
fn claude_stdio_merges_preserving_user_fields() {
    let home = tempfile::tempdir().unwrap();
    seed(
        InstallTarget::Claude,
        home.path(),
        r#"{"mcpServers": {"zvec_grep": {"type": "http", "url": "http://127.0.0.1:7999", "env": {"ZG_FOO": "bar"}}}}"#,
    );
    let entry = installed_entry(
        InstallTarget::Claude,
        &options(McpTransportArg::Stdio),
        home.path(),
    );
    assert_eq!(
        entry,
        serde_json::json!({
            "type": "stdio",
            "command": "zg",
            "args": ["server", "--stdio"],
            "env": {"ZG_FOO": "bar"},
        })
    );
}

#[test]
fn claude_http_with_token_keeps_dollar_env_syntax_and_env() {
    let home = tempfile::tempdir().unwrap();
    seed(
        InstallTarget::Claude,
        home.path(),
        r#"{"mcpServers": {"zvec_grep": {"command": "zg", "args": ["server", "--stdio"], "env": {"A": "1"}}}}"#,
    );
    let mut opts = options(McpTransportArg::Http);
    opts.token_env = Some("ZG_TOKEN".to_owned());
    let entry = installed_entry(InstallTarget::Claude, &opts, home.path());
    assert_eq!(
        entry["headers"],
        serde_json::json!({"Authorization": "Bearer ${ZG_TOKEN}"})
    );
    assert_eq!(entry["env"], serde_json::json!({"A": "1"}));
    assert_eq!(entry["type"], serde_json::json!("http"));
}
