//! Strict JSON -> config parsing with field validation.
//!
//! Ports the `parse*`/`merge*`/`optional*` helpers of `engine/config.ts`. All
//! entry points take a `serde_json::Value` so unknown fields and wrong JSON
//! shapes can be rejected with [`EngineError`] instead of failing serde
//! derivation.

use std::collections::BTreeMap;

use crate::config::{
    ClientConfig, ClientMode, EmbeddingDevice, EmbeddingModelConfig, GLOBAL_CONFIG_VERSION,
    GlobalConfig, GlobalDefaults, ProviderConfig, ServerConfig,
};
use crate::error::{EngineError, EngineResult, codes};
use serde_json::Value;

/// Validates `value` as a complete global config file body.
///
/// # Errors
///
/// Returns [`EngineError`] with [`codes::config_invalid()`] when any field fails validation.
pub fn parse_global_config(value: &Value, path: &str) -> EngineResult<GlobalConfig> {
    let object = value
        .as_object()
        .ok_or_else(|| invalid_config(path, "version must be 1"))?;
    if object.get("version").and_then(Value::as_u64) != Some(u64::from(GLOBAL_CONFIG_VERSION)) {
        return Err(invalid_config(path, "version must be 1"));
    }
    assert_known_fields(
        object,
        &[
            "version",
            "defaults",
            "providers",
            "models",
            "client",
            "server",
        ],
        path,
        "config",
    )?;

    Ok(GlobalConfig {
        version: GLOBAL_CONFIG_VERSION,
        defaults: parse_defaults(object.get("defaults"), path)?,
        providers: parse_providers(object.get("providers"), path)?,
        models: parse_models(object.get("models"), path)?,
        client: parse_client(object.get("client"), path)?,
        server: parse_server(object.get("server"), path)?,
    })
}

/// Field-level merge of per-provider configs (`apiKey` only, today).
#[must_use]
pub fn merge_provider_configs(
    current: Option<BTreeMap<String, ProviderConfig>>,
    update: Option<BTreeMap<String, ProviderConfig>>,
) -> Option<BTreeMap<String, ProviderConfig>> {
    let (current, update) = match (current, update) {
        (None, None) => return None,
        (current, update) => (current, update),
    };
    let mut merged = current.unwrap_or_default();
    for (provider, config) in update.unwrap_or_default() {
        let entry = merged.entry(provider).or_default();
        if config.api_key.is_some() {
            entry.api_key = config.api_key;
        }
    }
    Some(merged)
}

/// Field-level merge of per-model configs (`endpoint`/`device`).
#[must_use]
pub fn merge_model_configs(
    current: Option<BTreeMap<String, EmbeddingModelConfig>>,
    update: Option<BTreeMap<String, EmbeddingModelConfig>>,
) -> Option<BTreeMap<String, EmbeddingModelConfig>> {
    let (current, update) = match (current, update) {
        (None, None) => return None,
        (current, update) => (current, update),
    };
    let mut merged = current.unwrap_or_default();
    for (reference, config) in update.unwrap_or_default() {
        let entry = merged.entry(reference).or_default();
        if config.endpoint.is_some() {
            entry.endpoint = config.endpoint;
        }
        if config.device.is_some() {
            entry.device = config.device;
        }
    }
    Some(merged)
}

fn parse_defaults(value: Option<&Value>, path: &str) -> EngineResult<Option<GlobalDefaults>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let object = value
        .as_object()
        .ok_or_else(|| invalid_config(path, "defaults must be an object"))?;
    assert_known_fields(object, &["embedding", "modelCacheDir"], path, "defaults")?;
    let defaults = GlobalDefaults {
        embedding: optional_non_empty_string(object.get("embedding"), path, "defaults.embedding")?,
        model_cache_dir: optional_non_empty_string(
            object.get("modelCacheDir"),
            path,
            "defaults.modelCacheDir",
        )?,
    };
    Ok(if defaults.is_empty() {
        None
    } else {
        Some(defaults)
    })
}

fn parse_providers(
    value: Option<&Value>,
    path: &str,
) -> EngineResult<Option<BTreeMap<String, ProviderConfig>>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let object = value
        .as_object()
        .ok_or_else(|| invalid_config(path, "providers must be an object"))?;
    let mut providers = BTreeMap::new();
    for (provider, item) in object {
        let Some(entry) = item.as_object() else {
            return Err(invalid_config(
                path,
                &format!("providers.{provider} must be an object with a valid provider name"),
            ));
        };
        if !is_valid_provider_name(provider) {
            return Err(invalid_config(
                path,
                &format!("providers.{provider} must be an object with a valid provider name"),
            ));
        }
        assert_known_fields(entry, &["apiKey"], path, &format!("providers.{provider}"))?;
        let config = ProviderConfig {
            api_key: optional_non_empty_string(
                entry.get("apiKey"),
                path,
                &format!("providers.{provider}.apiKey"),
            )?,
        };
        if !config.is_empty() {
            providers.insert(provider.clone(), config);
        }
    }
    Ok(if providers.is_empty() {
        None
    } else {
        Some(providers)
    })
}

fn parse_models(
    value: Option<&Value>,
    path: &str,
) -> EngineResult<Option<BTreeMap<String, EmbeddingModelConfig>>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let object = value
        .as_object()
        .ok_or_else(|| invalid_config(path, "models must be an object"))?;
    let mut models = BTreeMap::new();
    for (reference, item) in object {
        let Some(entry) = item.as_object() else {
            return Err(invalid_config(
                path,
                &format!("models.{reference} must be an object with a valid embedding reference"),
            ));
        };
        if !is_valid_model_reference(reference) {
            return Err(invalid_config(
                path,
                &format!("models.{reference} must be an object with a valid embedding reference"),
            ));
        }
        assert_known_fields(
            entry,
            &["endpoint", "device"],
            path,
            &format!("models.{reference}"),
        )?;
        let endpoint = optional_non_empty_string(
            entry.get("endpoint"),
            path,
            &format!("models.{reference}.endpoint"),
        )?;
        if let Some(endpoint) = &endpoint
            && !crate::config::is_http_endpoint(endpoint)
        {
            return Err(invalid_config(
                path,
                &format!("models.{reference}.endpoint must be a valid HTTP(S) URL"),
            ));
        }
        let device = optional_device(
            entry.get("device"),
            path,
            &format!("models.{reference}.device"),
        )?;
        let local = reference.starts_with("local/");
        if local && endpoint.is_some() {
            return Err(invalid_config(
                path,
                &format!("models.{reference}.endpoint is only supported for remote models"),
            ));
        }
        if !local && device.is_some() {
            return Err(invalid_config(
                path,
                &format!("models.{reference}.device is only supported for local models"),
            ));
        }
        let config = EmbeddingModelConfig { endpoint, device };
        if !config.is_empty() {
            models.insert(reference.clone(), config);
        }
    }
    Ok(if models.is_empty() {
        None
    } else {
        Some(models)
    })
}

fn parse_client(value: Option<&Value>, path: &str) -> EngineResult<Option<ClientConfig>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let object = value
        .as_object()
        .ok_or_else(|| invalid_config(path, "client must be an object"))?;
    assert_known_fields(object, &["mode", "serverUrl"], path, "client")?;
    let mode = match object.get("mode") {
        None | Some(Value::Null) => None,
        Some(Value::String(mode)) => match mode.as_str() {
            "direct" => Some(ClientMode::Direct),
            "server" => Some(ClientMode::Server),
            "auto" => Some(ClientMode::Auto),
            _ => {
                return Err(invalid_config(
                    path,
                    "client.mode must be direct, server, or auto",
                ));
            }
        },
        Some(_) => {
            return Err(invalid_config(
                path,
                "client.mode must be direct, server, or auto",
            ));
        }
    };
    let server_url = optional_non_empty_string(object.get("serverUrl"), path, "client.serverUrl")?;
    let config = ClientConfig { mode, server_url };
    Ok(if config.is_empty() {
        None
    } else {
        Some(config)
    })
}

fn parse_server(value: Option<&Value>, path: &str) -> EngineResult<Option<ServerConfig>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let object = value
        .as_object()
        .ok_or_else(|| invalid_config(path, "server must be an object"))?;
    assert_known_fields(object, &["host", "port"], path, "server")?;
    let host = optional_non_empty_string(object.get("host"), path, "server.host")?;
    let port = match object.get("port") {
        None | Some(Value::Null) => None,
        Some(value) => Some(optional_positive_port(value, path)?),
    };
    let config = ServerConfig { host, port };
    Ok(if config.is_empty() {
        None
    } else {
        Some(config)
    })
}

fn optional_positive_port(value: &Value, path: &str) -> EngineResult<u16> {
    let port = value
        .as_u64()
        .filter(|port| *port > 0)
        .ok_or_else(|| invalid_config(path, "server.port must be a positive integer"))?;
    if port > u64::from(u16::MAX) {
        return Err(invalid_config(path, "server.port must not exceed 65535"));
    }
    Ok(port as u16)
}

fn optional_non_empty_string(
    value: Option<&Value>,
    path: &str,
    field: &str,
) -> EngineResult<Option<String>> {
    let Some(value) = value else {
        return Ok(None);
    };
    if matches!(value, Value::Null) {
        return Ok(None);
    }
    let Some(raw) = value.as_str() else {
        return Err(invalid_config(
            path,
            &format!("{field} must be a non-empty string"),
        ));
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(invalid_config(
            path,
            &format!("{field} must be a non-empty string"),
        ));
    }
    Ok(Some(trimmed.to_owned()))
}

fn optional_device(
    value: Option<&Value>,
    path: &str,
    field: &str,
) -> EngineResult<Option<EmbeddingDevice>> {
    let Some(value) = value else {
        return Ok(None);
    };
    if matches!(value, Value::Null) {
        return Ok(None);
    }
    let Some(raw) = value.as_str() else {
        return Err(invalid_config(
            path,
            &format!("{field} must be auto, cpu, metal, vulkan, or cuda"),
        ));
    };
    match raw {
        "auto" => Ok(Some(EmbeddingDevice::Auto)),
        "cpu" => Ok(Some(EmbeddingDevice::Cpu)),
        "metal" => Ok(Some(EmbeddingDevice::Metal)),
        "vulkan" => Ok(Some(EmbeddingDevice::Vulkan)),
        "cuda" => Ok(Some(EmbeddingDevice::Cuda)),
        _ => Err(invalid_config(
            path,
            &format!("{field} must be auto, cpu, metal, vulkan, or cuda"),
        )),
    }
}

fn assert_known_fields(
    object: &serde_json::Map<String, Value>,
    allowed: &[&str],
    path: &str,
    field: &str,
) -> EngineResult<()> {
    for key in object.keys() {
        if !allowed.contains(&key.as_str()) {
            return Err(invalid_config(
                path,
                &format!("{field}.{key} is not supported"),
            ));
        }
    }
    Ok(())
}

/// Mirrors `/^[a-z][a-z0-9_-]*$/` without pulling in the regex engine.
fn is_valid_provider_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first.is_ascii_lowercase() => (),
        _ => return false,
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

/// Mirrors `/^[a-z][a-z0-9_-]*\/[A-Za-z0-9._-]+$/`.
fn is_valid_model_reference(reference: &str) -> bool {
    let Some((provider, model)) = reference.split_once('/') else {
        return false;
    };
    if model.is_empty()
        || !model
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
    {
        return false;
    }
    is_valid_provider_name(provider)
}

fn invalid_config(path: &str, detail: &str) -> EngineError {
    EngineError::new(
        codes::config_invalid(),
        "zvec-grep global config is invalid",
    )
    .with_context(format!("path={path}\ndetail={detail}"))
}

/// Builds the `CONFIG.INVALID_EMBEDDING_RUNTIME` error used by runtime
/// resolution.
#[must_use]
pub fn invalid_runtime(reference: &str, message: &str) -> EngineError {
    EngineError::new(
        codes::config_invalid_embedding_runtime(),
        "Embedding runtime configuration is invalid",
    )
    .with_context(format!("reference={reference}\ndetail={message}"))
}
