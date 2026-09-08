//! `zg auth`: grant, inspect, and revoke remote-embedding workspace grants.

use std::path::{Path, PathBuf};

use zg_core::authorization::{
    RemoteEmbeddingAuthorizationManager, RemoteEmbeddingAuthorizationStore, RemoteEmbeddingScope,
    create_remote_embedding_target,
};
use zg_core::models::embeddings::{CreateEmbeddingModelOptions, DeviceKind};
use zg_core::models::factory::create_embedding_model;
use zg_core::service::facade::create_zvec_grep;

use super::catalog::{catalog_entry, catalog_identity, catalog_reference, catalog_reference_for};
use super::support::{absolute_path, service_options, single_root_or_cwd};
use crate::cli::{AuthAction, AuthArgs, AuthGrantArgs, AuthRootArgs};
use crate::error::CliError;

pub(crate) fn run_auth(args: AuthArgs) -> Result<(), CliError> {
    match &args.action {
        Some(AuthAction::Grant(grant)) => run_auth_grant(&args, grant),
        Some(AuthAction::Status(status)) => run_auth_status(&args, status),
        Some(AuthAction::Revoke(revoke)) => run_auth_revoke(&args, revoke),
        None => Err(CliError::usage("zg auth requires grant, status, or revoke")),
    }
}

fn auth_root(requested: &[PathBuf]) -> Result<String, CliError> {
    let start = single_root_or_cwd(requested, "zg auth grant accepts at most one root")?;
    Ok(absolute_path(&start)?.to_string_lossy().into_owned())
}

fn run_auth_status(_args: &AuthArgs, status: &AuthRootArgs) -> Result<(), CliError> {
    let root = auth_root(&status.roots)?;
    let store = RemoteEmbeddingAuthorizationStore::new();
    let status = store.status(&root)?;
    print_auth_status(&root, &status);
    Ok(())
}

fn run_auth_revoke(_args: &AuthArgs, revoke: &AuthRootArgs) -> Result<(), CliError> {
    let root = auth_root(&revoke.roots)?;
    let store = RemoteEmbeddingAuthorizationStore::new();
    let revoked = store.revoke_all(&root)?;
    if revoked > 0 {
        println!("Revoked {revoked} Remote Embedding Workspace grant(s).");
    } else {
        println!("No Remote Embedding Workspace grants found.");
    }
    Ok(())
}

fn run_auth_grant(args: &AuthArgs, grant: &AuthGrantArgs) -> Result<(), CliError> {
    let root = auth_root(&grant.roots)?;
    let service = create_zvec_grep(service_options(
        Some(PathBuf::from(&root)),
        args.embedding.clone(),
        args.api_key.clone(),
        args.endpoint.clone(),
        args.model_cache.clone(),
        args.device,
    ));
    let info = service.workspace_info(Some(Path::new(&root)))?;
    let configured = args.embedding.clone().or_else(|| {
        info.workspace_index
            .as_ref()
            .and_then(|index| index.embedding.clone())
            .flatten()
            .and_then(|schema| catalog_reference_for(&schema.provider, &schema.model))
            .map(|reference| reference.as_str().to_owned())
    });
    let Some(configured) = configured else {
        return Err(CliError::usage(
            "No embedding model is available. Pass --embedding <remote/model> or build an index first.",
        ));
    };
    let reference = catalog_reference(&configured)?;
    let entry = catalog_entry(reference.as_str()).ok_or_else(|| {
        CliError::config_invalid(format!("Unknown embedding model \"{configured}\""))
    })?;
    let (_, provider, model, _) = catalog_identity(entry);
    // Local models need no grant; only the supported remote provider
    // can be granted here.
    match provider {
        "local" => {
            return Err(CliError::usage(
                "Local embedding models do not require authorization.",
            ));
        }
        "qwen" => {}
        _ => {
            return Err(CliError::usage(format!(
                "Unsupported remote embedding provider: {provider}"
            )));
        }
    }
    let model_info = create_embedding_model(
        &reference,
        &CreateEmbeddingModelOptions {
            api_key: args.api_key.clone(),
            endpoint: args.endpoint.clone(),
            model_cache_dir: args.model_cache.clone(),
            device: DeviceKind::Auto,
        },
    )?;
    let endpoint = model_info.info().endpoint.clone().ok_or_else(|| {
        CliError::usage(format!(
            "Embedding model {} did not provide a remote endpoint.",
            model_info.info().reference
        ))
    })?;
    let roots: Vec<String> = info
        .workspace_index
        .as_ref()
        .map(|index| {
            index
                .root_paths
                .iter()
                .map(|path| path.absolute_path.clone())
                .collect()
        })
        .unwrap_or_else(|| vec![root.clone()]);
    let target = create_remote_embedding_target(&roots, provider, model, &endpoint)?;
    let manager = RemoteEmbeddingAuthorizationManager::new();
    manager.grant(&target, RemoteEmbeddingScope::Workspace)?;
    let store = RemoteEmbeddingAuthorizationStore::new();
    print_auth_status(&root, &store.status(&root)?);
    Ok(())
}

fn print_auth_status(root: &str, status: &zg_core::authorization::AuthorizationStatus) {
    println!("root: {root}");
    println!("grants: {}", status.grants.len());
    for grant in &status.grants {
        println!(
            "  {} {}/{} scope={:?} valid={}",
            grant.id, grant.provider, grant.model, grant.scope, grant.valid
        );
    }
}
