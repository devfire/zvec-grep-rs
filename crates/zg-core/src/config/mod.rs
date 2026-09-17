//! Global config (`~/.zvec-grep/config.json`): schema, IO, and runtime resolution.
//!
//! Ports `engine/config.ts`. JSON field names stay camelCase for
//! byte-compatibility with configs written by the TypeScript implementation.
//! Validation lives in `parse`; this module owns the schema types, file IO
//! (via [`crate::utils::json_io`]), and embedding-runtime resolution.

mod parse;

use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, ToSocketAddrs};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::EngineResult;

pub use parse::{
    invalid_runtime, merge_model_configs, merge_provider_configs, parse_global_config,
};

/// On-disk global config format version.
pub const GLOBAL_CONFIG_VERSION: u32 = 1;

/// Global default selections (`defaults` section).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GlobalDefaults {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_cache_dir: Option<String>,
}

impl GlobalDefaults {
    fn is_empty(&self) -> bool {
        self.embedding.is_none() && self.model_cache_dir.is_none()
    }
}

/// Per-provider credentials (`providers.<name>` section).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
}

impl ProviderConfig {
    fn is_empty(&self) -> bool {
        self.api_key.is_none()
    }
}

/// Device placement for local embedding models.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EmbeddingDevice {
    #[default]
    Auto,
    Cpu,
    Metal,
    Vulkan,
    Cuda,
}

/// Per-model overrides (`models.<provider>/<model>` section).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmbeddingModelConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<EmbeddingDevice>,
}

impl EmbeddingModelConfig {
    fn is_empty(&self) -> bool {
        self.endpoint.is_none() && self.device.is_none()
    }
}

/// Runtime options attached to one embedding reference: explicit call
/// arguments overlaid on the workspace manifest entry.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmbeddingRuntimeConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<EmbeddingDevice>,
}

/// Fully resolved runtime: every fallback layer applied, `api_key` defaulted
/// to `""` like the TypeScript implementation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedEmbeddingRuntimeConfig {
    pub api_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<EmbeddingDevice>,
}

/// Client transport selection (`client` section).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ClientMode {
    Direct,
    Server,
    Auto,
}

/// Client section of the global config.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<ClientMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_url: Option<String>,
}

impl ClientConfig {
    fn is_empty(&self) -> bool {
        self.mode.is_none() && self.server_url.is_none()
    }
}

/// Server section of the global config.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
}

impl ServerConfig {
    fn is_empty(&self) -> bool {
        self.host.is_none() && self.port.is_none()
    }
}

/// Complete global config file body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GlobalConfig {
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub defaults: Option<GlobalDefaults>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub providers: Option<BTreeMap<String, ProviderConfig>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub models: Option<BTreeMap<String, EmbeddingModelConfig>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client: Option<ClientConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<ServerConfig>,
}

impl GlobalConfig {
    /// Empty config: `{ version: 1 }`, returned when no file exists.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            version: GLOBAL_CONFIG_VERSION,
            defaults: None,
            providers: None,
            models: None,
            client: None,
            server: None,
        }
    }
}

/// Partial update accepted by [`update_global_config`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GlobalConfigUpdate {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub defaults: Option<GlobalDefaults>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub providers: Option<BTreeMap<String, ProviderConfig>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub models: Option<BTreeMap<String, EmbeddingModelConfig>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client: Option<ClientConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<ServerConfig>,
}

/// Location of the global config file.
///
/// The TypeScript implementation resolves `~/.zvec-grep/config.json` from the
/// OS home directory; the Rust port routes through
/// [`crate::paths::default_home`] so `$ZVEC_GREP_HOME` overrides keep working.
#[must_use]
pub fn global_config_path() -> PathBuf {
    crate::paths::default_home().join("config.json")
}

/// Reads and validates the global config; returns an empty config when the
/// file does not exist.
///
/// # Errors
///
/// Returns [`EngineError`](crate::error::EngineError) with `CONFIG.INVALID` when the file fails validation, or `JSON.READ_FAILED` when it cannot be read or parsed.
pub fn read_global_config(path: &Path) -> EngineResult<GlobalConfig> {
    let value: serde_json::Value =
        crate::utils::json_io::read_json_file(path, serde_json::Value::Null)?;
    if value.is_null() {
        return Ok(GlobalConfig::empty());
    }
    parse_global_config(&value, &path.display().to_string())
}

/// Merges `update` over the current file content under a write lock and
/// persists the result atomically, mirroring `updateGlobalConfig`.
///
/// # Errors
///
/// Returns [`EngineError`](crate::error::EngineError) with `CONFIG.INVALID` when the merged config fails validation, `LOCK.BUSY` when another writer holds the lock, or `JSON.WRITE_FAILED` when the atomic write fails.
pub fn update_global_config(path: &Path, update: GlobalConfigUpdate) -> EngineResult<GlobalConfig> {
    let lock_path = path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("locks")
        .join("config");
    let guard = crate::utils::lock::acquire_read_write_lock(
        &lock_path,
        crate::utils::lock::LockMode::Write,
        &crate::utils::lock::LockOptions::new("global-config.update"),
    )?;

    let current = read_global_config(path)?;
    let merged = GlobalConfig {
        version: GLOBAL_CONFIG_VERSION,
        defaults: merge_defaults(current.defaults, update.defaults),
        providers: merge_provider_configs(current.providers, update.providers),
        models: merge_model_configs(current.models, update.models),
        client: merge_client(current.client, update.client),
        server: merge_server(current.server, update.server),
    };
    // Round-trip through strict validation so cross-field rules (local-only
    // device, remote-only endpoint, port range) apply to merged content.
    let display = path.display().to_string();
    let value = serde_json::to_value(&merged).map_err(|error| {
        crate::error::EngineError::new(
            crate::error::EngineErrorCode::JsonWriteFailed,
            "failed to serialize global config",
        )
        .with_context(format!("error={error}"))
    })?;
    let next = parse_global_config(&value, &display)?;
    crate::utils::json_io::write_json_file(path, &next, crate::utils::json_io::SECURE_MODES)?;
    guard.release();
    Ok(next)
}

/// Resolves embedding runtime options from the process environment.
///
/// # Errors
///
/// Returns [`EngineError`](crate::error::EngineError) with `CONFIG.INVALID_EMBEDDING_RUNTIME` when `explicit` sets an endpoint on a local model, a device on a remote model, or a non-HTTP(S) endpoint.
pub fn resolve_embedding_runtime_options(
    reference: &str,
    explicit: &EmbeddingRuntimeConfig,
    workspace: &EmbeddingRuntimeConfig,
    config: &GlobalConfig,
) -> EngineResult<ResolvedEmbeddingRuntimeConfig> {
    resolve_embedding_runtime_options_with_env(reference, explicit, workspace, config, &|key| {
        std::env::var(key).ok()
    })
}

/// Resolves embedding runtime options with an injectable environment lookup.
///
/// Precedence per field: explicit call arguments, workspace manifest entry,
/// per-model/per-provider global config, environment, then the default
/// (`""` for `api_key`, `"auto"` for local `device`).
///
/// # Errors
///
/// Returns [`EngineError`](crate::error::EngineError) with `CONFIG.INVALID_EMBEDDING_RUNTIME` when `explicit` sets an endpoint on a local model, a device on a remote model, or a non-HTTP(S) endpoint.
pub fn resolve_embedding_runtime_options_with_env(
    reference: &str,
    explicit: &EmbeddingRuntimeConfig,
    workspace: &EmbeddingRuntimeConfig,
    config: &GlobalConfig,
    env: &impl Fn(&str) -> Option<String>,
) -> EngineResult<ResolvedEmbeddingRuntimeConfig> {
    let provider = provider_from_embedding(reference).map(str::to_owned);
    let local = provider.as_deref() == Some("local");
    if local && explicit.endpoint.is_some() {
        return Err(invalid_runtime(
            reference,
            "endpoint is only supported for remote embedding models",
        ));
    }
    if !local && explicit.device.is_some() {
        return Err(invalid_runtime(
            reference,
            "device is only supported for local embedding models",
        ));
    }

    let model = config
        .models
        .as_ref()
        .and_then(|models| models.get(reference));
    let provider_config = provider.as_deref().and_then(|name| {
        config
            .providers
            .as_ref()
            .and_then(|providers| providers.get(name))
    });

    if !local {
        let endpoint = explicit
            .endpoint
            .clone()
            .or_else(|| workspace.endpoint.clone())
            .or_else(|| model.and_then(|model| model.endpoint.clone()))
            .or_else(|| non_empty_env(env("ZVEC_GREP_ENDPOINT").as_deref()));
        if let Some(endpoint) = &endpoint
            && !is_http_endpoint(endpoint)
        {
            return Err(invalid_runtime(
                reference,
                "endpoint must be a valid HTTP(S) URL",
            ));
        }
        let api_key = explicit
            .api_key
            .clone()
            .or_else(|| workspace.api_key.clone())
            .or_else(|| provider_config.and_then(|config| config.api_key.clone()))
            .or_else(|| environment_api_key(provider.as_deref(), env))
            .unwrap_or_default();
        return Ok(ResolvedEmbeddingRuntimeConfig {
            api_key,
            endpoint,
            device: None,
        });
    }

    let device = explicit
        .device
        .or(workspace.device)
        .or_else(|| model.and_then(|model| model.device))
        .unwrap_or_else(|| environment_device(env));
    let api_key = explicit
        .api_key
        .clone()
        .or_else(|| workspace.api_key.clone())
        .or_else(|| provider_config.and_then(|config| config.api_key.clone()))
        .or_else(|| environment_api_key(provider.as_deref(), env))
        .unwrap_or_default();
    Ok(ResolvedEmbeddingRuntimeConfig {
        api_key,
        endpoint: None,
        device: Some(device),
    })
}

/// Extracts the provider segment (`"local"` from `"local/bge-m3"`).
/// Returns `None` when there is no `/` or it leads the reference.
#[must_use]
pub fn provider_from_embedding(reference: &str) -> Option<&str> {
    match reference.find('/') {
        Some(index) if index > 0 => Some(&reference[..index]),
        _ => None,
    }
}

/// Accepts only `http:`/`https:` URLs whose host is a public routable
/// address, mirroring the TS `isHttpEndpoint` plus SSRF hardening.
///
/// Rejects userinfo credentials, `localhost` names, loopback /
/// link-local / multicast / reserved hosts, and the well-known cloud
/// metadata endpoints (`169.254.169.254`, `fd00:ec2::254`,
/// `100.100.100.254`). Hostnames are resolved and every resolved IP is
/// re-checked, failing closed when resolution fails or yields nothing.
#[must_use]
pub fn is_http_endpoint(value: &str) -> bool {
    let Ok(url) = url::Url::parse(value) else {
        return false;
    };
    if !matches!(url.scheme(), "http" | "https") {
        return false;
    }
    if !url.username().is_empty() || url.password().is_some() {
        return false;
    }
    let Some(host) = url.host_str() else {
        return false;
    };
    if host.is_empty() || is_blocked_hostname(host) {
        return false;
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        return !is_blocked_ip(&ip);
    }
    let port = url.port_or_known_default().unwrap_or(443);
    let joint = format!("{host}:{port}");
    let Ok(addrs) = joint.as_str().to_socket_addrs() else {
        return false;
    };
    let mut resolved_any = false;
    for addr in addrs {
        resolved_any = true;
        if is_blocked_ip(&addr.ip()) {
            return false;
        }
    }
    resolved_any
}

/// True for hostnames that must never be dialed: `localhost` (plus any
/// sub-domain and the trailing-dot FQDN root form) and the well-known
/// cloud metadata hostnames.
fn is_blocked_hostname(host: &str) -> bool {
    let trimmed = host.strip_suffix('.').unwrap_or(host);
    if trimmed.eq_ignore_ascii_case("localhost") {
        return true;
    }
    let lower = trimmed.to_lowercase();
    if lower.ends_with(".localhost") {
        return true;
    }
    matches!(
        lower.as_str(),
        "metadata.google.internal"
            | "metadata.google"
            | "instance-data"
            | "instance-data.compute.internal"
    )
}

/// True for IPs that must never be dialed: loopback, link-local,
/// multicast, unspecified, private/reserved ranges, and the well-known
/// cloud metadata endpoints. IPv4-mapped IPv6 addresses inherit the IPv4
/// policy for their embedded address.
fn is_blocked_ip(ip: &IpAddr) -> bool {
    if ip.is_loopback() || ip.is_multicast() || ip.is_unspecified() {
        return true;
    }
    match ip {
        IpAddr::V4(addr) => is_blocked_ipv4(addr),
        IpAddr::V6(addr) => is_blocked_ipv6(addr),
    }
}

/// IPv4 SSRF policy as explicit octet ranges (no reliance on
/// version-gated std helpers): private, link-local, multicast,
/// special-use/reserved, and the metadata endpoints.
fn is_blocked_ipv4(addr: &Ipv4Addr) -> bool {
    if *addr == Ipv4Addr::new(169, 254, 169, 254) || *addr == Ipv4Addr::new(100, 100, 100, 254) {
        return true;
    }
    let [a, b, c, d] = addr.octets();
    if a == 127 {
        return true;
    }
    if a == 10 || (a == 172 && (16..=31).contains(&b)) || (a == 192 && b == 168) {
        return true;
    }
    if a == 169 && b == 254 {
        return true;
    }
    if (224..=239).contains(&a) {
        return true;
    }
    if a == 0 {
        return true;
    }
    if a == 100 && (64..=127).contains(&b) {
        return true;
    }
    if a == 192 && b == 0 && (c == 0 || c == 2) {
        return true;
    }
    if a == 192 && b == 88 && c == 99 {
        return true;
    }
    if a == 198 && (18..=19).contains(&b) {
        return true;
    }
    if a == 198 && b == 51 && c == 100 {
        return true;
    }
    if a == 203 && b == 0 && c == 113 {
        return true;
    }
    if a >= 240 {
        return true;
    }
    a == 255 && b == 255 && c == 255 && d == 255
}

/// IPv6 SSRF policy as explicit segment ranges: multicast, link-local,
/// unique-local, documentation, and the EC2 metadata endpoint.
fn is_blocked_ipv6(addr: &Ipv6Addr) -> bool {
    if *addr == Ipv6Addr::new(0xfd00, 0x0ec2, 0, 0, 0, 0, 0, 0x0254) {
        return true;
    }
    let [s0, s1, s2, s3, s4, s5, s6, s7] = addr.segments();
    if (s0 & 0xff00) == 0xff00 {
        return true;
    }
    if (s0 & 0xffc0) == 0xfe80 {
        return true;
    }
    if (s0 & 0xfe00) == 0xfc00 {
        return true;
    }
    if s0 == 0x2001 && s1 == 0x0db8 {
        return true;
    }
    if s0 == 0 && s1 == 0 && s2 == 0 && s3 == 0 && s4 == 0 && s5 == 0xffff {
        return is_blocked_ipv4(&Ipv4Addr::new(
            (s6 >> 8) as u8,
            (s6 & 0xff) as u8,
            (s7 >> 8) as u8,
            (s7 & 0xff) as u8,
        ));
    }
    false
}

fn merge_defaults(
    current: Option<GlobalDefaults>,
    update: Option<GlobalDefaults>,
) -> Option<GlobalDefaults> {
    let (current, update) = match (current, update) {
        (None, None) => return None,
        (current, update) => (current.unwrap_or_default(), update.unwrap_or_default()),
    };
    let merged = GlobalDefaults {
        embedding: update.embedding.or(current.embedding),
        model_cache_dir: update.model_cache_dir.or(current.model_cache_dir),
    };
    if merged.is_empty() {
        None
    } else {
        Some(merged)
    }
}

fn merge_client(
    current: Option<ClientConfig>,
    update: Option<ClientConfig>,
) -> Option<ClientConfig> {
    let (current, update) = match (current, update) {
        (None, None) => return None,
        (current, update) => (current.unwrap_or_default(), update.unwrap_or_default()),
    };
    let merged = ClientConfig {
        mode: update.mode.or(current.mode),
        server_url: update.server_url.or(current.server_url),
    };
    if merged.is_empty() {
        None
    } else {
        Some(merged)
    }
}

fn merge_server(
    current: Option<ServerConfig>,
    update: Option<ServerConfig>,
) -> Option<ServerConfig> {
    let (current, update) = match (current, update) {
        (None, None) => return None,
        (current, update) => (current.unwrap_or_default(), update.unwrap_or_default()),
    };
    let merged = ServerConfig {
        host: update.host.or(current.host),
        port: update.port.or(current.port),
    };
    if merged.is_empty() {
        None
    } else {
        Some(merged)
    }
}

fn environment_api_key(
    provider: Option<&str>,
    env: &impl Fn(&str) -> Option<String>,
) -> Option<String> {
    if provider == Some("qwen") {
        non_empty_env(env("ZVEC_GREP_API_KEY").as_deref())
            .or_else(|| non_empty_env(env("DASHSCOPE_API_KEY").as_deref()))
            .or_else(|| non_empty_env(env("QWEN_API_KEY").as_deref()))
    } else {
        non_empty_env(env("ZVEC_GREP_API_KEY").as_deref())
    }
}

fn non_empty_env(value: Option<&str>) -> Option<String> {
    let trimmed = value?.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

fn environment_device(env: &impl Fn(&str) -> Option<String>) -> EmbeddingDevice {
    match env("ZVEC_GREP_DEVICE")
        .as_deref()
        .map(|value| value.trim().to_lowercase())
    {
        Some(device) if device == "cpu" => EmbeddingDevice::Cpu,
        Some(device) if device == "metal" => EmbeddingDevice::Metal,
        Some(device) if device == "vulkan" => EmbeddingDevice::Vulkan,
        Some(device) if device == "cuda" => EmbeddingDevice::Cuda,
        _ => EmbeddingDevice::Auto,
    }
}
