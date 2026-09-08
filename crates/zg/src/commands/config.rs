//! `zg config`: global provider credentials and model defaults.

use std::collections::BTreeMap;

use zg_core::config::{EmbeddingModelConfig, GlobalConfigUpdate, GlobalDefaults, ProviderConfig};

use super::catalog::{catalog_entry, catalog_identity, catalog_reference};
use super::support::map_device;
use crate::error::CliError;

pub(crate) fn run_config(args: crate::cli::ConfigArgs) -> Result<(), CliError> {
    match &args.target {
        Some(crate::cli::ConfigTarget::Model(cmd)) => match &cmd.op {
            Some(crate::cli::ConfigModelOp::Set(set)) => run_config_model_set(set),
            None => Err(CliError::usage(
                "zg config requires provider set or model set",
            )),
        },
        Some(crate::cli::ConfigTarget::Provider(cmd)) => match &cmd.op {
            Some(crate::cli::ConfigProviderOp::Set(set)) => run_config_provider_set(set),
            None => Err(CliError::usage(
                "zg config requires provider set or model set",
            )),
        },
        None => Err(CliError::usage(
            "zg config requires provider set or model set",
        )),
    }
}

fn run_config_model_set(set: &crate::cli::ConfigModelSetArgs) -> Result<(), CliError> {
    // Slice pattern instead of `len() != 1` plus `reference[0]`: the
    // single-reference case is the only one that compiles a binding.
    let [reference] = set.reference.as_slice() else {
        return Err(CliError::usage(
            "zg config model set requires exactly one reference",
        ));
    };
    let reference: &str = reference;
    if set.endpoint.is_none() && set.device.is_none() && !set.default {
        return Err(CliError::usage(
            "zg config model set requires --endpoint, --device, or --default",
        ));
    }
    let entry = catalog_reference(reference)?;
    let catalog = catalog_entry(entry.as_str()).ok_or_else(|| {
        CliError::config_invalid(format!("Unknown embedding model \"{reference}\""))
    })?;
    let (_, provider, _, _) = catalog_identity(catalog);
    if provider == "local" && set.endpoint.is_some() {
        return Err(CliError::usage(
            "--endpoint is only supported for remote embedding models",
        ));
    }
    if provider != "local" && set.device.is_some() {
        return Err(CliError::usage(
            "--device is only supported for local embedding models",
        ));
    }
    if let Some(endpoint) = &set.endpoint {
        assert_http_endpoint(endpoint)?;
    }
    let path = zg_core::config::global_config_path();
    let models = (set.endpoint.is_some() || set.device.is_some()).then(|| {
        BTreeMap::from([(
            reference.to_owned(),
            EmbeddingModelConfig {
                endpoint: set.endpoint.clone(),
                device: set.device.map(map_device),
            },
        )])
    });
    let defaults = set.default.then(|| GlobalDefaults {
        embedding: Some(reference.to_owned()),
        model_cache_dir: None,
    });
    zg_core::config::update_global_config(
        &path,
        GlobalConfigUpdate {
            defaults,
            providers: None,
            models,
            client: None,
            server: None,
        },
    )?;
    println!("Model config: {reference}");
    println!("Global config: {}", path.display());
    Ok(())
}

fn run_config_provider_set(set: &crate::cli::ConfigProviderSetArgs) -> Result<(), CliError> {
    let [reference] = set.reference.as_slice() else {
        return Err(CliError::usage(
            "zg config provider set requires exactly one reference",
        ));
    };
    let reference: &str = reference;
    if !reference.chars().all(|marker| {
        marker.is_ascii_lowercase() || marker.is_ascii_digit() || marker == '_' || marker == '-'
    }) || !reference
        .chars()
        .next()
        .is_some_and(|marker| marker.is_ascii_lowercase())
    {
        return Err(CliError::config_invalid(format!(
            "Invalid embedding provider \"{reference}\""
        )));
    }
    if reference == "local"
        || !zg_core::models::catalog::list_embedding_models()
            .iter()
            .any(|entry| catalog_identity(entry).1 == reference)
    {
        return Err(CliError::config_invalid(format!(
            "Unsupported remote embedding provider: {reference}"
        )));
    }
    let Some(api_key) = set.api_key.clone() else {
        return Err(CliError::usage("zg config provider set requires --api-key"));
    };
    let path = zg_core::config::global_config_path();
    zg_core::config::update_global_config(
        &path,
        GlobalConfigUpdate {
            defaults: None,
            providers: Some(BTreeMap::from([(
                reference.to_owned(),
                ProviderConfig {
                    api_key: Some(api_key),
                },
            )])),
            models: None,
            client: None,
            server: None,
        },
    )?;
    println!("Provider config: {reference}");
    println!("Global config: {}", path.display());
    Ok(())
}

fn assert_http_endpoint(endpoint: &str) -> Result<(), CliError> {
    let valid = endpoint.starts_with("http://") || endpoint.starts_with("https://");
    if valid && endpoint.len() > "https://".len() {
        return Ok(());
    }
    Err(CliError::usage("--endpoint must be a valid HTTP(S) URL"))
}
