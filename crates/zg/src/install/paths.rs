//! Env-aware config-path resolution for install targets (upstream
//! `resolve*ConfigPath` / `resolve*Home` in `cli/install.ts`).
//!
//! Pure helpers take an env lookup so tests never mutate the process
//! environment; the public wrappers read `std::env`.

use std::path::{Path, PathBuf};

use super::InstallTarget;

pub(crate) type EnvLookup<'a> = dyn Fn(&str) -> Option<String> + 'a;

fn env_value(lookup: &EnvLookup<'_>, name: &str) -> Option<String> {
    lookup(name).filter(|value| !value.trim().is_empty())
}

fn env_dir(lookup: &EnvLookup<'_>, name: &str) -> Option<PathBuf> {
    env_value(lookup, name).map(|value| PathBuf::from(value.trim()))
}

fn home_join(home: &Path, segments: &[&str]) -> PathBuf {
    let mut path = home.to_path_buf();
    for segment in segments {
        path.push(segment);
    }
    path
}

/// Files written by install for `target` (Qoder: CLI + IDE; VS Code:
/// one per detected channel profile).
#[must_use]
pub fn install_paths(target: InstallTarget, home: &Path) -> Vec<PathBuf> {
    install_paths_with_env(target, home, &|name| std::env::var(name).ok())
}

pub(crate) fn config_path_with_env(
    target: InstallTarget,
    home: &Path,
    lookup: &EnvLookup<'_>,
) -> PathBuf {
    match install_paths_with_env(target, home, lookup)
        .into_iter()
        .next()
    {
        Some(path) => path,
        None => home_join(home, &[".config"]),
    }
}

pub(crate) fn install_paths_with_env(
    target: InstallTarget,
    home: &Path,
    lookup: &EnvLookup<'_>,
) -> Vec<PathBuf> {
    match target {
        InstallTarget::Claude => vec![claude_path(home, lookup)],
        InstallTarget::Codex => vec![codex_path(home, lookup)],
        InstallTarget::OpenCode => vec![opencode_path(home, lookup)],
        InstallTarget::Cursor => vec![cursor_path(home, lookup)],
        InstallTarget::Qwen => vec![home_join(&qwen_home(home, lookup), &["settings.json"])],
        InstallTarget::Qoder => vec![qoder_cli_path(home, lookup), qoder_ide_path(home, lookup)],
        InstallTarget::Copilot => vec![copilot_path(home, lookup)],
        InstallTarget::VsCode => vscode_paths(home, lookup),
        InstallTarget::Grok => vec![home_join(&grok_home(home, lookup), &["config.toml"])],
    }
}

/// Files scanned by uninstall; `OpenCode` also cleans the non-selected
/// sibling (`.jsonc`/`.json`) unless `OPENCODE_CONFIG` pins one file.
pub(crate) fn uninstall_paths_with_env(
    target: InstallTarget,
    home: &Path,
    lookup: &EnvLookup<'_>,
) -> Vec<PathBuf> {
    if target == InstallTarget::OpenCode && env_dir(lookup, "OPENCODE_CONFIG").is_none() {
        let dir = opencode_dir(home, lookup);
        return vec![dir.join("opencode.jsonc"), dir.join("opencode.json")];
    }
    install_paths_with_env(target, home, lookup)
}

fn claude_path(home: &Path, lookup: &EnvLookup<'_>) -> PathBuf {
    // Upstream: `$CLAUDE_CONFIG_DIR/.claude.json`, else `~/.claude.json`
    // (not `~/.claude/.claude.json`).
    env_dir(lookup, "CLAUDE_CONFIG_DIR")
        .map_or_else(|| home.join(".claude.json"), |dir| dir.join(".claude.json"))
}

fn codex_path(home: &Path, lookup: &EnvLookup<'_>) -> PathBuf {
    let codex_home = env_dir(lookup, "CODEX_HOME").unwrap_or_else(|| home.join(".codex"));
    codex_home.join("config.toml")
}

fn opencode_dir(home: &Path, lookup: &EnvLookup<'_>) -> PathBuf {
    let base = env_dir(lookup, "XDG_CONFIG_HOME").unwrap_or_else(|| home.join(".config"));
    base.join("opencode")
}

fn opencode_path(home: &Path, lookup: &EnvLookup<'_>) -> PathBuf {
    if let Some(file) = env_dir(lookup, "OPENCODE_CONFIG") {
        return file;
    }
    let dir = opencode_dir(home, lookup);
    let jsonc = dir.join("opencode.jsonc");
    // Upstream #107: prefer an existing `.jsonc` over `.json`.
    if jsonc.exists() {
        jsonc
    } else {
        dir.join("opencode.json")
    }
}

fn cursor_path(home: &Path, lookup: &EnvLookup<'_>) -> PathBuf {
    let dir = env_dir(lookup, "CURSOR_CONFIG_DIR").unwrap_or_else(|| home.join(".cursor"));
    dir.join("mcp.json")
}

fn qwen_home(home: &Path, lookup: &EnvLookup<'_>) -> PathBuf {
    env_dir(lookup, "QWEN_HOME").map_or_else(|| home.join(".qwen"), |dir| expand_tilde(&dir, home))
}

fn expand_tilde(path: &Path, home: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    if text == "~" {
        return home.to_path_buf();
    }
    if let Some(rest) = text.strip_prefix("~/").or_else(|| text.strip_prefix("~\\")) {
        return home.join(rest);
    }
    path.to_path_buf()
}

fn qoder_cli_path(home: &Path, lookup: &EnvLookup<'_>) -> PathBuf {
    let dir = env_dir(lookup, "QODER_CONFIG_DIR").unwrap_or_else(|| home.join(".qoder"));
    dir.join("settings.json")
}

fn qoder_ide_path(home: &Path, lookup: &EnvLookup<'_>) -> PathBuf {
    // Upstream resolves the IDE file from the real home, not
    // `QODER_CONFIG_DIR`; `QODER_IDE_MCP_PATH` pins it explicitly.
    env_dir(lookup, "QODER_IDE_MCP_PATH").unwrap_or_else(|| home.join(".qoder").join("mcp.json"))
}

fn copilot_path(home: &Path, lookup: &EnvLookup<'_>) -> PathBuf {
    let dir = env_dir(lookup, "COPILOT_HOME").unwrap_or_else(|| home.join(".copilot"));
    dir.join("mcp-config.json")
}

fn grok_home(home: &Path, lookup: &EnvLookup<'_>) -> PathBuf {
    env_dir(lookup, "GROK_HOME").unwrap_or_else(|| home.join(".grok"))
}

fn vscode_app_data(home: &Path, lookup: &EnvLookup<'_>) -> PathBuf {
    if let Some(dir) = env_dir(lookup, "VSCODE_APPDATA") {
        return dir;
    }
    #[cfg(windows)]
    {
        env_dir(lookup, "APPDATA").unwrap_or_else(|| home.join("AppData").join("Roaming"))
    }
    #[cfg(target_os = "macos")]
    {
        home.join("Library").join("Application Support")
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        env_dir(lookup, "XDG_CONFIG_HOME").unwrap_or_else(|| home.join(".config"))
    }
}

fn vscode_paths(home: &Path, lookup: &EnvLookup<'_>) -> Vec<PathBuf> {
    if let Some(dir) = env_dir(lookup, "VSCODE_USER_DIR") {
        return vec![dir.join("mcp.json")];
    }
    if let Some(portable) = env_dir(lookup, "VSCODE_PORTABLE") {
        return vec![portable.join("user-data").join("User").join("mcp.json")];
    }
    let base = vscode_app_data(home, lookup);
    let channels: [(&str, &str); 2] = [("code", "Code"), ("code-insiders", "Code - Insiders")];
    let mut paths = Vec::new();
    for (executable, product) in channels {
        let user_dir = base.join(product).join("User");
        if super::executable_on_path(executable) || user_dir.exists() {
            paths.push(user_dir.join("mcp.json"));
        }
    }
    if paths.is_empty() {
        paths.push(base.join("Code").join("User").join("mcp.json"));
    }
    paths
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn lookup<'a>(map: &'a HashMap<&'a str, &'a str>) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| map.get(name).map(|value| (*value).to_owned())
    }

    #[test]
    fn env_overrides_win_over_home() {
        let home = Path::new("/home/test");
        let map = HashMap::from([
            ("CLAUDE_CONFIG_DIR", "/cfg/claude"),
            ("CODEX_HOME", "/cfg/codex"),
            ("OPENCODE_CONFIG", "/cfg/oc.jsonc"),
            ("CURSOR_CONFIG_DIR", "/cfg/cursor"),
            ("QODER_CONFIG_DIR", "/cfg/qoder"),
            ("COPILOT_HOME", "/cfg/copilot"),
            ("GROK_HOME", "/cfg/grok"),
        ]);
        let env = lookup(&map);
        assert_eq!(
            config_path_with_env(InstallTarget::Claude, home, &env),
            Path::new("/cfg/claude/.claude.json")
        );
        assert_eq!(
            config_path_with_env(InstallTarget::Codex, home, &env),
            Path::new("/cfg/codex/config.toml")
        );
        assert_eq!(
            config_path_with_env(InstallTarget::OpenCode, home, &env),
            Path::new("/cfg/oc.jsonc")
        );
        assert_eq!(
            config_path_with_env(InstallTarget::Cursor, home, &env),
            Path::new("/cfg/cursor/mcp.json")
        );
        assert_eq!(
            config_path_with_env(InstallTarget::Copilot, home, &env),
            Path::new("/cfg/copilot/mcp-config.json")
        );
        assert_eq!(
            config_path_with_env(InstallTarget::Grok, home, &env),
            Path::new("/cfg/grok/config.toml")
        );
        let qoder = install_paths_with_env(InstallTarget::Qoder, home, &env);
        assert_eq!(
            qoder,
            vec![
                Path::new("/cfg/qoder/settings.json"),
                Path::new("/home/test/.qoder/mcp.json")
            ]
        );
    }

    #[test]
    fn xdg_and_defaults() {
        let home = Path::new("/home/test");
        let map = HashMap::from([("XDG_CONFIG_HOME", "/xdg")]);
        let env = lookup(&map);
        assert_eq!(
            config_path_with_env(InstallTarget::OpenCode, home, &env),
            Path::new("/xdg/opencode/opencode.json")
        );
        let empty = HashMap::new();
        let none = lookup(&empty);
        assert_eq!(
            config_path_with_env(InstallTarget::Claude, home, &none),
            Path::new("/home/test/.claude.json")
        );
        assert_eq!(
            config_path_with_env(InstallTarget::Qoder, home, &none),
            Path::new("/home/test/.qoder/settings.json")
        );
    }
}
