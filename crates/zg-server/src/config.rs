//! Daemon configuration: listen addresses, token paths, token resolution.
//!
//! Mirrors `../zvec-grep/src/daemon/config.ts` (`DEFAULT_SERVER_HOST` /
//! `DEFAULT_SERVER_PORT`, `daemonHome` / `daemonTokenPath`,
//! `parseListenAddress` / `isLoopbackHost`, `resolveServerToken` /
//! `resolveClientToken`). `daemonHome` reuses
//! [`crate::logger::daemon_home`]; file reads are sync (no tokio in these
//! helpers — the HTTP server calls them before it starts).

use std::path::PathBuf;

use crate::errors::DaemonError;

/// Default listen host: loopback only (mirrors TS `DEFAULT_SERVER_HOST`).
pub const DEFAULT_SERVER_HOST: &str = "127.0.0.1";

/// Default listen port (mirrors TS `DEFAULT_SERVER_PORT`).
pub const DEFAULT_SERVER_PORT: u16 = 7999;

/// Minimum server token length in chars (mirrors TS `validateToken`).
pub const MIN_SERVER_TOKEN_LEN: usize = 32;

/// Server token from the environment.
pub const SERVER_TOKEN_ENV: &str = "ZVEC_GREP_SERVER_TOKEN";

/// Server token file from the environment.
pub const SERVER_TOKEN_FILE_ENV: &str = "ZVEC_GREP_SERVER_TOKEN_FILE";

/// Parsed `host:port` listen address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerListenAddress {
    /// Listen host (loopback only).
    pub host: String,
    /// Listen port.
    pub port: u16,
}

impl ServerListenAddress {
    /// `host:port` display form.
    pub fn display(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

/// Daemon server config: host + port with loopback enforcement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerConfig {
    /// Listen host.
    pub host: String,
    /// Listen port.
    pub port: u16,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: DEFAULT_SERVER_HOST.to_owned(),
            port: DEFAULT_SERVER_PORT,
        }
    }
}

impl ServerConfig {
    /// `host:port` display form.
    pub fn listen(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

/// Token file path: `<daemon-home>/token` (mirrors TS `daemonTokenPath`).
pub fn daemon_token_path(home: Option<&Path>) -> PathBuf {
    let base = home.map_or_else(crate::logger::daemon_home, Path::to_path_buf);
    base.join("token")
}

use std::path::Path;

/// True for `127.0.0.1`, `::1`, and `localhost` (case-insensitive),
/// mirroring TS `isLoopbackHost`.
pub fn is_loopback_host(host: &str) -> bool {
    matches!(
        host.to_lowercase().as_str(),
        "127.0.0.1" | "::1" | "localhost"
    )
}

/// Parses `host:port`, enforcing loopback hosts and port range, mirroring
/// TS `parseListenAddress` (including bracket stripping for `[::1]`).
pub fn parse_listen_address(value: Option<&str>) -> Result<ServerListenAddress, DaemonError> {
    let listen = value
        .unwrap_or(&format!("{}:{}", DEFAULT_SERVER_HOST, DEFAULT_SERVER_PORT))
        .to_owned();
    let separator = listen.rfind(':').unwrap_or(0);
    if separator == 0 || separator == listen.len() - 1 {
        return Err(DaemonError::InvalidListenAddress { value: listen });
    }
    let raw_host = &listen[..separator];
    let host = raw_host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(raw_host);
    let port: u32 = listen[separator + 1..].parse().unwrap_or(0);
    if !is_loopback_host(host) {
        return Err(DaemonError::LoopbackRequired {
            host: host.to_owned(),
        });
    }
    if !(1..=65_535).contains(&port) {
        return Err(DaemonError::InvalidListenAddress { value: listen });
    }
    Ok(ServerListenAddress {
        host: host.to_owned(),
        port: port as u16,
    })
}

/// Reads the global config's server section for the listen address,
/// defaulting to loopback:7999 (mirrors TS `configuredListenAddress`).
pub fn configured_listen_address(listen: Option<&str>) -> Result<ServerListenAddress, DaemonError> {
    if let Some(explicit) = listen {
        return parse_listen_address(Some(explicit));
    }
    let path = zg_core::config::global_config_path();
    let config = zg_core::config::read_global_config(&path)
        .unwrap_or_else(|_| zg_core::config::GlobalConfig::empty());
    let host = config
        .server
        .as_ref()
        .and_then(|server| server.host.clone())
        .unwrap_or_else(|| DEFAULT_SERVER_HOST.to_owned());
    let port = config
        .server
        .as_ref()
        .and_then(|server| server.port)
        .unwrap_or(DEFAULT_SERVER_PORT);
    parse_listen_address(Some(&format!("{host}:{port}")))
}

/// Server-side token: explicit value (or env) wins, else the token file.
/// Files are chmodded `0600` after reading, mirroring TS.
pub fn resolve_server_token(
    token: Option<String>,
    token_file: Option<PathBuf>,
) -> Result<Option<String>, DaemonError> {
    if let Some(explicit) = token.or_else(|| std::env::var(SERVER_TOKEN_ENV).ok()) {
        validate_token(&explicit)?;
        return Ok(Some(explicit));
    }
    let path = token_file.or_else(|| std::env::var(SERVER_TOKEN_FILE_ENV).ok().map(PathBuf::from));
    let Some(path) = path else {
        return Ok(None);
    };
    let raw = std::fs::read_to_string(&path).map_err(|_| DaemonError::InvalidToken)?;
    let found = raw.trim().to_owned();
    validate_token(&found)?;
    chmod_owner_only(&path);
    Ok(Some(found))
}

/// Client-side token: env first, then the explicit/default daemon token
/// file. A missing default file means "no token" (`Ok(None)`); a missing
/// explicit file is an error (mirrors TS `resolveClientToken`).
pub fn resolve_client_token(
    token_file: Option<PathBuf>,
    home: Option<&Path>,
) -> Result<Option<String>, DaemonError> {
    if let Ok(explicit) = std::env::var(SERVER_TOKEN_ENV) {
        validate_token(&explicit)?;
        return Ok(Some(explicit));
    }
    let configured =
        token_file.or_else(|| std::env::var(SERVER_TOKEN_FILE_ENV).ok().map(PathBuf::from));
    let path = configured
        .clone()
        .unwrap_or_else(|| daemon_token_path(home));
    match std::fs::read_to_string(&path) {
        Ok(raw) => {
            let found = raw.trim().to_owned();
            validate_token(&found)?;
            Ok(Some(found))
        }
        Err(_) if configured.is_none() => Ok(None),
        Err(_) => Err(DaemonError::InvalidToken),
    }
}

fn validate_token(token: &str) -> Result<(), DaemonError> {
    if token.chars().count() < MIN_SERVER_TOKEN_LEN {
        return Err(DaemonError::InvalidToken);
    }
    Ok(())
}

fn chmod_owner_only(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(metadata) = std::fs::metadata(path) {
            let mut permissions = metadata.permissions();
            permissions.set_mode(0o600);
            let _ = std::fs::set_permissions(path, permissions);
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_loopback() {
        let address = parse_listen_address(None).unwrap();
        assert_eq!(address.host, "127.0.0.1");
        assert_eq!(address.port, 7999);
        assert!(is_loopback_host("localhost"));
        assert!(is_loopback_host("::1"));
        assert!(!is_loopback_host("0.0.0.0"));
    }

    #[test]
    fn rejects_non_loopback_and_bad_ports() {
        assert!(matches!(
            parse_listen_address(Some("0.0.0.0:7999")),
            Err(DaemonError::LoopbackRequired { .. })
        ));
        assert!(matches!(
            parse_listen_address(Some("127.0.0.1:0")),
            Err(DaemonError::InvalidListenAddress { .. })
        ));
        assert!(matches!(
            parse_listen_address(Some("no-port")),
            Err(DaemonError::InvalidListenAddress { .. })
        ));
        assert_eq!(
            parse_listen_address(Some("[::1]:7999")).unwrap().host,
            "::1"
        );
    }

    #[test]
    fn short_tokens_are_invalid() {
        assert!(matches!(
            resolve_server_token(Some("short".to_owned()), None),
            Err(DaemonError::InvalidToken)
        ));
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("token");
        std::fs::write(&file, format!("  {}  \n", "x".repeat(40))).unwrap();
        let resolved = resolve_server_token(None, Some(file)).unwrap().unwrap();
        assert_eq!(resolved.len(), 40);
    }
}
