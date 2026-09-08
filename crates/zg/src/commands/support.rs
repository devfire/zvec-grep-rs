//! Shared wiring for command handlers: engine options, CLI mappers,
//! path resolution, daemon probing, and cooperative cancellation.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use zg_core::config::EmbeddingDevice;
use zg_core::models::catalog::ModelReference;
use zg_core::service::facade::CreateZvecGrepOptions;
use zg_core::types::{CodeSymbolType, UnixMillis};

use crate::cli::{DeviceArg, SymbolType, parse_modified_time};
use crate::client::DaemonClient;
use crate::error::CliError;

/// Mirrors `createServiceOptions` for the direct engine path.
pub(crate) fn service_options(
    root: Option<PathBuf>,
    embedding: Option<String>,
    api_key: Option<String>,
    endpoint: Option<String>,
    model_cache: Option<PathBuf>,
    device: Option<DeviceArg>,
) -> CreateZvecGrepOptions {
    let _ = device;
    CreateZvecGrepOptions {
        root,
        embedding: embedding.map(ModelReference::new),
        embedding_model: None,
        api_key,
        endpoint,
        model_cache_dir: model_cache,
    }
}

/// Maps a CLI device flag onto the model options.
pub(crate) fn map_device(device: DeviceArg) -> EmbeddingDevice {
    match device {
        DeviceArg::Auto => EmbeddingDevice::Auto,
        DeviceArg::Cpu => EmbeddingDevice::Cpu,
        DeviceArg::Metal => EmbeddingDevice::Metal,
        DeviceArg::Vulkan => EmbeddingDevice::Vulkan,
        DeviceArg::Cuda => EmbeddingDevice::Cuda,
    }
}

/// Maps CLI symbol types onto indexed symbol types.
pub(crate) fn map_symbol_types(types: &[SymbolType]) -> Vec<CodeSymbolType> {
    types
        .iter()
        .map(|symbol| match symbol {
            SymbolType::Module => CodeSymbolType::Module,
            SymbolType::Class => CodeSymbolType::Class,
            SymbolType::Interface => CodeSymbolType::Interface,
            SymbolType::Function => CodeSymbolType::Function,
            SymbolType::Value => CodeSymbolType::Value,
            SymbolType::Alias => CodeSymbolType::Alias,
        })
        .collect()
}

pub(crate) fn map_modified_time(
    value: Option<&str>,
    option: &str,
) -> Result<Option<UnixMillis>, CliError> {
    value
        .map(|value| parse_modified_time(value, option).map(UnixMillis::from_millis))
        .transpose()
}

/// `false` flags become `None` (unset), mirroring optional TS booleans.
pub(crate) fn bool_flag(set: bool) -> Option<bool> {
    set.then_some(true)
}

/// Exactly one root (or none, meaning cwd): slice-pattern matching keeps
/// the empty/single/ambiguous cases exhaustive without indexing.
pub(crate) fn single_root_or_cwd(
    roots: &[PathBuf],
    usage: &'static str,
) -> Result<PathBuf, CliError> {
    match roots.split_first() {
        None => Ok(PathBuf::from(".")),
        Some((only, [])) => Ok(only.clone()),
        Some(_) => Err(CliError::usage(usage)),
    }
}

pub(crate) fn absolute_cwd() -> Result<String, CliError> {
    Ok(absolute_path(PathBuf::from("."))?
        .to_string_lossy()
        .into_owned())
}

pub(crate) fn absolute_path(path: impl AsRef<Path>) -> Result<PathBuf, CliError> {
    let path = path.as_ref();
    if path.as_os_str() == "." {
        return std::env::current_dir().map_err(|error| CliError::io(Path::new("."), error));
    }
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()
            .map_err(|error| CliError::io(Path::new("."), error))?
            .join(path)
    };
    Ok(absolute)
}

/// Probes the daemon for `auto` mode routing.
pub(crate) async fn server_available(home: Option<&Path>) -> bool {
    match DaemonClient::from_env(None, home) {
        Ok(client) => client.server_available().await,
        Err(_) => false,
    }
}

/// Cooperative cancellation: flips on Ctrl-C, polled by blocking runs.
pub(crate) struct CancelLatch {
    flag: Arc<AtomicBool>,
}

impl CancelLatch {
    pub(crate) fn check(&self) -> zg_core::service::types::AbortCheck {
        let flag = Arc::clone(&self.flag);
        Arc::new(move || flag.load(Ordering::Relaxed))
    }
}

pub(crate) fn cancel_flag() -> CancelLatch {
    let latch = CancelLatch {
        flag: Arc::new(AtomicBool::new(false)),
    };
    let flag = Arc::clone(&latch.flag);
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            flag.store(true, Ordering::Relaxed);
        }
    });
    latch
}
