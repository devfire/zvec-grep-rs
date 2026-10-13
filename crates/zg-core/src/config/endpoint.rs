//! Endpoint validation split by call-site risk.
//!
//! Config parsing uses only the syntactic check ([`is_http_endpoint`]): it
//! must never perform DNS resolution or reject private / loopback /
//! benchmarking-range addresses, because a config that merely *mentions* a
//! LAN, fake-ip (Clash/Surge `198.18.0.0/15`), or temporarily unresolvable
//! endpoint must still load so unrelated local commands keep working.
//!
//! The strict SSRF policy ([`is_allowed_request_endpoint`] with
//! `allow_private_endpoint == false`) is applied only where an actual
//! outbound request is about to be made. Setting
//! `allowPrivateEndpoint` in the global config, or the
//! `ZVEC_GREP_ALLOW_PRIVATE_ENDPOINT` environment variable, explicitly
//! opts a trusted private/self-hosted endpoint out of the IP-range and
//! DNS checks; the syntactic HTTP(S) check and the userinfo rejection
//! still apply.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, ToSocketAddrs};

/// Environment variable explicitly allowing private/self-hosted remote
/// embedding endpoints at request time (`"1"`, `"true"`, or `"yes"`).
/// This weakens SSRF protection for those endpoints and must only be set
/// for endpoints the operator controls.
pub const ALLOW_PRIVATE_ENDPOINT_ENV_VAR: &str = "ZVEC_GREP_ALLOW_PRIVATE_ENDPOINT";

/// Parses `value` as an HTTP(S) URL, returning `None` for unparseable
/// values, non-HTTP(S) schemes, and missing/empty hosts.
fn parse_http_url(value: &str) -> Option<url::Url> {
    let url = url::Url::parse(value).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    match url.host_str() {
        Some(host) if !host.is_empty() => Some(url),
        _ => None,
    }
}

/// Syntactic endpoint check used on the config-parse path: `value` must be
/// a parseable `http:`/`https:` URL with a non-empty host.
///
/// This performs no DNS resolution and no IP-range rejection, so offline
/// use, DNS hiccups, fake-ip proxy ranges, and LAN self-hosted endpoints
/// never make an otherwise valid config unloadable.
#[must_use]
pub fn is_http_endpoint(value: &str) -> bool {
    parse_http_url(value).is_some()
}

/// Strict request-time SSRF check: syntactic validity plus rejection of
/// userinfo credentials, `localhost` names, loopback / link-local /
/// multicast / reserved hosts, and the well-known cloud metadata
/// endpoints (`169.254.169.254`, `fd00:ec2::254`, `100.100.100.254`).
/// Hostnames are resolved and every resolved IP is re-checked, failing
/// closed when resolution fails or yields nothing.
#[must_use]
pub fn is_public_http_endpoint(value: &str) -> bool {
    is_allowed_request_endpoint(value, false)
}

/// Request-time endpoint guard for outbound remote-provider requests.
///
/// With `allow_private_endpoint == false` this is the strict SSRF policy
/// of [`is_public_http_endpoint`]. With `true` — an explicit operator
/// opt-out for trusted private/self-hosted endpoints — only the
/// syntactic HTTP(S) check and the userinfo rejection apply, and no DNS
/// resolution is performed.
#[must_use]
pub fn is_allowed_request_endpoint(value: &str, allow_private_endpoint: bool) -> bool {
    let Some(url) = parse_http_url(value) else {
        return false;
    };
    if !url.username().is_empty() || url.password().is_some() {
        return false;
    }
    let Some(host) = url.host_str() else {
        return false;
    };
    if allow_private_endpoint {
        return true;
    }
    if is_blocked_hostname(host) {
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

/// Explicit private-endpoint opt-out from the process environment
/// (`ZVEC_GREP_ALLOW_PRIVATE_ENDPOINT`). Unset or unrecognised values
/// keep the strict request-time policy.
#[must_use]
pub fn allow_private_endpoint_from_env() -> bool {
    parse_allow_private_env(
        std::env::var(ALLOW_PRIVATE_ENDPOINT_ENV_VAR)
            .ok()
            .as_deref(),
    )
    .unwrap_or(false)
}

/// Parses an opt-out flag value: `Some(true)`/`Some(false)` for recognised
/// spellings, `None` for unset/unrecognised values (caller defaults to
/// the strict policy).
pub(crate) fn parse_allow_private_env(value: Option<&str>) -> Option<bool> {
    match value?.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" => Some(true),
        "0" | "false" | "no" => Some(false),
        _ => None,
    }
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
        IpAddr::V4(addr) => is_blocked_ipv4(*addr),
        IpAddr::V6(addr) => is_blocked_ipv6(addr),
    }
}

/// IPv4 SSRF policy as explicit octet ranges (no reliance on
/// version-gated std helpers): private, link-local, multicast,
/// special-use/reserved, and the metadata endpoints.
fn is_blocked_ipv4(addr: Ipv4Addr) -> bool {
    if addr == Ipv4Addr::new(169, 254, 169, 254) || addr == Ipv4Addr::new(100, 100, 100, 254) {
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
        return is_blocked_ipv4(Ipv4Addr::new(
            (s6 >> 8) as u8,
            (s6 & 0xff) as u8,
            (s7 >> 8) as u8,
            (s7 & 0xff) as u8,
        ));
    }
    false
}

#[cfg(test)]
mod tests {
    use super::{
        ALLOW_PRIVATE_ENDPOINT_ENV_VAR, is_allowed_request_endpoint, is_http_endpoint,
        is_public_http_endpoint, parse_allow_private_env,
    };
    use crate::config::{
        EmbeddingModelConfig, EmbeddingRuntimeConfig, GlobalConfig, parse_global_config,
        resolve_embedding_runtime_options_with_env,
    };
    use std::collections::BTreeMap;

    fn config_with_endpoint(endpoint: &str) -> serde_json::Value {
        serde_json::json!({
            "version": 1,
            "models": {
                "qwen/text-embedding-v4": { "endpoint": endpoint }
            }
        })
    }

    #[test]
    fn config_parse_accepts_unresolvable_private_and_fake_ip_endpoints() {
        // `.invalid` never resolves, the LAN address needs no DNS, and
        // 198.18.0.0/15 is the Clash/Surge fake-ip range: all must load
        // offline on the parse path.
        for endpoint in [
            "http://nonexistent.invalid/v1",
            "http://192.168.1.10:11434/v1",
            "http://198.18.0.5/v1",
        ] {
            let parsed = parse_global_config(&config_with_endpoint(endpoint), "config.json");
            assert!(parsed.is_ok(), "{endpoint}: {parsed:?}");
            assert!(is_http_endpoint(endpoint), "{endpoint}");
        }
    }

    #[test]
    fn request_guard_blocks_private_by_default_and_allows_explicit_opt_out() {
        for endpoint in [
            "http://192.168.1.10:11434/v1",
            "http://198.18.0.5/v1",
            "http://127.0.0.1:11434/v1",
            "http://169.254.169.254/",
            "http://nonexistent.invalid/v1",
        ] {
            assert!(!is_public_http_endpoint(endpoint), "{endpoint}");
            assert!(!is_allowed_request_endpoint(endpoint, false), "{endpoint}");
            assert!(is_allowed_request_endpoint(endpoint, true), "{endpoint}");
        }
        // The opt-out never weakens scheme or userinfo validation.
        assert!(!is_allowed_request_endpoint("ftp://192.168.1.10/v1", true));
        assert!(!is_allowed_request_endpoint(
            "http://user:pass@192.168.1.10/v1",
            true
        ));
        // Literal public IPs need no DNS, so this stays offline-safe.
        assert!(is_public_http_endpoint("http://8.8.8.8/v1"));
    }

    #[test]
    fn allow_private_env_parsing_defaults_to_strict() {
        assert_eq!(parse_allow_private_env(Some("true")), Some(true));
        assert_eq!(parse_allow_private_env(Some(" 1 ")), Some(true));
        assert_eq!(parse_allow_private_env(Some("false")), Some(false));
        assert_eq!(parse_allow_private_env(Some("bogus")), None);
        assert_eq!(parse_allow_private_env(None), None);
    }

    #[test]
    fn config_parse_reads_allow_private_endpoint_flag() {
        let value = serde_json::json!({
            "version": 1,
            "models": {
                "qwen/text-embedding-v4": {
                    "endpoint": "http://192.168.1.10:11434/v1",
                    "allowPrivateEndpoint": true
                }
            }
        });
        let parsed = match parse_global_config(&value, "config.json") {
            Ok(parsed) => parsed,
            Err(err) => panic!("flagged private endpoint must parse: {err}"),
        };
        let model = parsed
            .models
            .as_ref()
            .and_then(|models| models.get("qwen/text-embedding-v4"));
        assert_eq!(
            model.and_then(|model| model.allow_private_endpoint),
            Some(true)
        );
        let bad = serde_json::json!({
            "version": 1,
            "models": {
                "qwen/text-embedding-v4": { "allowPrivateEndpoint": "yes" }
            }
        });
        assert!(parse_global_config(&bad, "config.json").is_err());
    }

    #[test]
    fn resolved_runtime_carries_config_and_env_private_opt_out() {
        let mut config = GlobalConfig::empty();
        config.models = Some(BTreeMap::from([(
            "qwen/text-embedding-v4".to_owned(),
            EmbeddingModelConfig {
                endpoint: Some("http://192.168.1.10:11434/v1".to_owned()),
                device: None,
                allow_private_endpoint: Some(true),
            },
        )]));
        let resolved = resolve_embedding_runtime_options_with_env(
            "qwen/text-embedding-v4",
            &EmbeddingRuntimeConfig::default(),
            &EmbeddingRuntimeConfig::default(),
            &config,
            &|_| None,
        );
        let resolved = match resolved {
            Ok(resolved) => resolved,
            Err(err) => panic!("private endpoint must resolve: {err}"),
        };
        assert!(resolved.allow_private_endpoint);
        assert_eq!(
            resolved.endpoint.as_deref(),
            Some("http://192.168.1.10:11434/v1")
        );

        // Env override enables the opt-out when the config does not.
        let mut config = GlobalConfig::empty();
        config.models = Some(BTreeMap::from([(
            "qwen/text-embedding-v4".to_owned(),
            EmbeddingModelConfig {
                endpoint: Some("http://198.18.0.5/v1".to_owned()),
                device: None,
                allow_private_endpoint: None,
            },
        )]));
        let resolved = resolve_embedding_runtime_options_with_env(
            "qwen/text-embedding-v4",
            &EmbeddingRuntimeConfig::default(),
            &EmbeddingRuntimeConfig::default(),
            &config,
            &|key| (key == ALLOW_PRIVATE_ENDPOINT_ENV_VAR).then(|| "true".to_owned()),
        );
        let resolved = match resolved {
            Ok(resolved) => resolved,
            Err(err) => panic!("env opt-out must resolve: {err}"),
        };
        assert!(resolved.allow_private_endpoint);
    }
}
